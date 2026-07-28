import { type ReactNode, useEffect, useMemo, useState } from "react";
import {
  Area,
  Bar,
  BarChart,
  CartesianGrid,
  ComposedChart,
  Line,
  ReferenceLine,
  XAxis,
  YAxis,
} from "recharts";
import {
  fetchHistory,
  type DaySummary,
  type History,
  type HistoryMetric,
  type HistorySeries,
  type Range,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import {
  calculateDaySummary,
  DayHistorySummary,
} from "@/components/DayHistorySummary";
import { DayHistoryTables } from "@/components/DayHistoryTables";
import { MonthHistoryView } from "@/components/MonthHistoryView";
import { WeekHistoryView } from "@/components/WeekHistoryView";
import { YearHistoryView } from "@/components/YearHistoryView";
import {
  ChartContainer,
  ChartLegend,
  ChartLegendContent,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import {
  formatEnergyKilowattHours,
  formatNumber,
  formatPowerKilowatts,
} from "@/lib/format";
import {
  formatIsoDate,
  formatIsoMonth,
  shiftIsoDay,
  shiftIsoMonth,
} from "@/lib/date";
import { aggregateHistorySeries } from "@/lib/history";
import { cn } from "@/lib/utils";
import { localizeSeriesLabel, useI18n } from "@/lib/i18n";

// Distinct series colours (inverters or strings).
const COLORS = [
  "hsl(217 91% 60%)",
  "hsl(150 60% 45%)",
  "hsl(38 92% 55%)",
  "hsl(280 65% 60%)",
  "hsl(0 72% 60%)",
  "hsl(190 80% 50%)",
  "hsl(330 75% 60%)",
  "hsl(90 55% 48%)",
  "hsl(25 90% 58%)",
  "hsl(250 70% 65%)",
];

const LEGACY_DAY_SERIES: HistorySeries[] = [
  { key: "power", label: "Power", metric: "power" },
  { key: "energy", label: "Energy Generated", metric: "energy" },
  { key: "temperature", label: "Temperature", metric: "temperature" },
];

/** Day view combines power, generated energy and inverter temperature;
 *  coarser ranges remain yield bars. */
export function HistoryChart({
  range,
  serial,
  inverterName,
}: {
  range: Range;
  serial: number | null;
  inverterName: string;
}) {
  const { locale, t } = useI18n();
  const [history, setHistory] = useState<History | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [day, setDay] = useState<string | null>(null);
  const [today, setToday] = useState<string | null>(null);
  const [week, setWeek] = useState<string | null>(null);
  const [currentWeek, setCurrentWeek] = useState<string | null>(null);
  const [month, setMonth] = useState<string | null>(null);
  const [currentMonth, setCurrentMonth] = useState<string | null>(null);
  const [year, setYear] = useState<string | null>(null);
  const [currentYear, setCurrentYear] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setHistory(null);
    setError(null);
    const period =
      range === "day"
        ? day
        : range === "week"
          ? week
          : range === "month"
            ? month
            : range === "year"
              ? year
              : null;
    fetchHistory(range, serial, false, period)
      .then((data) => {
        if (cancelled) return;
        setHistory(data);
        if (range === "day" && data.date && day === null) {
          setDay(data.date);
        }
        if (range === "day" && data.today) {
          setToday(data.today);
        }
        if (range === "week" && data.weekStart && week === null) {
          setWeek(data.weekStart);
        }
        if (range === "week" && data.currentWeekStart) {
          setCurrentWeek(data.currentWeekStart);
        }
        if (range === "month" && data.month && month === null) {
          setMonth(data.month);
        }
        if (range === "month" && data.currentMonth) {
          setCurrentMonth(data.currentMonth);
        }
        if (range === "year" && data.year && year === null) {
          setYear(data.year);
        }
        if (range === "year" && data.currentYear) {
          setCurrentYear(data.currentYear);
        }
      })
      .catch((e: unknown) => !cancelled && setError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [range, serial, day, week, month, year, refresh]);

  // Today's curve is the live view and follows new poll data.
  useEffect(() => {
    if (range !== "day" || !history?.live) return;
    const timer = setInterval(() => setRefresh((value) => value + 1), 30_000);
    return () => clearInterval(timer);
  }, [range, history?.live]);

  const isDay = range === "day";
  const live = isDay && day !== null && day === today;

  let chart: ReactNode;
  if (error) {
    chart = (
      <p className="py-12 text-center text-sm text-muted-foreground">
        {t("loadError", { error })}
      </p>
    );
  } else if (!history) {
    chart = (
      <p className="py-12 text-center text-sm text-muted-foreground">
        {t("loading")}
      </p>
    );
  } else if (
    history.keys.length === 0 ||
    (history.rows.length === 0 && range !== "year")
  ) {
    chart = (
      <p className="py-12 text-center text-sm text-muted-foreground">
        {t("noProductionData", { range: t(range) })}
      </p>
    );
  } else {
    if (isDay) {
      chart = <DayHistoryChart history={history} allInverters={serial === null} />;
    } else if (range === "week") {
      chart = <WeekHistoryView history={history} />;
    } else if (range === "month") {
      chart = <MonthHistoryView history={history} />;
    } else if (range === "year") {
      chart = <YearHistoryView history={history} />;
    } else {
      const series = history.keys.map((label, index) => ({
        dataKey: `series${index}`,
        label,
        color: COLORS[index % COLORS.length],
      }));
      const rows = history.rows.map((row) => ({
        label: row.label,
        ...Object.fromEntries(
          series.map(({ dataKey, label }) => [dataKey, row[label]]),
        ),
      }));
      const config = Object.fromEntries(
        series.map(({ dataKey, label, color }) => [
          dataKey,
          { label, color },
        ]),
      ) as ChartConfig;

      chart = (
        <ChartContainer config={config} className="h-[340px] w-full aspect-auto">
          <BarChart accessibilityLayer data={rows}>
            <CartesianGrid vertical={false} />
            <XAxis
              dataKey="label"
              axisLine={false}
              tickLine={false}
              tickMargin={10}
              minTickGap={16}
            />
            <YAxis
              axisLine={false}
              tickLine={false}
              width={80}
              tickFormatter={(value) =>
                formatEnergyKilowattHours(value, locale)
              }
            />
            <ChartTooltip
              content={
                <ChartTooltipContent
                  indicator="line"
                  formatter={(value, name, item) => (
                    <ChartValue
                      color={item.color}
                      label={String(config[String(name)]?.label ?? name)}
                      formattedValue={formatEnergyKilowattHours(
                        Number(value),
                        locale,
                      )}
                    />
                  )}
                />
              }
            />
            {series.length > 1 && (
              <ChartLegend content={<ChartLegendContent />} />
            )}
            {series.map(({ dataKey }) => (
              <Bar
                key={dataKey}
                dataKey={dataKey}
                fill={`var(--color-${dataKey})`}
                radius={[4, 4, 0, 0]}
              />
            ))}
          </BarChart>
        </ChartContainer>
      );
    }
  }

  return (
    <div className="flex flex-col gap-4">
      {isDay && (
        <div className="flex flex-col gap-3 border-b pb-3 sm:flex-row sm:items-center sm:justify-between">
          <h4 className="text-sm font-semibold text-foreground">
            {t("powerAndEnergy")} — {live ? t("live") : t("day")}
          </h4>
        </div>
      )}
      {isDay && day && today && (
        <DayNavigator day={day} today={today} live={live} onChange={setDay} />
      )}
      {range === "week" && week && currentWeek && (
        <WeekNavigator
          weekStart={week}
          currentWeekStart={currentWeek}
          onChange={setWeek}
        />
      )}
      {range === "month" && month && currentMonth && (
        <MonthNavigator
          month={month}
          currentMonth={currentMonth}
          onChange={setMonth}
        />
      )}
      {range === "year" && year && currentYear && (
        <YearNavigator
          year={year}
          currentYear={currentYear}
          onChange={setYear}
        />
      )}
      {isDay &&
        history &&
        history.rows.length > 0 &&
        history.keys.length > 0 && (
          <DayHistorySummary history={history} live={live} />
        )}
      {chart}
      {isDay && history && history.rows.length > 0 && history.keys.length > 0 && (
        <DayHistoryTables
          history={history}
          selectedLabel={inverterName}
          aggregateOnlyValues={serial === null}
        />
      )}
    </div>
  );
}

function DayHistoryChart({
  history,
  allInverters,
}: {
  history: History;
  allInverters: boolean;
}) {
  const { locale, t } = useI18n();
  const availableSeries = useMemo(
    () => history.series ?? LEGACY_DAY_SERIES,
    [history.series],
  );
  const series = useMemo(() => {
    if (!allInverters) return availableSeries;
    return aggregateHistorySeries(availableSeries);
  }, [allInverters, availableSeries]);
  const seriesSignature = series.map(({ key }) => key).join("|");
  const [hiddenSeries, setHiddenSeries] = useState<Set<string>>(new Set());

  useEffect(() => {
    setHiddenSeries(new Set());
  }, [seriesSignature]);

  const config = useMemo(
    () => ({
      ...Object.fromEntries(
        series.map((item, index) => [
          item.key,
          {
            label: localizeSeriesLabel(
              item.label,
              item.metric,
              item.aggregate === true,
              t,
            ),
            color: COLORS[index % COLORS.length],
          },
        ]),
      ),
      averageReference: {
        label: t("averagePower"),
        color: "hsl(25 95% 53%)",
      },
    }) as ChartConfig,
    [series, t],
  );
  const seriesByKey = useMemo(
    () => new Map(series.map((item) => [item.key, item])),
    [series],
  );
  const summary =
    (history.summary as DaySummary | undefined) ??
    calculateDaySummary(history);

  const toggleSeries = (key: string) => {
    setHiddenSeries((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  return (
    <div className="flex flex-col gap-2">
      <div
        role="group"
        aria-label={t("visibleChartSeries")}
        className="flex flex-wrap justify-end gap-1"
      >
        {series.map((item) => {
          const visible = !hiddenSeries.has(item.key);
          const color = String(config[item.key]?.color);
          const label = `${localizeSeriesLabel(
            item.label,
            item.metric,
            item.aggregate === true,
            t,
          )} (${metricUnit(item.metric)})`;
          return (
            <Button
              key={item.key}
              type="button"
              variant="ghost"
              size="sm"
              aria-pressed={visible}
              aria-label={t(visible ? "hideSeries" : "showSeries", { label })}
              onClick={() => toggleSeries(item.key)}
              className={cn("h-auto py-1", !visible && "opacity-40")}
            >
              <span
                aria-hidden="true"
                className={cn(
                  "w-3 shrink-0",
                  item.metric === "energy"
                    ? "border-t-2 border-dashed"
                    : "h-0.5",
                )}
                style={
                  item.metric === "energy"
                    ? { borderColor: color }
                    : { backgroundColor: color }
                }
              />
              {label}
            </Button>
          );
        })}
      </div>

      <ChartContainer
        config={config}
        className="h-[340px] w-full aspect-auto"
      >
        <ComposedChart
          accessibilityLayer
          data={history.rows}
          margin={{ top: 8, right: 4, left: 4 }}
        >
          <defs>
            {series
              .filter(({ metric }) => metric !== "temperature")
              .map(({ key, metric }) => (
                <linearGradient
                  key={key}
                  id={`${key}-fill`}
                  x1="0"
                  y1="0"
                  x2="0"
                  y2="1"
                >
                  <stop
                    offset="0%"
                    stopColor={`var(--color-${key})`}
                    stopOpacity={metric === "energy" ? 0.22 : 0.2}
                  />
                  <stop
                    offset="100%"
                    stopColor={`var(--color-${key})`}
                    stopOpacity={0.03}
                  />
                </linearGradient>
              ))}
          </defs>
          <CartesianGrid vertical={false} />
          <XAxis
            dataKey="label"
            axisLine={false}
            tickLine={false}
            tickMargin={10}
            minTickGap={40}
          />
          <YAxis
            yAxisId="power"
            axisLine={false}
            tickLine={false}
            width={72}
            tickFormatter={(value) => formatPowerKilowatts(value, locale)}
          />
          <YAxis
            yAxisId="energy"
            orientation="right"
            axisLine={false}
            tickLine={false}
            width={76}
            tickFormatter={(value) =>
              formatEnergyKilowattHours(value, locale)
            }
          />
          <ChartTooltip
            content={
              <ChartTooltipContent
                indicator="line"
                formatter={(value, name, item) => {
                  const metadata = seriesByKey.get(String(name));
                  return (
                    <ChartValue
                      color={item.color}
                      label={
                        metadata
                          ? localizeSeriesLabel(
                              metadata.label,
                              metadata.metric,
                              metadata.aggregate === true,
                              t,
                            )
                          : String(name)
                      }
                      formattedValue={formatDayMetric(
                        metadata?.metric ?? "power",
                        value,
                        locale,
                      )}
                    />
                  );
                }}
              />
            }
          />
          {Number.isFinite(summary.averagePower) &&
            summary.averagePower > 0 && (
              <ReferenceLine
                yAxisId="power"
                y={summary.averagePower}
                stroke="var(--color-averageReference)"
                strokeWidth={1.5}
                strokeDasharray="6 4"
                label={{
                  value: t("averageReference", {
                    value: formatPowerKilowatts(
                      summary.averagePower,
                      locale,
                    ),
                  }),
                  position: "insideTopRight",
                  fill: "hsl(var(--muted-foreground))",
                  fontSize: 12,
                }}
              />
            )}

          {series.map((item) =>
            item.metric === "temperature" ? (
              <Line
                key={item.key}
                yAxisId="power"
                type="monotone"
                dataKey={item.key}
                stroke={`var(--color-${item.key})`}
                strokeDasharray="5 3"
                strokeWidth={1.5}
                dot={false}
                activeDot={{ r: 3 }}
                connectNulls={false}
                hide={hiddenSeries.has(item.key)}
                animationDuration={500}
              />
            ) : (
              <Area
                key={item.key}
                yAxisId={item.metric === "energy" ? "energy" : "power"}
                type="monotone"
                dataKey={item.key}
                stroke={`var(--color-${item.key})`}
                fill={`url(#${item.key}-fill)`}
                strokeWidth={
                  item.aggregate ? 3 : item.metric === "power" ? 2.5 : 2
                }
                dot={false}
                activeDot={{ r: 4, strokeWidth: 2 }}
                hide={hiddenSeries.has(item.key)}
                animationDuration={500}
              />
            ),
          )}
        </ComposedChart>
      </ChartContainer>
    </div>
  );
}

function ChartValue({
  color,
  label,
  formattedValue,
}: {
  color?: string;
  label: string;
  formattedValue: string;
}) {
  return (
    <div className="flex w-full min-w-44 items-center gap-2">
      <span
        className="size-2 shrink-0 rounded-[2px]"
        style={{ backgroundColor: color }}
      />
      <span className="text-muted-foreground">{label}</span>
      <span className="ml-auto font-mono font-medium tabular-nums text-foreground">
        {formattedValue}
      </span>
    </div>
  );
}

function formatDayMetric(
  metric: HistoryMetric,
  value: number | string | readonly (number | string)[] | undefined,
  locale: string,
): string {
  const numeric = Number(Array.isArray(value) ? value[0] : value);
  if (!Number.isFinite(numeric)) return String(value);
  if (metric === "power") return formatPowerKilowatts(numeric, locale);
  if (metric === "energy") return formatEnergyKilowattHours(numeric, locale);
  return `${formatNumber(numeric, locale)}\u00a0°C`;
}

function metricUnit(metric: HistoryMetric): string {
  if (metric === "power") return "W / KW / MW / GW / TW / PW";
  if (metric === "energy") return "Wh / KWh / MWh / GWh / TWh / PWh";
  return "°C";
}

function DayNavigator({
  day,
  today,
  live,
  onChange,
}: {
  day: string;
  today: string;
  live: boolean;
  onChange: (day: string) => void;
}) {
  const { locale, t } = useI18n();
  const label = formatIsoDate(day, {
    weekday: "short",
    day: "2-digit",
    month: "long",
    year: "numeric",
  }, locale);

  return (
    <nav
      aria-label={t("dayNavigation")}
      className="flex min-h-12 items-center justify-between gap-2 border-b px-2 pb-3"
    >
      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("previousDay")}
        onClick={() => onChange(shiftDay(day, -1))}
      >
        ←
      </Button>

      <div className="min-w-0 text-center">
        <div className="flex items-center justify-center gap-2">
          {live && (
            <span className="inline-flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-amber-400">
              <span className="h-2 w-2 rounded-full bg-amber-400 shadow-[0_0_10px_rgba(251,191,36,0.7)] motion-safe:animate-pulse" />
              {t("live")}
            </span>
          )}
          <span className="truncate text-sm font-medium">
            {live ? t("today") : label}
          </span>
        </div>
        {live && <div className="truncate text-xs text-muted-foreground">{label}</div>}
        {!live && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onChange(today)}
          >
            {t("backToToday")}
          </Button>
        )}
      </div>

      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("nextDay")}
        disabled={day >= today}
        onClick={() => onChange(shiftDay(day, 1))}
      >
        →
      </Button>
    </nav>
  );
}

function shiftDay(day: string, amount: number): string {
  return shiftIsoDay(day, amount);
}

function WeekNavigator({
  weekStart,
  currentWeekStart,
  onChange,
}: {
  weekStart: string;
  currentWeekStart: string;
  onChange: (week: string) => void;
}) {
  const { locale, t } = useI18n();
  const current = weekStart === currentWeekStart;
  const weekEnd = shiftDay(weekStart, 6);
  const formatDate = (value: string) =>
    formatIsoDate(
      value,
      {
        day: "2-digit",
        month: "short",
        year: "numeric",
      },
      locale,
    );
  const label = `${formatDate(weekStart)} – ${formatDate(weekEnd)}`;

  return (
    <nav
      aria-label={t("weekNavigation")}
      className="flex min-h-12 items-center justify-between gap-2 border-b px-2 pb-3"
    >
      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("previousWeek")}
        onClick={() => onChange(shiftDay(weekStart, -7))}
      >
        ←
      </Button>

      <div className="flex min-w-0 flex-col items-center gap-1 text-center">
        <span className="truncate text-sm font-medium">{label}</span>
        {!current && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onChange(currentWeekStart)}
          >
            {t("backToCurrentWeek")}
          </Button>
        )}
      </div>

      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("nextWeek")}
        disabled={weekStart >= currentWeekStart}
        onClick={() => onChange(shiftDay(weekStart, 7))}
      >
        →
      </Button>
    </nav>
  );
}

