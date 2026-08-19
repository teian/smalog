import { useEffect, useMemo, useRef, useState } from "react";
import {
  Area,
  CartesianGrid,
  ComposedChart,
  Line,
  XAxis,
  YAxis,
} from "recharts";
import {
  fetchDiagnostics,
  type DiagnosticEvent,
  type DiagnosticMppt,
  type Diagnostics,
  type InverterDiagnostics,
} from "@/lib/api";
import {
  formatEnergyKilowattHours,
  formatNumber,
  formatPowerWatts,
} from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import {
  buildMpptChartData,
  mpptSeriesKey,
  orderedMppts,
} from "@/lib/mppts";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import {
  Table,
  TableBody,
  TableCaption,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const MPPT_COLORS = [
  "hsl(217 91% 60%)",
  "hsl(150 60% 45%)",
  "hsl(38 92% 55%)",
  "hsl(280 65% 60%)",
  "hsl(0 72% 60%)",
  "hsl(190 80% 50%)",
  "hsl(330 75% 60%)",
  "hsl(90 55% 48%)",
] as const;

export type DiagnosticSection = "events" | "device" | "grid";

export function DiagnosticsView({
  date,
  serial,
  refreshKey,
  section,
}: {
  date?: string;
  serial: number | null;
  refreshKey: number;
  section: DiagnosticSection;
}) {
  const { locale, t } = useI18n();
  const [diagnostics, setDiagnostics] = useState<Diagnostics | null>(null);
  const [error, setError] = useState<string | null>(null);
  // What the tables on screen show. The 30-second refresh swaps their values;
  // only a different day or inverter empties them, since keeping one
  // inverter's rows under another's name would be wrong rather than merely
  // stale.
  const shownQuery = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const query = `${date ?? ""}|${serial ?? "all"}`;
    if (shownQuery.current !== query) {
      setDiagnostics(null);
    }
    setError(null);
    fetchDiagnostics(date, serial)
      .then((data) => {
        if (cancelled) return;
        shownQuery.current = query;
        setDiagnostics(data);
      })
      .catch((reason: unknown) => !cancelled && setError(String(reason)));
    return () => {
      cancelled = true;
    };
  }, [date, serial, refreshKey]);

  // A failed refresh keeps the tables that are already up.
  if (error && !diagnostics) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        {t("loadError", { error })}
      </p>
    );
  }
  if (!diagnostics) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        {t("loading")}
      </p>
    );
  }
  if (section !== "events" && diagnostics.inverters.length === 0) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        {t("noDiagnosticData")}
      </p>
    );
  }

  const names = new Map(
    diagnostics.inverters.map((inverter) => [inverter.serial, inverter.name]),
  );

  return (
    <section className="flex flex-col gap-4" aria-labelledby="diagnostics-title">
      <h4 id="diagnostics-title" className="sr-only">
        {t(
          section === "events"
            ? "events"
            : section === "device"
              ? "deviceStatistics"
              : "gridQuality",
        )}
      </h4>
      {section === "events" ? (
        <EventTable
          events={diagnostics.events}
          inverterNames={names}
          locale={locale}
        />
      ) : (
        diagnostics.inverters.map((inverter) => (
          <InverterDiagnosticPanel
            key={inverter.serial}
            inverter={inverter}
            section={section}
          />
        ))
      )}
    </section>
  );
}

