import {
  createContext,
  type ReactNode,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";

export type Language = "en" | "de";

const STORAGE_KEY = "smalog-language";

const en = {
  language: "Language",
  english: "English",
  german: "German",
  offline: "offline",
  connectionFailed: "Connection failed",
  connectionError: "Inverter connection error",
  showErrorDetails: "Show connection error details",
  pollContinues:
    "The last polling cycle failed. The service remains active and will retry automatically.",
  connecting: "Connecting…",
  noInverterData: "No inverter data yet.",
  currentPower: "Current power",
  yieldToday: "Yield today",
  today: "today",
  production: "Production",
  energyData: "Energy data",
  events: "Events",
  deviceStatistics: "Device statistics",
  gridQuality: "Grid quality",
  dataNavigation: "Data navigation",
  dataAreas: "Data areas",
  system: "System",
  transmissions: "Transmissions",
  applicationLog: "Application log",
  systemTabs: "System views",
  colTime: "Time",
  colTarget: "Target",
  colTransport: "Transport",
  colRequest: "Request",
  colCommand: "Command",
  colRegisters: "Registers",
  colDuration: "Duration",
  colFrames: "Frames",
  colOutcome: "Outcome",
  colLevel: "Level",
  colMessage: "Message",
  colSource: "Source",
  outcomeOk: "ok",
  outcomeEmpty: "no answer",
  outcomeUnsupported: "not available",
  outcomeFailed: "failed",
  levelError: "error",
  levelWarn: "warning",
  levelInfo: "info",
  levelDebug: "debug",
  levelTrace: "trace",
  allOutcomes: "All outcomes",
  allLevels: "All levels",
  minimumLevel: "Minimum level",
  filterPlaceholder: "Filter…",
  pauseRefresh: "Pause",
  resumeRefresh: "Resume",
  refreshPaused: "Refresh paused",
  refreshFailed: "Could not refresh — showing the last data loaded.",
  loadOlder: "Load older entries",
  loadingOlder: "Loading…",
  atOldestEntry: "Oldest retained entry reached.",
  windowFull: "Showing the last {hours} h, as configured.",
  windowShortened:
    "Showing the last {actual} h of the configured {hours} h — the row cap of {cap} was reached first.",
  ringEmpty: "Nothing recorded yet.",
  transmissionsDisabled:
    "Transmission recording is switched off (service.transmission_log_retention_hours = 0).",
  logCaptureDisabled:
    "Log capture is switched off (service.application_log_retention_hours = 0).",
  droppedEntries:
    "{count} entries were dropped because the writer fell behind — a gap here is a drop, not a quiet period.",
  logNotFilteredByInverter:
    "Log records are not attributed to one inverter, so the inverter filter does not apply here.",
  logIsMemoryOnly:
    "Kept in the service's memory and lost on restart — the journal or container log is the durable copy.",
  logRingReset: "The service restarted; this list begins again at its newest record.",
  noDevicesAddressed: "no devices",
  devicesAnswered: "{answered} of {addressed} answered",
  allInverters: "All inverters",
  day: "Day",
  week: "Week",
  month: "Month",
  year: "Year",
  live: "Live",
  loading: "Loading…",
  loadError: "Could not load data: {error}",
  noProductionData: "No production data for this {range}.",
  powerAndEnergy: "Power & Energy",
  power: "Power",
  totalPower: "Total Power",
  energyGenerated: "Energy Generated",
  totalEnergy: "Total Energy",
  temperature: "Temperature",
  visibleChartSeries: "Visible chart series",
  hideSeries: "Hide {label}",
  showSeries: "Show {label}",
  todaysEnergy: "Today's energy",
  energyThisDay: "Energy this day",
  peakPower: "Peak power",
  averagePower: "Average power",
  recordedIntervals: "{count} recorded intervals",
  vsYesterday: "Vs. yesterday",
  vsPreviousDay: "Vs. previous day",
  previous: "Previous: {value}",
  noPreviousDayData: "No previous-day data",
  dayTotals: "Day totals",
  dayTotalsCaption: "Daily production summary for the selected inverter view.",
  inverter: "Inverter",
  lastTemperature: "Last temperature",
  fiveMinuteValues: "5-minute values",
  fiveMinuteCaption: "Five-minute production values, newest first.",
  time: "Time",
  total: "Total",
  energyThisWeek: "Energy this week",
  energyThisMonth: "Energy this month",
  energyThisYear: "Energy this year",
  averageDaily: "Average daily",
  averageReference: "Average: {value}",
  averageMonthly: "Average monthly",
  recordedDays: "{count} recorded days",
  recordedMonths: "{count} recorded months",
  bestDay: "Best day",
  bestMonth: "Best month",
  vsLastWeek: "Vs. last week",
  vsLastMonth: "Vs. last month",
  vsLastYear: "Vs. last year",
  noPreviousWeekData: "No previous-week data",
  noPreviousMonthData: "No previous-month data",
  noPreviousYearData: "No previous-year data",
  dailyEnergyThisWeek: "Daily energy this week",
  dailyEnergyThisMonth: "Daily energy this month",
  monthlyEnergyThisYear: "Monthly energy this year",
  dailyValues: "Daily values",
  monthlyValues: "Monthly values",
  generated: "Generated",
  shareOfWeek: "Share of week",
  shareOfMonth: "Share of month",
  shareOfYear: "Share of year",
  dailyWeekCaption: "Daily energy values for the selected week, newest first.",
  dailyMonthCaption: "Daily energy values for the current month, newest first.",
  monthlyYearCaption:
    "Monthly energy values for the selected year, newest first.",
  dayNavigation: "Day navigation",
  weekNavigation: "Week navigation",
  monthNavigation: "Month navigation",
  yearNavigation: "Year navigation",
  previousDay: "Previous day",
  nextDay: "Next day",
  previousWeek: "Previous week",
  nextWeek: "Next week",
  previousMonth: "Previous month",
  nextMonth: "Next month",
  previousYear: "Previous year",
  nextYear: "Next year",
  backToToday: "Back to today",
  backToCurrentWeek: "Back to current week",
  backToCurrentMonth: "Back to current month",
  backToCurrentYear: "Back to current year",
  diagnostics: "Diagnostics",
  noDiagnosticData: "No diagnostic data for this day.",
  latestMeasurementFallback:
    "No diagnostic data for this day. Showing the latest measurement from {time}.",
  lifetimeEnergy: "Lifetime energy",
  operatingTime: "Operating time",
  feedInTime: "Feed-in time",
  averageEfficiency: "Average efficiency",
  signalStrength: "Signal strength",
  deviceDetails: "Device details",
  model: "Model",
  firmware: "Firmware",
  status: "Status",
  mpptPerformance: "MPPT / string performance",
  mpptPower: "MPPT power",
  noMpptData: "No MPPT trackers observed.",
  tracker: "Tracker {number}",
  dcPower: "DC power",
  dcVoltage: "DC voltage",
  dcCurrent: "DC current",
  electricalConditions: "Electrical conditions",
  acVoltage: "AC voltage",
  acCurrent: "AC current",
  gridFrequency: "Grid frequency",
  efficiency: "Efficiency",
  recentEvents: "Recent warnings and faults",
  noEvents: "No warnings or faults recorded.",
  eventTime: "Time",
  device: "Device",
  category: "Category",
  event: "Event",
  code: "Code",
  trackerColumn: "Tracker",
  ok: "OK",
  warning: "Warning",
  fault: "Fault",
} as const;

type TranslationKey = keyof typeof en;
type Messages = Record<TranslationKey, string>;

const de: Messages = {
  language: "Sprache",
  english: "Englisch",
  german: "Deutsch",
  offline: "offline",
  connectionFailed: "Verbindung fehlgeschlagen",
  connectionError: "Verbindungsfehler zum Wechselrichter",
  showErrorDetails: "Details zum Verbindungsfehler anzeigen",
  pollContinues:
    "Der letzte Abruf ist fehlgeschlagen. Der Dienst bleibt aktiv und versucht es automatisch erneut.",
  connecting: "Verbindung wird hergestellt…",
  noInverterData: "Noch keine Wechselrichterdaten vorhanden.",
  currentPower: "Aktuelle Leistung",
  yieldToday: "Heutiger Ertrag",
  today: "heute",
  production: "Produktion",
  energyData: "Energiedaten",
  events: "Ereignisse",
  deviceStatistics: "Gerätestatistik",
  gridQuality: "Netzqualität",
  dataNavigation: "Datennavigation",
  dataAreas: "Datenbereiche",
  system: "System",
  transmissions: "Übertragungen",
  applicationLog: "Anwendungslog",
  systemTabs: "Systemansichten",
  colTime: "Zeit",
  colTarget: "Ziel",
  colTransport: "Transport",
  colRequest: "Anfrage",
  colCommand: "Kommando",
  colRegisters: "Register",
  colDuration: "Dauer",
  colFrames: "Frames",
  colOutcome: "Ergebnis",
  colLevel: "Stufe",
  colMessage: "Meldung",
  colSource: "Quelle",
  outcomeOk: "ok",
  outcomeEmpty: "keine Antwort",
  outcomeUnsupported: "nicht verfügbar",
  outcomeFailed: "fehlgeschlagen",
  levelError: "Fehler",
  levelWarn: "Warnung",
  levelInfo: "Info",
  levelDebug: "Debug",
  levelTrace: "Trace",
  allOutcomes: "Alle Ergebnisse",
  allLevels: "Alle Stufen",
  minimumLevel: "Mindeststufe",
  filterPlaceholder: "Filtern…",
  pauseRefresh: "Pausieren",
  resumeRefresh: "Fortsetzen",
  refreshPaused: "Aktualisierung pausiert",
  refreshFailed:
    "Aktualisierung fehlgeschlagen — angezeigt werden die zuletzt geladenen Daten.",
  loadOlder: "Ältere Einträge laden",
  loadingOlder: "Lädt…",
  atOldestEntry: "Ältester vorgehaltener Eintrag erreicht.",
  windowFull: "Zeigt die letzten {hours} h, wie konfiguriert.",
  windowShortened:
    "Zeigt die letzten {actual} h der konfigurierten {hours} h — die Zeilengrenze von {cap} wurde vorher erreicht.",
  ringEmpty: "Noch nichts aufgezeichnet.",
  transmissionsDisabled:
    "Aufzeichnung der Übertragungen ist abgeschaltet (service.transmission_log_retention_hours = 0).",
  logCaptureDisabled:
    "Logaufzeichnung ist abgeschaltet (service.application_log_retention_hours = 0).",
  droppedEntries:
    "{count} Einträge wurden verworfen, weil der Writer nicht hinterherkam — eine Lücke hier ist ein Verwurf, keine ruhige Phase.",
  logNotFilteredByInverter:
    "Logsätze sind keinem einzelnen Wechselrichter zugeordnet, der Wechselrichter-Filter greift hier nicht.",
  logIsMemoryOnly:
    "Wird im Speicher des Dienstes gehalten und geht beim Neustart verloren — das Journal bzw. Container-Log ist die dauerhafte Kopie.",
  logRingReset: "Dienst wurde neu gestartet; diese Liste beginnt wieder beim neuesten Satz.",
  noDevicesAddressed: "keine Geräte",
  devicesAnswered: "{answered} von {addressed} geantwortet",
  allInverters: "Alle Wechselrichter",
  day: "Tag",
  week: "Woche",
  month: "Monat",
  year: "Jahr",
  live: "Live",
  loading: "Wird geladen…",
  loadError: "Daten konnten nicht geladen werden: {error}",
  noProductionData: "Keine Produktionsdaten für diesen Zeitraum ({range}).",
  powerAndEnergy: "Leistung & Energie",
  power: "Leistung",
  totalPower: "Gesamtleistung",
  energyGenerated: "Erzeugte Energie",
  totalEnergy: "Gesamtenergie",
  temperature: "Temperatur",
  visibleChartSeries: "Sichtbare Diagrammreihen",
  hideSeries: "{label} ausblenden",
  showSeries: "{label} anzeigen",
  todaysEnergy: "Heutige Energie",
  energyThisDay: "Energie an diesem Tag",
  peakPower: "Spitzenleistung",
  averagePower: "Durchschnittsleistung",
  recordedIntervals: "{count} erfasste Intervalle",
  vsYesterday: "Gegenüber gestern",
  vsPreviousDay: "Gegenüber dem Vortag",
  previous: "Vorher: {value}",
  noPreviousDayData: "Keine Daten für den Vortag",
  dayTotals: "Tagessummen",
  dayTotalsCaption:
    "Tageszusammenfassung der ausgewählten Wechselrichteransicht.",
  inverter: "Wechselrichter",
  lastTemperature: "Letzte Temperatur",
  fiveMinuteValues: "5-Minuten-Werte",
  fiveMinuteCaption: "Fünf-Minuten-Produktionswerte, neueste zuerst.",
  time: "Uhrzeit",
  total: "Gesamt",
  energyThisWeek: "Energie dieser Woche",
  energyThisMonth: "Energie dieses Monats",
  energyThisYear: "Energie dieses Jahres",
  averageDaily: "Tagesdurchschnitt",
  averageReference: "Mittelwert: {value}",
  averageMonthly: "Monatsdurchschnitt",
  recordedDays: "{count} erfasste Tage",
  recordedMonths: "{count} erfasste Monate",
  bestDay: "Bester Tag",
  bestMonth: "Bester Monat",
  vsLastWeek: "Gegenüber letzter Woche",
  vsLastMonth: "Gegenüber letztem Monat",
  vsLastYear: "Gegenüber letztem Jahr",
  noPreviousWeekData: "Keine Daten für die Vorwoche",
  noPreviousMonthData: "Keine Daten für den Vormonat",
  noPreviousYearData: "Keine Daten für das Vorjahr",
  dailyEnergyThisWeek: "Tägliche Energie dieser Woche",
  dailyEnergyThisMonth: "Tägliche Energie dieses Monats",
  monthlyEnergyThisYear: "Monatliche Energie dieses Jahres",
  dailyValues: "Tageswerte",
  monthlyValues: "Monatswerte",
  generated: "Erzeugt",
  shareOfWeek: "Anteil der Woche",
  shareOfMonth: "Anteil des Monats",
  shareOfYear: "Anteil des Jahres",
  dailyWeekCaption:
    "Tägliche Energiewerte der ausgewählten Woche, neueste zuerst.",
  dailyMonthCaption:
    "Tägliche Energiewerte des aktuellen Monats, neueste zuerst.",
  monthlyYearCaption:
    "Monatliche Energiewerte des ausgewählten Jahres, neueste zuerst.",
  dayNavigation: "Tagesnavigation",
  weekNavigation: "Wochennavigation",
  monthNavigation: "Monatsnavigation",
  yearNavigation: "Jahresnavigation",
  previousDay: "Vorheriger Tag",
  nextDay: "Nächster Tag",
  previousWeek: "Vorherige Woche",
  nextWeek: "Nächste Woche",
  previousMonth: "Vorheriger Monat",
  nextMonth: "Nächster Monat",
  previousYear: "Vorheriges Jahr",
  nextYear: "Nächstes Jahr",
  backToToday: "Zurück zu heute",
  backToCurrentWeek: "Zurück zur aktuellen Woche",
  backToCurrentMonth: "Zurück zum aktuellen Monat",
  backToCurrentYear: "Zurück zum aktuellen Jahr",
  diagnostics: "Diagnose",
  noDiagnosticData: "Keine Diagnosedaten für diesen Tag vorhanden.",
  latestMeasurementFallback:
    "Für diesen Tag liegen keine Diagnosedaten vor. Angezeigt wird der letzte Messwert vom {time}.",
  lifetimeEnergy: "Gesamtertrag",
  operatingTime: "Betriebszeit",
  feedInTime: "Einspeisezeit",
  averageEfficiency: "Durchschnittlicher Wirkungsgrad",
  signalStrength: "Signalstärke",
  deviceDetails: "Gerätedetails",
  model: "Modell",
  firmware: "Firmware",
  status: "Status",
  mpptPerformance: "MPPT-/String-Leistung",
  mpptPower: "MPPT-Leistung",
  noMpptData: "Keine MPPT-Tracker erfasst.",
  tracker: "Tracker {number}",
  dcPower: "DC-Leistung",
  dcVoltage: "DC-Spannung",
  dcCurrent: "DC-Strom",
  electricalConditions: "Elektrische Messwerte",
  acVoltage: "AC-Spannung",
  acCurrent: "AC-Strom",
  gridFrequency: "Netzfrequenz",
  efficiency: "Wirkungsgrad",
  recentEvents: "Letzte Warnungen und Fehler",
  noEvents: "Keine Warnungen oder Fehler aufgezeichnet.",
  eventTime: "Zeit",
  device: "Gerät",
  category: "Kategorie",
  event: "Ereignis",
  code: "Code",
  trackerColumn: "Tracker",
  ok: "OK",
  warning: "Warnung",
  fault: "Fehler",
};

const messages: Record<Language, Messages> = { en, de };

interface I18nContextValue {
  language: Language;
  locale: string;
  setLanguage: (language: Language) => void;
  t: (key: TranslationKey, values?: Record<string, string | number>) => string;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [language, setLanguage] = useState<Language>(detectLanguage);
  const locale = language === "de" ? "de-DE" : "en-US";

  useEffect(() => {
    document.documentElement.lang = language;
    window.localStorage.setItem(STORAGE_KEY, language);
  }, [language]);

  const value = useMemo<I18nContextValue>(
    () => ({
      language,
      locale,
      setLanguage,
      t: (key, values) => interpolate(messages[language][key], values),
    }),
    [language, locale],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const context = useContext(I18nContext);
  if (!context) {
    throw new Error("useI18n must be used inside I18nProvider");
  }
  return context;
}

export function localizeSeriesLabel(
  label: string,
  metric: "power" | "energy" | "temperature",
  aggregate: boolean,
  t: I18nContextValue["t"],
): string {
  if (aggregate) {
    if (metric === "power") return t("totalPower");
    if (metric === "energy") return t("totalEnergy");
  }

  const suffix = /\s+(Power|Energy(?: Generated)?|Temperature)$/;
  const base = label.replace(suffix, "");
  if (base !== label) {
    const metricLabel =
      metric === "power"
        ? t("power")
        : metric === "energy"
          ? t("energyGenerated")
          : t("temperature");
    return `${base} ${metricLabel}`;
  }

  if (metric === "power" && /^power$/i.test(label)) return t("power");
  if (metric === "energy" && /^energy(?: generated)?$/i.test(label)) {
    return t("energyGenerated");
  }
  if (metric === "temperature" && /^temperature$/i.test(label)) {
    return t("temperature");
  }
  return label;
}

function detectLanguage(): Language {
  if (typeof window === "undefined") return "en";
  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored === "en" || stored === "de") return stored;
  return window.navigator.language.toLowerCase().startsWith("de") ? "de" : "en";
}

function interpolate(
  message: string,
  values?: Record<string, string | number>,
): string {
  if (!values) return message;
  return message.replace(/\{(\w+)\}/g, (placeholder, key: string) =>
    key in values ? String(values[key]) : placeholder,
  );
}
