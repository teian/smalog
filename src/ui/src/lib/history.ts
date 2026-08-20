import type { HistorySeries } from "@/lib/api";

/** Select aggregate production series from current and legacy API payloads. */
export function aggregateHistorySeries(
  series: HistorySeries[],
): HistorySeries[] {
  const aggregate = series.filter(
    (item) =>
      item.aggregate === true ||
      item.key.startsWith("total_") ||
      /^total\b/i.test(item.label),
  );
  if (aggregate.length > 0) return aggregate;

  // Legacy aggregate day responses used the generic power/energy keys.
  return series.filter(
    (item) =>
      item.metric !== "temperature" &&
      (item.key === "power" || item.key === "energy"),
  );
}

/** One row value as a number, or null when the metric is absent.
 *
 *  A metric the inverter does not report arrives as `null`, and `Number(null)`
 *  is `0` — which is how a missing temperature came to be shown as 0.00 °C. */
export function numericValue(
  value: string | number | null | undefined,
): number | null {
  if (value === null || value === undefined) return null;
  // `Number("")` is 0 as well; an empty cell is not a reading either.
  if (typeof value === "string" && value.trim() === "") return null;
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : null;
}
