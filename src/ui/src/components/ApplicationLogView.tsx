import { useMemo, useState } from "react";
import type { LogLevel, LogRecord } from "@/lib/api";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import {
  formatTimestamp,
  LoadOlder,
  PauseButton,
  RingStatus,
  type RingView,
} from "@/components/SystemView";

/** Most severe first, matching the endpoint's "this level or worse". */
const LEVELS: LogLevel[] = ["error", "warn", "info", "debug", "trace"];

const LEVEL_LABEL = {
  error: "levelError",
  warn: "levelWarn",
  info: "levelInfo",
  debug: "levelDebug",
  trace: "levelTrace",
} as const;

/** Warnings and errors have to stand out in a wall of info lines. */
const LEVEL_STYLE: Record<LogLevel, string> = {
  error: "border-destructive/40 bg-destructive/10 text-destructive",
  warn: "border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-400",
  info: "text-muted-foreground",
  debug: "text-muted-foreground",
  trace: "text-muted-foreground",
};

export function ApplicationLogView({
  view,
  level,
  onLevelChange,
  paused,
  onTogglePause,
}: {
  view: RingView<LogRecord>;
  level: LogLevel | "";
  onLevelChange: (value: LogLevel | "") => void;
  paused: boolean;
  onTogglePause: () => void;
}) {
  const { t, language } = useI18n();
  const [search, setSearch] = useState("");

  const rows = useMemo(() => {
    const needle = search.trim().toLowerCase();
    if (!needle) return view.state.entries;
    return view.state.entries.filter(
      (entry) =>
        entry.message.toLowerCase().includes(needle) ||
        entry.target.toLowerCase().includes(needle) ||
        (entry.fields?.toLowerCase().includes(needle) ?? false),
    );
  }, [view.state.entries, search]);

  const window = view.state.window;
  const disabled = window != null && window.retentionHours === 0;

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <label className="sr-only" htmlFor="log-level">
          {t("minimumLevel")}
        </label>
        <select
          id="log-level"
          className="min-h-9 rounded-md border bg-background px-2 py-1 text-sm"
          value={level}
          onChange={(event) => onLevelChange(event.target.value as LogLevel | "")}
        >
          <option value="">{t("allLevels")}</option>
          {LEVELS.map((value) => (
            <option key={value} value={value}>
              {t(LEVEL_LABEL[value])}
            </option>
          ))}
        </select>
        <label className="sr-only" htmlFor="log-search">
          {t("filterPlaceholder")}
        </label>
        <input
          id="log-search"
          type="search"
          className="min-h-9 w-full rounded-md border bg-background px-2 py-1 text-sm sm:w-56"
          placeholder={t("filterPlaceholder")}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
        <PauseButton paused={paused} onToggle={onTogglePause} />
      </div>

      <p className="text-xs text-muted-foreground">
        {t("logNotFilteredByInverter")} {t("logIsMemoryOnly")}
      </p>
      {window?.reset && (
        <p className="text-xs text-amber-600 dark:text-amber-500">
          {t("logRingReset")}
        </p>
      )}
      <RingStatus window={window} paused={paused} failed={view.failed} />

      {disabled ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {t("logCaptureDisabled")}
        </p>
      ) : rows.length === 0 ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {t("ringEmpty")}
        </p>
      ) : (
        <>
          <ul className="flex min-w-0 flex-col gap-1">
            {rows.map((entry) => (
              <li
                key={entry.sequence}
                className={cn(
                  "flex min-w-0 flex-col gap-1 rounded-md border border-transparent px-2 py-1.5 text-sm sm:flex-row sm:items-baseline sm:gap-3",
                  LEVEL_STYLE[entry.level],
                )}
              >
                <span className="shrink-0 whitespace-nowrap text-xs tabular-nums text-muted-foreground">
                  {formatTimestamp(entry.occurredAt, language)}
                </span>
                <Badge
                  variant={
                    entry.level === "error"
                      ? "destructive"
                      : entry.level === "warn"
                        ? "secondary"
                        : "outline"
                  }
                  className="shrink-0"
                >
                  {t(LEVEL_LABEL[entry.level])}
                </Badge>
                <span className="min-w-0 flex-1 break-words">
                  {entry.message}
                  {entry.fields && (
                    <span className="ml-2 font-mono text-xs text-muted-foreground">
                      {entry.fields}
                    </span>
                  )}
                </span>
                <span className="shrink-0 font-mono text-xs text-muted-foreground">
                  {entry.target}
                </span>
              </li>
            ))}
          </ul>
          <LoadOlder
            atOldest={view.state.atOldest}
            loading={view.loadingOlder}
            onLoad={view.loadOlder}
          />
        </>
      )}
    </div>
  );
}
