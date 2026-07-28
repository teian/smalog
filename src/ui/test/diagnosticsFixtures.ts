import type {
  DiagnosticMppt,
  Diagnostics,
  HistoricalDiagnosticMeasurement,
  InverterDiagnostics,
} from "../src/lib/api.ts";

export function mppt(
  tracker_number: number,
  dc_power_w: number | null = tracker_number * 100,
  dc_current_ma: number | null = tracker_number * 1_000,
  dc_voltage_mv: number | null = tracker_number * 10_000,
): DiagnosticMppt {
  return {
    tracker_number,
    dc_power_w,
    dc_current_ma,
    dc_voltage_mv,
  };
}

export function diagnosticMeasurement(
  timestamp: number,
  mppts: DiagnosticMppt[],
): HistoricalDiagnosticMeasurement {
  return {
    timestamp,
    label: new Date(timestamp * 1_000).toISOString().slice(11, 16),
    mppts,
    acPower: 1_000,
    acVoltage: 230,
    acCurrent: 4.35,
    frequency: 50,
    efficiency: 95,
    signal: null,
    status: "OK",
  };
}

export function inverterDiagnostics(
  trackerSets: DiagnosticMppt[][],
): InverterDiagnostics {
  const rows = trackerSets.map((trackers, index) =>
    diagnosticMeasurement(1_700_000_000 + index * 300, trackers),
  );

  return {
    serial: 42,
    name: "Test inverter",
    model: "Sunny Test",
    firmware: "1.0",
    status: "OK",
    totalEnergy: 12_345,
    operatingTime: 1_000,
    feedInTime: 900,
    averageEfficiency: 95,
    latestMeasurement: rows.at(-1) ?? diagnosticMeasurement(1_700_000_000, []),
    rows,
  };
}

export function diagnosticsResponse(
  trackerSets: DiagnosticMppt[][],
): Diagnostics {
  return {
    date: "2026-07-27",
    inverters: [inverterDiagnostics(trackerSets)],
    events: [],
  };
}
