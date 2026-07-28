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
