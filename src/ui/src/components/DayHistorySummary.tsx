import type { DaySummary, History, HistorySeries } from "@/lib/api";
import {
  formatEnergyKilowattHours,
  formatNumber,
  formatPowerKilowatts,
} from "@/lib/format";
import { aggregateHistorySeries } from "@/lib/history";
import { HistoryMetricCard } from "@/components/MonthHistoryView";
import { useI18n } from "@/lib/i18n";

export function DayHistorySummary({
  history,
  live,
}: {
  history: History;
  live: boolean;
}) {
  const { locale, t } = useI18n();
  const summary =
    (history.summary as DaySummary | undefined) ??
    calculateDaySummary(history);
  const change = summary.changePercent;

  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      <HistoryMetricCard
        title={live ? t("todaysEnergy") : t("energyThisDay")}
        value={formatEnergyKilowattHours(summary.totalEnergy, locale)}
      />
      <HistoryMetricCard
        title={t("peakPower")}
        value={formatPowerKilowatts(summary.peakPower, locale)}
      />
      <HistoryMetricCard
        title={t("averagePower")}
        value={formatPowerKilowatts(summary.averagePower, locale)}
        detail={t("recordedIntervals", {
          count: summary.recordedIntervals,
        })}
      />
      <HistoryMetricCard
        title={live ? t("vsYesterday") : t("vsPreviousDay")}
        value={
          change === null
            ? "—"
            : `${change >= 0 ? "▲" : "▼"} ${formatNumber(Math.abs(change), locale)}%`
        }
        detail={
          summary.previousDayEnergy > 0
            ? t("previous", {
                value: formatEnergyKilowattHours(
                  summary.previousDayEnergy,
                  locale,
                ),
              })
            : t("noPreviousDayData")
        }
        negative={change !== null && change < 0}
      />
    </div>
  );
}

export function calculateDaySummary(history: History): DaySummary {
  const series = aggregateHistorySeries(
    history.series ?? inferSeries(history),
  );
  const powerKey = series.find((item) => item.metric === "power")?.key;
  const energyKey = series.find((item) => item.metric === "energy")?.key;
  const powerValues = history.rows
    .map((row) => numericValue(powerKey ? row[powerKey] : undefined))
    .filter((value): value is number => value !== null);
  const energyValues = history.rows
    .map((row) => numericValue(energyKey ? row[energyKey] : undefined))
    .filter((value): value is number => value !== null);

  return {
    totalEnergy: energyValues.length > 0 ? Math.max(...energyValues) : 0,
    peakPower: powerValues.length > 0 ? Math.max(...powerValues) : 0,
    averagePower:
      powerValues.length > 0
        ? powerValues.reduce((sum, value) => sum + value, 0) /
          powerValues.length
        : 0,
    recordedIntervals: powerValues.length,
    previousDayEnergy: 0,
    changePercent: null,
  };
}

function inferSeries(history: History): HistorySeries[] {
  return history.keys.map((key) => ({
    key,
    label: key,
    metric: key.toLowerCase().includes("energy") ? "energy" : "power",
  }));
}

function numericValue(value: string | number | undefined): number | null {
  const numeric = Number(value);
  return value === undefined || !Number.isFinite(numeric) ? null : numeric;
}
