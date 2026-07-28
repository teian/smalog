import { useMemo } from "react";
import type {
  History,
  HistoryMetric,
  HistorySeries,
} from "@/lib/api";
import {
  formatEnergyKilowattHours,
  formatNumber,
  formatPowerKilowatts,
} from "@/lib/format";
import { aggregateHistorySeries } from "@/lib/history";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea, ScrollBar } from "@/components/ui/scroll-area";
import {
  Table,
  TableBody,
  TableCaption,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { localizeSeriesLabel, useI18n } from "@/lib/i18n";

interface InverterSummary {
  id: string;
  label: string;
  aggregate: boolean;
  powerKey?: string;
  energyKey?: string;
  temperatureKey?: string;
}

export function DayHistoryTables({
  history,
  selectedLabel,
  aggregateOnlyValues,
}: {
  history: History;
  selectedLabel: string;
  aggregateOnlyValues: boolean;
}) {
  const { locale, t } = useI18n();
  const series = useMemo(
    () => history.series ?? inferSeries(history),
    [history],
  );
  const valueSeries = useMemo(() => {
    if (!aggregateOnlyValues) return series;
    return aggregateHistorySeries(series);
  }, [aggregateOnlyValues, series]);
  const summaries = useMemo(
    () => groupSeries(series, selectedLabel),
    [series, selectedLabel],
  );
  const rows = useMemo(() => [...history.rows].reverse(), [history.rows]);
  const showTemperature = summaries.some((item) => item.temperatureKey);

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader>
          <CardTitle>{t("dayTotals")}</CardTitle>
        </CardHeader>
        <CardContent>
          <Table>
            <TableCaption className="sr-only">
              {t("dayTotalsCaption")}
            </TableCaption>
            <TableHeader>
              <TableRow>
                <TableHead>{t("inverter")}</TableHead>
                <TableHead className="text-right">{t("peakPower")}</TableHead>
                <TableHead className="text-right">
                  {t("energyGenerated")}
                </TableHead>
                {showTemperature && (
                  <TableHead className="text-right">
                    {t("lastTemperature")}
                  </TableHead>
                )}
              </TableRow>
            </TableHeader>
            <TableBody>
              {summaries.map((summary) => (
                <TableRow key={summary.id}>
                  <TableCell className="font-medium">
                    {summary.aggregate ? t("total") : summary.label}
                  </TableCell>
                  <MetricCell
                    metric="power"
                    value={maximumValue(history, summary.powerKey)}
                    locale={locale}
                  />
                  <MetricCell
                    metric="energy"
                    value={maximumValue(history, summary.energyKey)}
                    locale={locale}
                  />
                  {showTemperature && (
                    <MetricCell
                      metric="temperature"
                      value={latestValue(history, summary.temperatureKey)}
                      locale={locale}
                    />
                  )}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("fiveMinuteValues")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ScrollArea className="h-96 rounded-md border">
            <Table>
              <TableCaption className="sr-only">
                {t("fiveMinuteCaption")}
              </TableCaption>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("time")}</TableHead>
                  {valueSeries.map((item) => (
                    <TableHead key={item.key} className="text-right">
                      {localizeSeriesLabel(
                        item.label,
                        item.metric,
                        item.aggregate === true,
                        t,
                      )}
                    </TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((row, index) => (
                  <TableRow key={`${row.label}-${index}`}>
                    <TableCell className="font-mono font-medium tabular-nums">
                      {row.label}
                    </TableCell>
                    {valueSeries.map((item) => (
                      <MetricCell
                        key={item.key}
                        metric={item.metric}
                        value={numericValue(row[item.key])}
                        locale={locale}
                      />
                    ))}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            <ScrollBar orientation="horizontal" />
          </ScrollArea>
        </CardContent>
      </Card>
    </div>
  );
}

function MetricCell({
  metric,
  value,
  locale,
}: {
  metric: HistoryMetric;
  value: number | null;
  locale: string;
}) {
  return (
    <TableCell className="text-right font-mono tabular-nums">
      {value === null ? "—" : formatMetric(metric, value, locale)}
    </TableCell>
  );
}

function groupSeries(
  series: HistorySeries[],
  selectedLabel: string,
): InverterSummary[] {
  const summaries = new Map<string, InverterSummary>();

  for (const item of series) {
    const suffix = `_${item.metric}`;
    const id = item.key.endsWith(suffix)
      ? item.key.slice(0, -suffix.length)
      : "selected";
    const label = item.label.replace(
      /\s+(Power|Energy(?: Generated)?|Temperature)$/,
      "",
    );
    const summary = summaries.get(id) ?? {
      id,
      label: label === item.label ? selectedLabel : label,
      aggregate: item.aggregate === true,
    };

    summary.aggregate ||= item.aggregate === true;
    if (item.metric === "power") summary.powerKey = item.key;
    if (item.metric === "energy") summary.energyKey = item.key;
    if (item.metric === "temperature") summary.temperatureKey = item.key;
    summaries.set(id, summary);
  }

  return [...summaries.values()].sort(
    (left, right) => Number(right.aggregate) - Number(left.aggregate),
  );
}

function inferSeries(history: History): HistorySeries[] {
  return history.keys.map((key) => ({
    key,
    label: key,
    metric: inferMetric(key),
  }));
}

function inferMetric(key: string): HistoryMetric {
  if (key.toLowerCase().includes("energy")) return "energy";
  if (key.toLowerCase().includes("temperature")) return "temperature";
  return "power";
}

function maximumValue(history: History, key?: string): number | null {
  if (!key) return null;
  const values = history.rows
    .map((row) => numericValue(row[key]))
    .filter((value): value is number => value !== null);
  return values.length > 0 ? Math.max(...values) : null;
}

function latestValue(history: History, key?: string): number | null {
  if (!key) return null;
  for (let index = history.rows.length - 1; index >= 0; index -= 1) {
    const value = numericValue(history.rows[index][key]);
    if (value !== null) return value;
  }
  return null;
}

function numericValue(value: string | number | undefined): number | null {
  const numeric = Number(value);
  return value === undefined || !Number.isFinite(numeric) ? null : numeric;
}

function formatMetric(
  metric: HistoryMetric,
  value: number,
  locale: string,
): string {
  if (metric === "power") return formatPowerKilowatts(value, locale);
  if (metric === "energy") return formatEnergyKilowattHours(value, locale);
  return `${formatNumber(value, locale)}\u00a0°C`;
}
