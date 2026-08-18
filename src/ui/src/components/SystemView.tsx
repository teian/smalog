import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Pause, Play } from "lucide-react";
import {
  fetchLogs,
  fetchTransmissions,
  type LogLevel,
  type LogRecord,
  type Transmission,
  type TransmissionOutcome,
} from "@/lib/api";
import { useI18n } from "@/lib/i18n";
import {
  appendOlder,
  emptyRingState,
  mergeNewer,
  newestCursor,
  oldestCursor,
  replaceAll,
  windowStatus,
  type RingEntry,
  type RingPage,
  type RingState,
} from "@/lib/systemLog";
import { Button } from "@/components/ui/button";
import { ApplicationLogView } from "@/components/ApplicationLogView";
import { TransmissionsTable } from "@/components/TransmissionsTable";

export type SystemTab = "transmissions" | "log";

/** Entries held in the browser. The retained window is up to two days, so it
 *  is paged rather than loaded. */
const KEEP_ENTRIES = 500;
const PAGE_SIZE = 100;
const REFRESH_MS = 5_000;

interface RingView<T extends RingEntry> {
  state: RingState<T>;
  failed: boolean;
  loadingOlder: boolean;
  loadOlder: () => void;
}

/** Follows the live tail of one ring and pages backwards on demand.
 *
 *  Only the visible tab runs this: `active` gates the interval, so the tab in
 *  the background costs nothing.
 */
function useRing<T extends RingEntry>(
  load: (query: { since?: number | null; before?: number | null; limit: number }) => Promise<
    RingPage<T>
  >,
  active: boolean,
  paused: boolean,
  filterKey: string,
): RingView<T> {
  const [state, setState] = useState<RingState<T>>(emptyRingState<T>);
  const [failed, setFailed] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  // The interval must not be re-created on every state change, so the loader
  // reads the current cursor through a ref instead of closing over it.
  const cursorRef = useRef<number | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  cursorRef.current = newestCursor(state);

  useEffect(() => {
    setState(emptyRingState<T>());
    cursorRef.current = null;
  }, [filterKey]);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;

    const poll = async () => {
      try {
        const page = await loadRef.current({
          since: cursorRef.current,
          limit: PAGE_SIZE,
        });
        if (cancelled) return;
        setFailed(false);
        setState((current) =>
          current.entries.length === 0
            ? replaceAll(page)
            : mergeNewer(current, page, KEEP_ENTRIES),
        );
      } catch {
        if (!cancelled) setFailed(true);
      }
    };

    void poll();
    if (paused) return () => void (cancelled = true);
    const timer = setInterval(() => void poll(), REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [active, paused, filterKey]);

  const loadOlder = useCallback(() => {
    setLoadingOlder(true);
    void (async () => {
      try {
        const page = await loadRef.current({
          before: oldestCursor(state),
          limit: PAGE_SIZE,
        });
        setFailed(false);
        setState((current) => appendOlder(current, page));
      } catch {
        setFailed(true);
      } finally {
        setLoadingOlder(false);
      }
    })();
  }, [state]);

  return { state, failed, loadingOlder, loadOlder };
}

/** Time of an entry, in the browser's own timezone. */
export function formatTimestamp(ms: number, language: string): string {
  return new Date(ms).toLocaleString(language, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** Shared header: what window is shown, whether anything was dropped, and
 *  whether refreshing is paused or failing. */
export function RingStatus({
  window: ringWindow,
  paused,
  failed,
}: {
  window: RingState<RingEntry>["window"];
  paused: boolean;
  failed: boolean;
}) {
  const { t } = useI18n();
  const status = useMemo(
    () => windowStatus(ringWindow, Date.now()),
    [ringWindow],
  );

  return (
    <div className="flex flex-col gap-1 text-xs text-muted-foreground">
      {status.kind === "full" && (
        <span>{t("windowFull", { hours: status.retentionHours })}</span>
      )}
      {status.kind === "shortened" && (
        <span className="text-amber-600 dark:text-amber-500">
          {t("windowShortened", {
            actual: Math.max(1, Math.round(status.actualHours)),
            hours: status.retentionHours,
            cap: ringWindow?.maxEntries ?? 0,
          })}
        </span>
      )}
      {(ringWindow?.dropped ?? 0) > 0 && (
        <span className="text-amber-600 dark:text-amber-500">
          {t("droppedEntries", { count: ringWindow?.dropped ?? 0 })}
        </span>
      )}
      {failed && (
        <span className="text-destructive">{t("refreshFailed")}</span>
      )}
      {paused && !failed && <span>{t("refreshPaused")}</span>}
    </div>
  );
}

export function PauseButton({
  paused,
  onToggle,
}: {
  paused: boolean;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      onClick={onToggle}
      aria-pressed={paused}
    >
      {paused ? (
        <Play data-icon="inline-start" aria-hidden="true" />
      ) : (
        <Pause data-icon="inline-start" aria-hidden="true" />
      )}
      <span>{paused ? t("resumeRefresh") : t("pauseRefresh")}</span>
    </Button>
  );
}

export function LoadOlder({
  atOldest,
  loading,
  onLoad,
}: {
  atOldest: boolean;
  loading: boolean;
  onLoad: () => void;
}) {
  const { t } = useI18n();
  if (atOldest) {
    return (
      <p className="py-2 text-center text-xs text-muted-foreground">
        {t("atOldestEntry")}
      </p>
    );
  }
  return (
    <div className="flex justify-center py-2">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        onClick={onLoad}
        disabled={loading}
      >
        {loading ? t("loadingOlder") : t("loadOlder")}
      </Button>
    </div>
  );
}

export function SystemView({
  tab,
  serial,
}: {
  tab: SystemTab;
  serial: number | null;
}) {
  const [paused, setPaused] = useState(false);
  const [outcome, setOutcome] = useState<TransmissionOutcome | "">("");
  const [level, setLevel] = useState<LogLevel | "">("");

  const loadTransmissions = useCallback(
    (query: { since?: number | null; before?: number | null; limit: number }) =>
      fetchTransmissions({
        ...query,
        outcome: outcome || null,
        serial,
      }),
    [outcome, serial],
  );
  const loadLogs = useCallback(
    (query: { since?: number | null; before?: number | null; limit: number }) =>
      fetchLogs({ ...query, level: level || null }),
    [level],
  );

  const transmissions = useRing<Transmission>(
    loadTransmissions,
    tab === "transmissions",
    paused,
    `${outcome}|${serial ?? "all"}`,
  );
  const logs = useRing<LogRecord>(loadLogs, tab === "log", paused, level);

  const togglePause = useCallback(() => setPaused((value) => !value), []);

  if (tab === "transmissions") {
    return (
      <TransmissionsTable
        view={transmissions}
        outcome={outcome}
        onOutcomeChange={setOutcome}
        paused={paused}
        onTogglePause={togglePause}
      />
    );
  }
  return (
    <ApplicationLogView
      view={logs}
      level={level}
      onLevelChange={setLevel}
      paused={paused}
      onTogglePause={togglePause}
    />
  );
}

export type { RingView };