function MonthNavigator({
  month,
  currentMonth,
  onChange,
}: {
  month: string;
  currentMonth: string;
  onChange: (month: string) => void;
}) {
  const { locale, t } = useI18n();
  const current = month === currentMonth;
  const label = formatIsoMonth(month, {
    month: "long",
    year: "numeric",
  }, locale);

  return (
    <nav
      aria-label={t("monthNavigation")}
      className="flex min-h-12 items-center justify-between gap-2 border-b px-2 pb-3"
    >
      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("previousMonth")}
        onClick={() => onChange(shiftMonth(month, -1))}
      >
        ←
      </Button>

      <div className="flex min-w-0 flex-col items-center gap-1 text-center">
        <span className="truncate text-sm font-medium">{label}</span>
        {!current && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onChange(currentMonth)}
          >
            {t("backToCurrentMonth")}
          </Button>
        )}
      </div>

      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("nextMonth")}
        disabled={month >= currentMonth}
        onClick={() => onChange(shiftMonth(month, 1))}
      >
        →
      </Button>
    </nav>
  );
}

function shiftMonth(month: string, amount: number): string {
  return shiftIsoMonth(month, amount);
}

function YearNavigator({
  year,
  currentYear,
  onChange,
}: {
  year: string;
  currentYear: string;
  onChange: (year: string) => void;
}) {
  const { t } = useI18n();
  const current = year === currentYear;

  return (
    <nav
      aria-label={t("yearNavigation")}
      className="flex min-h-12 items-center justify-between gap-2 border-b px-2 pb-3"
    >
      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("previousYear")}
        onClick={() => onChange(shiftYear(year, -1))}
      >
        ←
      </Button>

      <div className="flex min-w-0 flex-col items-center gap-1 text-center">
        <span className="truncate text-sm font-medium">{year}</span>
        {!current && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onChange(currentYear)}
          >
            {t("backToCurrentYear")}
          </Button>
        )}
      </div>

      <Button
        type="button"
        variant="outline"
        size="icon"
        aria-label={t("nextYear")}
        disabled={year >= currentYear}
        onClick={() => onChange(shiftYear(year, 1))}
      >
        →
      </Button>
    </nav>
  );
}

function shiftYear(year: string, amount: number): string {
  return String(Number(year) + amount);
}
