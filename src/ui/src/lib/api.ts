// Typed client for the smalog HTTP API. In dev, Vite proxies `/api` to the
// running service; override with VITE_API_BASE for a remote instance.
const BASE = import.meta.env.VITE_API_BASE ?? "";

export interface InverterStatus {
  serial: number;
  name: string;
  totalPac: number;
  eToday: number;
  eTotal: number;
  status: string;
}

export interface Status {
  version: string;
  lastPoll: number | null;
  lastError: string | null;
  isLight: boolean;
  inverters: InverterStatus[];
}

export type Range = "day" | "week" | "month" | "year";

export interface Inverter {
  serial: number;
  name: string;
}

export type HistoryMetric = "power" | "energy" | "temperature";

export interface HistorySeries {
  key: string;
  label: string;
  metric: HistoryMetric;
  aggregate?: boolean;
}

export interface DaySummary {
  totalEnergy: number;
  peakPower: number;
  averagePower: number;
  recordedIntervals: number;
  previousDayEnergy: number;
  changePercent: number | null;
}

export interface MonthSummary {
  total: number;
  averageDaily: number;
  recordedDays: number;
  bestDay: { label: string; value: number } | null;
  previousMonthTotal: number;
  changePercent: number | null;
}

export interface WeekSummary {
  total: number;
  averageDaily: number;
  recordedDays: number;
  bestDay: { label: string; value: number } | null;
  previousWeekTotal: number;
  changePercent: number | null;
}

export interface YearSummary {
  total: number;
  averageMonthly: number;
  recordedMonths: number;
  bestMonth: { label: string; value: number } | null;
  previousYearTotal: number;
  changePercent: number | null;
}

/** A labelled, multi-series dataset. `rows` are chart-ready: each has a
 *  `label` plus one numeric field per entry in `keys`. */
export interface History {
  range: Range;
  date?: string;
  today?: string;
  live?: boolean;
  weekStart?: string;
  weekEnd?: string;
  currentWeekStart?: string;
  month?: string;
  currentMonth?: string;
  year?: string;
  currentYear?: string;
  unit: string;
  keys: string[];
  series?: HistorySeries[];
  summary?: DaySummary | WeekSummary | MonthSummary | YearSummary;
  rows: Array<Record<string, string | number>>;
}

export interface DiagnosticMppt {
  tracker_number: number;
  dc_power_w: number | null;
  dc_current_ma: number | null;
  dc_voltage_mv: number | null;
}

interface DiagnosticMeasurement {
  timestamp: number;
  label: string;
  mppts: DiagnosticMppt[];
  acPower: number;
  acVoltage: number;
  acCurrent: number;
  frequency: number;
  efficiency: number | null;
  signal: number | null;
  status: string;
}

export type CurrentDiagnosticMeasurement = DiagnosticMeasurement;
export type HistoricalDiagnosticMeasurement = DiagnosticMeasurement;

export interface InverterDiagnostics {
  serial: number;
  name: string;
  model: string;
  firmware: string;
  status: string;
  totalEnergy: number;
  operatingTime: number;
  feedInTime: number;
  averageEfficiency: number | null;
  latestMeasurement: CurrentDiagnosticMeasurement | null;
  rows: HistoricalDiagnosticMeasurement[];
}

export interface DiagnosticEvent {
  timestamp: number;
  serial: number;
  code: number;
  type: string;
  category: string;
  group: string;
  message: string;
  oldValue: string;
  newValue: string;
}

export interface Diagnostics {
  date: string;
  inverters: InverterDiagnostics[];
  events: DiagnosticEvent[];
}

async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(`${BASE}${path}`);
  if (!response.ok) {
    throw new Error(`${path}: HTTP ${response.status}`);
  }
  return response.json() as Promise<T>;
}

export const fetchStatus = () => getJson<Status>("/api/status");

export const fetchInverters = () => getJson<Inverter[]>("/api/inverters");

/** History for a range. `serial` filters to one inverter; null returns
 *  aggregate and separate per-inverter day series, plus aggregate coarser ranges.
 *  `strings` splits the day view into per-string DC power. */
export function fetchHistory(
  range: Range,
  serial: number | null,
  strings: boolean,
  period?: string | null,
): Promise<History> {
  const params = new URLSearchParams({ range });
  if (serial != null) params.set("serial", String(serial));
  if (strings) params.set("strings", "true");
  if (range === "day" && period) params.set("date", period);
  if (range === "week" && period) params.set("week", period);
  if (range === "month" && period) params.set("month", period);
  if (range === "year" && period) params.set("year", period);
  return getJson<History>(`/api/history?${params.toString()}`);
}

export function fetchDiagnostics(
  date: string | undefined,
  serial: number | null,
): Promise<Diagnostics> {
  const params = new URLSearchParams();
  if (date) params.set("date", date);
  if (serial != null) params.set("serial", String(serial));
  return getJson<Diagnostics>(`/api/diagnostics?${params.toString()}`);
}
