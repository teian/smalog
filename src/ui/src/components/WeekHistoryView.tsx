import {
  Bar,
  BarChart,
  CartesianGrid,
  ReferenceLine,
  XAxis,
  YAxis,
} from "recharts";
import type { History, WeekSummary } from "@/lib/api";
import { formatIsoDate } from "@/lib/date";
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

export function WeekHistoryView({ history }: { history: History }) {
  const { locale, t } = useI18n();
  const chartConfig = {
    energy: {
      label: t("energyGenerated"),
      color: "hsl(217 91% 60%)",
    },
    average: {
      label: t("averageDaily"),
      color: "hsl(25 95% 53%)",
    },
  } satisfies ChartConfig;
  const key = history.keys[0];
  const rows = history.rows.map((row) => {
    const date = String(row.label);
    return {
      date,
      day: formatIsoDate(date, { weekday: "short" }, locale),
      energy: Number(row[key]),
    };
  });
  const summary =
    (history.summary as WeekSummary | undefined) ?? calculateSummary(rows);
  const change = summary.changePercent;

  return (
    <div className="flex flex-col gap-4">
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <HistoryMetricCard
          title={t("energyThisWeek")}
          value={formatEnergyKilowattHours(summary.total, locale)}
        />
        <HistoryMetricCard
          title={t("averageDaily")}
          value={formatEnergyKilowattHours(summary.averageDaily, locale)}
          detail={t("recordedDays", { count: summary.recordedDays })}
        />
        <HistoryMetricCard
          title={t("bestDay")}
          value={
            summary.bestDay
              ? formatEnergyKilowattHours(summary.bestDay.value, locale)
              : "—"
          }
          detail={
            summary.bestDay
              ? formatIsoDate(summary.bestDay.label, {
                  weekday: "short",
                  day: "2-digit",
                  month: "short",
                  year: "numeric",
                }, locale)
              : undefined
          }
        />
        <HistoryMetricCard
          title={t("vsLastWeek")}
          value={
            change === null
              ? "—"
              : `${change >= 0 ? "▲" : "▼"} ${formatNumber(Math.abs(change), locale)}%`
          }
          detail={
            summary.previousWeekTotal > 0
              ? t("previous", {
                  value: formatEnergyKilowattHours(
                    summary.previousWeekTotal,
                    locale,
                  ),
                })
              : t("noPreviousWeekData")
          }
          negative={change !== null && change < 0}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("dailyEnergyThisWeek")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ChartContainer
            config={chartConfig}
            className="h-[340px] w-full aspect-auto"
          >
            <BarChart accessibilityLayer data={rows}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="day"
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
                      payload?.[0]?.payload?.date
                        ? formatIsoDate(payload[0].payload.date, {
                            weekday: "short",
                            day: "2-digit",
                            month: "short",
                            year: "numeric",
                          }, locale)
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
              {Number.isFinite(summary.averageDaily) &&
                summary.averageDaily > 0 && (
                  <ReferenceLine
                    y={summary.averageDaily}
                    stroke="var(--color-average)"
                    strokeWidth={1.5}
                    strokeDasharray="6 4"
                    label={{
                      value: t("averageReference", {
                        value: formatEnergyKilowattHours(
                          summary.averageDaily,
                          locale,
                        ),
                      }),
                      position: "insideTopRight",
                      fill: "hsl(var(--muted-foreground))",
                      fontSize: 12,
                    }}
                  />
                )}
              <Bar
                dataKey="energy"
                fill="var(--color-energy)"
                radius={[4, 4, 0, 0]}
                isAnimationActive={false}
              />
            </BarChart>
          </ChartContainer>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("dailyValues")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ScrollArea className="h-80 rounded-md border">
            <Table>
              <TableCaption className="sr-only">
                {t("dailyWeekCaption")}
              </TableCaption>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("day")}</TableHead>
                  <TableHead className="text-right">
                    {t("generated")}
                  </TableHead>
                  <TableHead className="text-right">
                    {t("shareOfWeek")}
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {[...rows].reverse().map((row) => (
                  <TableRow key={row.date}>
                    <TableCell className="font-medium">
                      {formatIsoDate(row.date, {
                        weekday: "short",
                        day: "2-digit",
                        month: "short",
                        year: "numeric",
                      }, locale)}
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
  rows: Array<{ date: string; energy: number }>,
): WeekSummary {
  const total = rows.reduce((sum, row) => sum + row.energy, 0);
  const best = rows.reduce<(typeof rows)[number] | null>(
    (current, row) => (!current || row.energy > current.energy ? row : current),
    null,
  );
  return {
    total,
    averageDaily: rows.length > 0 ? total / rows.length : 0,
    recordedDays: rows.length,
    bestDay: best ? { label: best.date, value: best.energy } : null,
    previousWeekTotal: 0,
    changePercent: null,
  };
}
