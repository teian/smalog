import type {
  DiagnosticMppt,
  HistoricalDiagnosticMeasurement,
} from "./api.ts";

export function orderedMppts(mppts: DiagnosticMppt[]): DiagnosticMppt[] {
  return [...mppts].sort(
    (left, right) => left.tracker_number - right.tracker_number,
  );
}

export function buildMpptChartData(
  measurements: HistoricalDiagnosticMeasurement[],
): {
  trackers: number[];
  rows: Array<Record<string, string | number | null>>;
} {
  const trackers = [
    ...new Set(
      measurements.flatMap((measurement) =>
        measurement.mppts.map((mppt) => mppt.tracker_number),
      ),
    ),
  ].sort((left, right) => left - right);

  return {
    trackers,
    rows: measurements.map((measurement) => {
      const row: Record<string, string | number | null> = {
        label: measurement.label,
      };
      for (const mppt of measurement.mppts) {
        row[mpptSeriesKey(mppt.tracker_number)] = mppt.dc_power_w;
      }
      return row;
    }),
  };
}

export function mpptSeriesKey(tracker: number): string {
  return `mppt_${tracker}_dc_power_w`;
}