function InverterDiagnosticPanel({
  inverter,
  section,
}: {
  inverter: InverterDiagnostics;
  section: Exclude<DiagnosticSection, "events">;
}) {
  const { locale, t } = useI18n();
  const latest = inverter.rows.at(-1);
  const trackerMeasurement = latest ?? inverter.latestMeasurement;
  const gridMeasurement = latest ?? inverter.latestMeasurement;
  const usesGridFallback =
    section === "grid" && !latest && gridMeasurement !== null;
  const signal = latest?.signal;
  const trackerRows = orderedMppts(trackerMeasurement?.mppts ?? []);

  return (
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle className="text-foreground">
          {inverter.name} · {inverter.model || `#${inverter.serial}`}
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div className="grid gap-3 sm:grid-cols-2 2xl:grid-cols-4">
          {section === "device" && (
            <DiagnosticMetric
              label={t("lifetimeEnergy")}
              value={formatEnergyKilowattHours(inverter.totalEnergy, locale)}
            />
          )}
          {section === "device" && (
            <DiagnosticMetric
              label={t("operatingTime")}
              value={formatHours(inverter.operatingTime, locale)}
            />
          )}
          {section === "device" && (
            <DiagnosticMetric
              label={t("feedInTime")}
              value={formatHours(inverter.feedInTime, locale)}
            />
          )}
          {section === "device" && (
            <DiagnosticMetric
              label={t("averageEfficiency")}
              value={
                inverter.averageEfficiency === null
                  ? "—"
                  : `${formatNumber(inverter.averageEfficiency, locale)}%`
              }
            />
          )}
          {section === "device" &&
            signal !== null &&
            signal !== undefined &&
            signal > 0 && (
              <DiagnosticMetric
                label={t("signalStrength")}
                value={`${formatNumber(signal, locale)}%`}
              />
            )}
          {section === "grid" && gridMeasurement && (
            <DiagnosticMetric
              label={t("acVoltage")}
              value={`${formatNumber(gridMeasurement.acVoltage, locale)}\u00a0V`}
            />
          )}
          {section === "grid" && gridMeasurement && (
            <DiagnosticMetric
              label={t("acCurrent")}
              value={`${formatNumber(gridMeasurement.acCurrent, locale)}\u00a0A`}
            />
          )}
          {section === "grid" && gridMeasurement && (
            <DiagnosticMetric
              label={t("gridFrequency")}
              value={`${formatNumber(gridMeasurement.frequency, locale)}\u00a0Hz`}
            />
          )}
        </div>

        {usesGridFallback && gridMeasurement && (
          <p className="text-sm text-muted-foreground">
            {t("latestMeasurementFallback", {
              time: formatMeasurementTime(gridMeasurement.timestamp, locale),
            })}
          </p>
        )}

        {section === "device" && (
          <>
            <div className="grid gap-4 2xl:grid-cols-2">
              <DeviceDetails inverter={inverter} />
              <TrackerTable mppts={trackerRows} />
            </div>
            {inverter.rows.some((row) => row.mppts.length > 0) && (
              <MpptChart inverter={inverter} />
            )}
          </>
        )}
        {section === "grid" && inverter.rows.length > 0 && (
          <div>
            <GridChart inverter={inverter} />
          </div>
        )}
        {inverter.rows.length === 0 &&
          (section !== "grid" || !gridMeasurement) && (
            <p className="text-sm text-muted-foreground">
              {t("noDiagnosticData")}
            </p>
          )}
      </CardContent>
    </Card>
  );
}

function formatMeasurementTime(timestamp: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(timestamp * 1000));
}

function DiagnosticMetric({
  label,
  value,
}: {
  label: string;
  value: string;
}) {
  return (
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>{label}</CardTitle>
      </CardHeader>
      <CardContent>
        <span className="text-xl font-semibold tabular-nums">{value}</span>
      </CardContent>
    </Card>
  );
}

