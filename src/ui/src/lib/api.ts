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
  // A metric an inverter does not report comes back as null, which is not
  // the same as zero.
  rows: Array<Record<string, string | number | null>>;
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

export type TransmissionOutcome = "ok" | "empty" | "unsupported" | "failed";

export interface TransmissionDevice {
  serial: number;
  frames: number;
  addressed: boolean;
}

export interface Transmission {
  sequence: number;
  occurredAt: number;
  target: string;
  transport: string;
  protocol: string;
  requestKind: string;
  command: number | null;
  firstLri: number | null;
  lastLri: number | null;
  durationMs: number;
  totalFrames: number;
  outcome: TransmissionOutcome;
  error: string | null;
  detail: string | null;
  devices: TransmissionDevice[];
}

export type LogLevel = "error" | "warn" | "info" | "debug" | "trace";

export interface LogRecord {
  sequence: number;
  occurredAt: number;
  level: LogLevel;
  target: string;
  message: string;
  fields: string | null;
}

/** What a diagnostics ring currently holds. `oldestOccurredAt` next to
 *  `retentionHours` is what tells a 48-hour window apart from one the entry
 *  cap cut short. */
export interface RingEnvelope {
  cursor: number | null;
  retentionHours: number;
  maxEntries: number;
  retained: number;
  oldestOccurredAt: number | null;
  dropped: number;
  /** Log ring only: the cursor sent was ahead of the ring, so the page
   *  restarted from the newest record. The log ring lives in the service's
   *  memory and its cursors restart with the process. */
  reset?: boolean;
}

export interface TransmissionsResponse {
  entries: Transmission[];
  envelope: RingEnvelope;
}

export interface LogsResponse {
  entries: LogRecord[];
  envelope: RingEnvelope;
}

/** Keyset paging: `since` follows the live tail, `before` walks backwards
 *  through the retained window. Never both at once. */
export interface RingQuery {
  since?: number | null;
  before?: number | null;
  limit?: number;
}

function ringParams(query: RingQuery): URLSearchParams {
  const params = new URLSearchParams();
  if (query.since != null) params.set("since", String(query.since));
  if (query.before != null) params.set("before", String(query.before));
  if (query.limit != null) params.set("limit", String(query.limit));
  return params;
}

export function fetchTransmissions(
  query: RingQuery & {
    outcome?: TransmissionOutcome | null;
    target?: string | null;
    serial?: number | null;
  } = {},
): Promise<TransmissionsResponse> {
  const params = ringParams(query);
  if (query.outcome) params.set("outcome", query.outcome);
  if (query.target) params.set("target", query.target);
  if (query.serial != null) params.set("serial", String(query.serial));
  return getJson<TransmissionsResponse>(`/api/transmissions?${params.toString()}`);
}

export function fetchLogs(
  query: RingQuery & { level?: LogLevel | null; target?: string | null } = {},
): Promise<LogsResponse> {
  const params = ringParams(query);
  if (query.level) params.set("level", query.level);
  if (query.target) params.set("target", query.target);
  return getJson<LogsResponse>(`/api/logs?${params.toString()}`);
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
