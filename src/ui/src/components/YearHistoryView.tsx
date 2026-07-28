import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";
import type { History, YearSummary } from "@/lib/api";
import { formatIsoMonth } from "@/lib/date";
import { formatEnergyKilowattHours, formatNumber } from "@/lib/format";
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
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Table,
  TableBody,
  TableCaption,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { HistoryMetricCard } from "@/components/MonthHistoryView";
import { useI18n } from "@/lib/i18n";

export function YearHistoryView({ history }: { history: History }) {
  const { locale, t } = useI18n();
  const chartConfig = {
    energy: {
      label: t("energyGenerated"),
      color: "hsl(217 91% 60%)",
    },
  } satisfies ChartConfig;
  const key = history.keys[0];
  const observedRows = history.rows.map((row) => {
    const month = String(row.label);
    return {
      month,
      label: formatCalendarMonth(month, "short", locale),
      energy: Number(row[key]),
    };
  });
  const rows = completeYearRows(
    history.year ?? observedRows[0]?.month.slice(0, 4),
    observedRows,
    locale,
  );
  const summary =
    (history.summary as YearSummary | undefined) ??
    calculateSummary(observedRows);
  const change = summary.changePercent;

  return (
    <div className="flex flex-col gap-4">
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <HistoryMetricCard
          title={t("energyThisYear")}
          value={formatEnergyKilowattHours(summary.total, locale)}
        />
        <HistoryMetricCard
          title={t("averageMonthly")}
          value={formatEnergyKilowattHours(summary.averageMonthly, locale)}
          detail={t("recordedMonths", { count: summary.recordedMonths })}
        />
        <HistoryMetricCard
          title={t("bestMonth")}
          value={
            summary.bestMonth
              ? formatEnergyKilowattHours(summary.bestMonth.value, locale)
              : "—"
          }
          detail={
            summary.bestMonth
              ? formatCalendarMonth(summary.bestMonth.label, "long", locale)
              : undefined
          }
        />
        <HistoryMetricCard
          title={t("vsLastYear")}
          value={
            change === null
              ? "—"
              : `${change >= 0 ? "▲" : "▼"} ${formatNumber(Math.abs(change), locale)}%`
          }
          detail={
            summary.previousYearTotal > 0
              ? t("previous", {
                  value: formatEnergyKilowattHours(
                    summary.previousYearTotal,
                    locale,
                  ),
                })
              : t("noPreviousYearData")
          }
          negative={change !== null && change < 0}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("monthlyEnergyThisYear")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ChartContainer
            config={chartConfig}
            className="h-[340px] w-full aspect-auto"
          >
            <BarChart accessibilityLayer data={rows}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="label"
                axisLine={false}
                tickLine={false}
                tickMargin={10}
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
                    labelFormatter={(_, payload) =>
                      payload?.[0]?.payload?.month
                        ? formatCalendarMonth(
                            payload[0].payload.month,
                            "long",
                            locale,
                          )
                        : ""
                    }
                    formatter={(value) => (
                      <span className="font-mono font-medium tabular-nums">
                        {formatEnergyKilowattHours(Number(value), locale)}
                      </span>
                    )}
                  />
                }
              />
              <Bar
                dataKey="energy"
                fill="var(--color-energy)"
                radius={[4, 4, 0, 0]}
                minPointSize={2}
                isAnimationActive={false}
              />
            </BarChart>
          </ChartContainer>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("monthlyValues")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ScrollArea className="h-80 rounded-md border">
            <Table>
              <TableCaption className="sr-only">
                {t("monthlyYearCaption")}
              </TableCaption>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("month")}</TableHead>
                  <TableHead className="text-right">
                    {t("generated")}
                  </TableHead>
                  <TableHead className="text-right">
                    {t("shareOfYear")}
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {[...rows].reverse().map((row) => (
                  <TableRow key={row.month}>
                    <TableCell className="font-medium">
                      {formatCalendarMonth(row.month, "long", locale)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {formatEnergyKilowattHours(row.energy, locale)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {summary.total > 0
                        ? `${formatNumber((row.energy / summary.total) * 100, locale)}%`
                        : "—"}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </ScrollArea>
        </CardContent>
      </Card>
    </div>
  );
}

function calculateSummary(
  rows: Array<{ month: string; energy: number }>,
): YearSummary {
  const total = rows.reduce((sum, row) => sum + row.energy, 0);
  const best = rows.reduce<(typeof rows)[number] | null>(
    (current, row) => (!current || row.energy > current.energy ? row : current),
    null,
  );
  return {
    total,
    averageMonthly: rows.length > 0 ? total / rows.length : 0,
    recordedMonths: rows.length,
    bestMonth: best ? { label: best.month, value: best.energy } : null,
    previousYearTotal: 0,
    changePercent: null,
  };
}

function completeYearRows(
  year: string | undefined,
  rows: Array<{ month: string; label: string; energy: number }>,
  locale: string,
): Array<{ month: string; label: string; energy: number }> {
  if (!year || !/^\d{4}$/.test(year)) return rows;

  const energyByMonth = new Map(
    rows.map((row) => [row.month, Number.isFinite(row.energy) ? row.energy : 0]),
  );
  return Array.from({ length: 12 }, (_, index) => {
    const month = `${year}-${String(index + 1).padStart(2, "0")}`;
    return {
      month,
      label: formatCalendarMonth(month, "short", locale),
      energy: energyByMonth.get(month) ?? 0,
    };
  });
}

function formatCalendarMonth(
  value: string,
  month: "short" | "long",
  locale: string,
): string {
  return formatIsoMonth(value, {
    month,
    year: month === "long" ? "numeric" : undefined,
  }, locale);
}
