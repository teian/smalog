import { useMemo, useState } from "react";
import type { Transmission, TransmissionOutcome } from "@/lib/api";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  formatTimestamp,
  LoadOlder,
  PauseButton,
  RingStatus,
  type RingView,
} from "@/components/SystemView";

const OUTCOMES: TransmissionOutcome[] = ["ok", "empty", "failed"];

/** SMA command and LRI window, rendered the way SBFspot and the SMA
 *  documentation write them, so a value can be compared against either. */
function hex(value: number | null, digits: number): string {
  if (value == null) return "—";
  return `0x${value.toString(16).toUpperCase().padStart(digits, "0")}`;
}

export function TransmissionsTable({
  view,
  outcome,
  onOutcomeChange,
  paused,
  onTogglePause,
}: {
  view: RingView<Transmission>;
  outcome: TransmissionOutcome | "";
  onOutcomeChange: (value: TransmissionOutcome | "") => void;
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
        entry.target.toLowerCase().includes(needle) ||
        entry.requestKind.toLowerCase().includes(needle) ||
        (entry.error?.toLowerCase().includes(needle) ?? false),
    );
  }, [view.state.entries, search]);

  const window = view.state.window;
  const disabled = window != null && window.retentionHours === 0;

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <label className="sr-only" htmlFor="transmission-outcome">
          {t("colOutcome")}
        </label>
        <select
          id="transmission-outcome"
          className="min-h-9 rounded-md border bg-background px-2 py-1 text-sm"
          value={outcome}
          onChange={(event) =>
            onOutcomeChange(event.target.value as TransmissionOutcome | "")
          }
        >
          <option value="">{t("allOutcomes")}</option>
          {OUTCOMES.map((value) => (
            <option key={value} value={value}>
              {t(
                value === "ok"
                  ? "outcomeOk"
                  : value === "empty"
                    ? "outcomeEmpty"
                    : "outcomeFailed",
              )}
            </option>
          ))}
        </select>
        <label className="sr-only" htmlFor="transmission-search">
          {t("filterPlaceholder")}
        </label>
        <input
          id="transmission-search"
          type="search"
          className="min-h-9 w-full rounded-md border bg-background px-2 py-1 text-sm sm:w-56"
          placeholder={t("filterPlaceholder")}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
        <PauseButton paused={paused} onToggle={onTogglePause} />
      </div>

      <RingStatus window={window} paused={paused} failed={view.failed} />

      {disabled ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {t("transmissionsDisabled")}
        </p>
      ) : rows.length === 0 ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {t("ringEmpty")}
        </p>
      ) : (
        <>
          <div className="min-w-0 overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("colTime")}</TableHead>
                  <TableHead>{t("colTarget")}</TableHead>
                  <TableHead>{t("colTransport")}</TableHead>
                  <TableHead>{t("colRequest")}</TableHead>
                  <TableHead>{t("colCommand")}</TableHead>
                  <TableHead>{t("colRegisters")}</TableHead>
                  <TableHead className="text-right">{t("colDuration")}</TableHead>
                  <TableHead className="text-right">{t("colFrames")}</TableHead>
                  <TableHead>{t("colOutcome")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((entry) => (
                  <TableRow
                    key={entry.sequence}
                    className={cn(
                      entry.outcome === "failed" &&
                        "bg-destructive/10 hover:bg-destructive/15",
                    )}
                  >
                    <TableCell className="whitespace-nowrap tabular-nums">
                      {formatTimestamp(entry.occurredAt, language)}
                    </TableCell>
                    <TableCell className="whitespace-nowrap">
                      {entry.target}
                    </TableCell>
                    <TableCell>{entry.transport}</TableCell>
                    <TableCell className="whitespace-nowrap font-mono text-xs">
                      {entry.requestKind}
                    </TableCell>
                    <TableCell className="whitespace-nowrap font-mono text-xs">
                      {hex(entry.command, 8)}
                    </TableCell>
                    <TableCell className="whitespace-nowrap font-mono text-xs">
                      {entry.firstLri == null
                        ? "—"
                        : `${hex(entry.firstLri, 8)}–${hex(entry.lastLri, 8)}`}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {entry.durationMs} ms
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {entry.totalFrames}
                      {entry.devices.length > 0 && (
                        <span className="ml-1 text-xs text-muted-foreground">
                          (
                          {t("devicesAnswered", {
                            answered: entry.devices.filter(
                              (device) => device.frames > 0,
                            ).length,
                            addressed: entry.devices.length,
                          })}
                          )
                        </span>
                      )}
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-col gap-1">
                        <Badge
                          variant={
                            entry.outcome === "failed"
                              ? "destructive"
                              : entry.outcome === "empty"
                                ? "secondary"
                                : "outline"
                          }
                        >
                          {t(
                            entry.outcome === "ok"
                              ? "outcomeOk"
                              : entry.outcome === "empty"
                                ? "outcomeEmpty"
                                : "outcomeFailed",
                          )}
                        </Badge>
                        {(entry.error ?? entry.detail) && (
                          <span
                            className={cn(
                              "max-w-xs whitespace-normal break-words text-xs",
                              entry.error
                                ? "text-destructive"
                                : "text-muted-foreground",
                            )}
                          >
                            {entry.error ?? entry.detail}
                          </span>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
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