function DeviceDetails({ inverter }: { inverter: InverterDiagnostics }) {
  const { t } = useI18n();
  const rows = [
    [t("model"), inverter.model || "—"],
    [t("firmware"), inverter.firmware || "—"],
    [t("status"), localizeStatus(inverter.status, t)],
    [t("inverter"), String(inverter.serial)],
  ];

  return (
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>{t("deviceDetails")}</CardTitle>
      </CardHeader>
      <CardContent>
        <Table>
          <TableCaption className="sr-only">{t("deviceDetails")}</TableCaption>
          <TableBody>
            {rows.map(([label, value]) => (
              <TableRow key={label}>
                <TableHead>{label}</TableHead>
                <TableCell className="text-right font-mono">{value}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

function TrackerTable({
  mppts,
}: {
  mppts: DiagnosticMppt[];
}) {
  const { locale, t } = useI18n();

  return (
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>{t("mpptPerformance")}</CardTitle>
      </CardHeader>
      <CardContent>
        {mppts.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("noMpptData")}</p>
        ) : (
          <Table>
            <TableCaption className="sr-only">
              {t("mpptPerformance")}
            </TableCaption>
            <TableHeader>
              <TableRow>
                <TableHead>{t("trackerColumn")}</TableHead>
                <TableHead className="text-right">{t("dcPower")}</TableHead>
                <TableHead className="text-right">{t("dcVoltage")}</TableHead>
                <TableHead className="text-right">{t("dcCurrent")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {mppts.map((mppt) => (
                <TableRow key={mppt.tracker_number}>
                  <TableCell>
                    {t("tracker", { number: mppt.tracker_number })}
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatOptional(
                      mppt.dc_power_w,
                      (value) => formatPowerWatts(value, locale),
                    )}
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatOptional(
                      mppt.dc_voltage_mv,
                      (value) =>
                        `${formatNumber(value / 1000, locale)}\u00a0V`,
                    )}
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatOptional(
                      mppt.dc_current_ma,
                      (value) =>
                        `${formatNumber(value / 1000, locale)}\u00a0A`,
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
    </Card>
  );
}

function MpptChart({ inverter }: { inverter: InverterDiagnostics }) {
  const { locale, t } = useI18n();
  const { rows, trackers } = useMemo(
    () => buildMpptChartData(inverter.rows),
    [inverter.rows],
  );
  const config = useMemo(
    () =>
      Object.fromEntries(
        trackers.map((tracker, index) => [
          mpptSeriesKey(tracker),
          {
            label: t("tracker", { number: tracker }),
            color: MPPT_COLORS[index % MPPT_COLORS.length],
          },
        ]),
      ) satisfies ChartConfig,
    [t, trackers],
  );

  return (
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>{t("mpptPower")}</CardTitle>
      </CardHeader>
      <CardContent>
        <ChartContainer config={config} className="h-[280px] w-full aspect-auto">
          <ComposedChart accessibilityLayer data={rows}>
            <CartesianGrid vertical={false} />
            <XAxis
              dataKey="label"
              axisLine={false}
              tickLine={false}
              tickMargin={10}
              minTickGap={32}
            />
            <YAxis
              axisLine={false}
              tickLine={false}
              width={72}
              tickFormatter={(value) => formatPowerWatts(value, locale)}
            />
            <ChartTooltip
              content={
                <ChartTooltipContent
                  indicator="line"
                  formatter={(value) => (
                    <span className="font-mono font-medium tabular-nums">
                      {formatPowerWatts(Number(value), locale)}
                    </span>
                  )}
                />
              }
            />
            {trackers.map((tracker) => {
              const key = mpptSeriesKey(tracker);
              return (
              <Area
                key={tracker}
                type="monotone"
                dataKey={key}
                name={t("tracker", { number: tracker })}
                stroke={`var(--color-${key})`}
                fill={`var(--color-${key})`}
                fillOpacity={0.12}
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
              />
              );
            })}
          </ComposedChart>
        </ChartContainer>
      </CardContent>
    </Card>
  );
}

function formatOptional(
  value: number | null,
  formatter: (value: number) => string,
): string {
  return value === null ? "—" : formatter(value);
}

function GridChart({ inverter }: { inverter: InverterDiagnostics }) {
  const { locale, t } = useI18n();
  const rows = inverter.rows.filter(
    (row) => row.acVoltage > 0 || row.frequency > 0,
  );
  const config = useMemo(
    () =>
      ({
        acVoltage: {
          label: t("acVoltage"),
          color: "hsl(38 92% 55%)",
        },
        frequency: {
          label: t("gridFrequency"),
          color: "hsl(280 65% 60%)",
        },
      }) satisfies ChartConfig,
    [t],
  );

  return (
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>{t("electricalConditions")}</CardTitle>
      </CardHeader>
      <CardContent>
        <ChartContainer config={config} className="h-[280px] w-full aspect-auto">
          <ComposedChart accessibilityLayer data={rows}>
            <CartesianGrid vertical={false} />
            <XAxis
              dataKey="label"
              axisLine={false}
              tickLine={false}
              tickMargin={10}
              minTickGap={32}
            />
            <YAxis
              yAxisId="voltage"
              axisLine={false}
              tickLine={false}
              width={60}
              tickFormatter={(value) => `${formatNumber(value, locale)} V`}
            />
            <YAxis
              yAxisId="frequency"
              orientation="right"
              axisLine={false}
              tickLine={false}
              width={68}
              domain={["dataMin - 0.1", "dataMax + 0.1"]}
              tickFormatter={(value) => `${formatNumber(value, locale)} Hz`}
            />
            <ChartTooltip
              content={
                <ChartTooltipContent
                  indicator="line"
                  formatter={(value, name) => (
                    <span className="font-mono font-medium tabular-nums">
                      {formatNumber(Number(value), locale)}
                      {String(name) === "frequency" ? "\u00a0Hz" : "\u00a0V"}
                    </span>
                  )}
                />
              }
            />
            <Line
              yAxisId="voltage"
              type="monotone"
              dataKey="acVoltage"
              stroke="var(--color-acVoltage)"
              strokeWidth={2}
              dot={false}
              isAnimationActive={false}
            />
            <Line
              yAxisId="frequency"
              type="monotone"
              dataKey="frequency"
              stroke="var(--color-frequency)"
              strokeWidth={1.5}
              dot={false}
              isAnimationActive={false}
            />
          </ComposedChart>
        </ChartContainer>
      </CardContent>
    </Card>
  );
}

function EventTable({
  events,
  inverterNames,
  locale,
}: {
  events: DiagnosticEvent[];
  inverterNames: Map<number, string>;
  locale: string;
}) {
  const { t } = useI18n();

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("recentEvents")}</CardTitle>
      </CardHeader>
      <CardContent>
        {events.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("noEvents")}</p>
        ) : (
          <div className="max-h-80 overflow-auto rounded-md border">
            <Table className="min-w-[44rem]">
              <TableCaption className="sr-only">{t("recentEvents")}</TableCaption>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("eventTime")}</TableHead>
                  <TableHead>{t("device")}</TableHead>
                  <TableHead>{t("category")}</TableHead>
                  <TableHead>{t("event")}</TableHead>
                  <TableHead className="text-right">{t("code")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.map((event) => (
                  <TableRow key={`${event.serial}-${event.timestamp}-${event.code}`}>
                    <TableCell className="whitespace-nowrap font-mono text-xs">
                      {formatTimestamp(event.timestamp, locale)}
                    </TableCell>
                    <TableCell>
                      {inverterNames.get(event.serial) ?? `#${event.serial}`}
                    </TableCell>
                    <TableCell>{localizeStatus(event.category, t)}</TableCell>
                    <TableCell>{event.message || event.group}</TableCell>
                    <TableCell className="text-right font-mono">
                      {event.code}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function formatHours(value: number, locale: string): string {
  return `${formatNumber(value, locale)}\u00a0h`;
}

function formatTimestamp(timestamp: number, locale: string): string {
  const date = new Date(timestamp * 1000);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function localizeStatus(
  status: string,
  t: ReturnType<typeof useI18n>["t"],
): string {
  if (/^ok$/i.test(status)) return t("ok");
  if (/^warning$/i.test(status)) return t("warning");
  if (/^fault$/i.test(status)) return t("fault");
  return status || "—";
}
