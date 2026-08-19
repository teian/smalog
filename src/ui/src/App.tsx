import { useEffect, useMemo, useState } from "react";
import {
  fetchInverters,
  fetchStatus,
  type Inverter,
  type Range,
  type Status,
} from "@/lib/api";
import { StatusCards } from "@/components/StatusCards";
import { ServiceStatus } from "@/components/ServiceStatus";
import { HistoryChart } from "@/components/HistoryChart";
import {
  DashboardNavigation,
  type DashboardSection,
} from "@/components/DashboardNavigation";
import {
  DiagnosticsView,
  type DiagnosticSection,
} from "@/components/DiagnosticsView";
import { SystemView, type SystemTab } from "@/components/SystemView";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { type Language, useI18n } from "@/lib/i18n";

const RANGES: Range[] = ["day", "week", "month", "year"];
const SYSTEM_TABS: SystemTab[] = ["transmissions", "log"];

export default function App() {
  const { language, setLanguage, t } = useI18n();
  const [status, setStatus] = useState<Status | null>(null);
  const [inverters, setInverters] = useState<Inverter[]>([]);
  const [range, setRange] = useState<Range>("day");
  const [serial, setSerial] = useState<number | null>(null); // null = aggregate
  const [section, setSection] = useState<DashboardSection>("energy");
  // Kept in App so the choice survives leaving and re-entering the area.
  const [systemTab, setSystemTab] = useState<SystemTab>("transmissions");
  const [refreshKey, setRefreshKey] = useState(0);
  // The last poll failed. The data stays on screen; only the badge changes.
  const [offline, setOffline] = useState(false);

  // Poll live status every 30 s. The cards and charts update in place — a
  // failed poll keeps the last reading on screen rather than emptying the
  // page, which would flicker once every time the network hiccups.
  useEffect(() => {
    let active = true;
    const load = () =>
      fetchStatus()
        .then((s) => {
          if (!active) return;
          setStatus(s);
          setOffline(false);
          setRefreshKey((value) => value + 1);
        })
        .catch(() => active && setOffline(true));
    load();
    const timer = setInterval(load, 30_000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);

  // Inverter list for the filter (once).
  useEffect(() => {
    fetchInverters()
      .then(setInverters)
      .catch(() => setInverters([]));
  }, []);

  const heading = useMemo(() => {
    if (serial === null) return t("allInverters");
    return inverters.find((i) => i.serial === serial)?.name || `#${serial}`;
  }, [serial, inverters, t]);

  const sectionTitle = t(
    section === "energy"
      ? "energyData"
      : section === "events"
        ? "events"
        : section === "device"
          ? "deviceStatistics"
          : section === "grid"
            ? "gridQuality"
            : "system",
  );

  return (
    <div className="mx-auto flex max-w-7xl flex-col gap-6 p-4 sm:p-6">
      <header className="flex items-center justify-between gap-3">
        <h1 className="shrink-0">
          <span className="sr-only">SMAlog – PV Monitoring</span>
          <img
            src="/smalog-logo.svg"
            alt=""
            aria-hidden="true"
            className="h-9 w-auto dark:hidden sm:h-11"
          />
          <img
            src="/smalog-logo-dark.svg"
            alt=""
            aria-hidden="true"
            className="hidden h-9 w-auto dark:block sm:h-11"
          />
        </h1>
        <div className="flex min-w-0 items-center gap-2 sm:gap-3">
          <label className="sr-only" htmlFor="language">
            {t("language")}
          </label>
          <select
            id="language"
            aria-label={t("language")}
            className="rounded-md border bg-background px-2 py-1 text-sm"
            value={language}
            onChange={(event) => setLanguage(event.target.value as Language)}
          >
            <option value="en">{t("english")}</option>
            <option value="de">{t("german")}</option>
          </select>
          <ServiceStatus status={status} offline={offline} />
        </div>
      </header>

      <div className="grid min-w-0 gap-6 lg:grid-cols-[14rem_minmax(0,1fr)]">
        <DashboardNavigation active={section} onChange={setSection} />

        <main className="flex min-w-0 flex-col gap-6">
          {section === "energy" && <StatusCards status={status} />}

          <Card>
            <CardHeader className="flex-col gap-3 space-y-0 xl:flex-row xl:items-center xl:justify-between">
              <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-center">
                <CardTitle className="text-foreground">
                  {sectionTitle} — {heading}
                </CardTitle>
                <label className="sr-only" htmlFor="inverter">
                  {t("inverter")}
                </label>
                <select
                  id="inverter"
                  className="min-h-9 w-full rounded-md border bg-background px-2 py-1 text-sm sm:w-auto"
                  value={serial === null ? "all" : String(serial)}
                  onChange={(event) =>
                    setSerial(
                      event.target.value === "all"
                        ? null
                        : Number(event.target.value),
                    )
                  }
                >
                  <option value="all">{t("allInverters")}</option>
                  {inverters.map((inverter) => (
                    <option key={inverter.serial} value={inverter.serial}>
                      {inverter.name || `#${inverter.serial}`}
                    </option>
                  ))}
                </select>
              </div>
              {section === "energy" && (
                <Tabs value={range} onValueChange={(v) => setRange(v as Range)}>
                  <TabsList className="grid w-full grid-cols-4 sm:w-auto">
                    {RANGES.map((r) => (
                      <TabsTrigger key={r} value={r} className="capitalize">
                        {t(r)}
                      </TabsTrigger>
                    ))}
                  </TabsList>
                </Tabs>
              )}
              {section === "system" && (
                <Tabs
                  value={systemTab}
                  onValueChange={(value) => setSystemTab(value as SystemTab)}
                >
                  <TabsList
                    aria-label={t("systemTabs")}
                    className="grid w-full grid-cols-2 sm:w-auto"
                  >
                    {SYSTEM_TABS.map((tab) => (
                      <TabsTrigger key={tab} value={tab}>
                        {t(tab === "transmissions" ? "transmissions" : "applicationLog")}
                      </TabsTrigger>
                    ))}
                  </TabsList>
                </Tabs>
              )}
            </CardHeader>
            <CardContent className="min-w-0">
              {section === "energy" ? (
                <HistoryChart
                  range={range}
                  serial={serial}
                  inverterName={heading}
                />
              ) : section === "system" ? (
                <SystemView tab={systemTab} serial={serial} />
              ) : (
                <DiagnosticsView
                  serial={serial}
                  refreshKey={refreshKey}
                  section={section as DiagnosticSection}
                />
              )}
            </CardContent>
          </Card>
        </main>
      </div>
    </div>
  );
}
