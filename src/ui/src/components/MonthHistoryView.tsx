import {
  Bar,
  BarChart,
  CartesianGrid,
  ReferenceLine,
  XAxis,
  YAxis,
} from "recharts";
import type { History, MonthSummary } from "@/lib/api";
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
import { cn } from "@/lib/utils";
import { useI18n } from "@/lib/i18n";

export function MonthHistoryView({ history }: { history: History }) {
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
  const rows = history.rows.map((row) => ({
    date: String(row.label),
    day: String(Number(String(row.label).slice(-2))),
    energy: Number(row[key]),
  }));
  const summary =
    (history.summary as MonthSummary | undefined) ?? calculateSummary(rows);
  const change = summary.changePercent;

  return (
    <div className="flex flex-col gap-4">
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <HistoryMetricCard
          title={t("energyThisMonth")}
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
              ? formatCalendarDate(summary.bestDay.label, locale)
              : undefined
          }
        />
        <HistoryMetricCard
          title={t("vsLastMonth")}
          value={
            change === null
              ? "—"
              : `${change >= 0 ? "▲" : "▼"} ${formatNumber(Math.abs(change), locale)}%`
          }
          detail={
            summary.previousMonthTotal > 0
              ? t("previous", {
                  value: formatEnergyKilowattHours(
                    summary.previousMonthTotal,
                    locale,
                  ),
                })
              : t("noPreviousMonthData")
          }
          negative={change !== null && change < 0}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("dailyEnergyThisMonth")}</CardTitle>
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
                interval="preserveStartEnd"
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
                        ? formatCalendarDate(payload[0].payload.date, locale)
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
                {t("dailyMonthCaption")}
              </TableCaption>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("day")}</TableHead>
                  <TableHead className="text-right">
                    {t("generated")}
                  </TableHead>
                  <TableHead className="text-right">
                    {t("shareOfMonth")}
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {[...rows].reverse().map((row) => (
                  <TableRow key={row.date}>
                    <TableCell className="font-medium">
                      {formatCalendarDate(row.date, locale)}
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

export function HistoryMetricCard({
  title,
  value,
  detail,
  negative = false,
}: {
  title: string;
  value: string;
  detail?: string;
  negative?: boolean;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent>
        <p
          className={cn(
            "text-2xl font-semibold tabular-nums",
            negative ? "text-destructive" : "text-foreground",
          )}
        >
          {value}
        </p>
        {detail && (
          <p className="mt-1 text-xs text-muted-foreground">{detail}</p>
        )}
      </CardContent>
    </Card>
  );
}

function calculateSummary(
  rows: Array<{ date: string; energy: number }>,
): MonthSummary {
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
    previousMonthTotal: 0,
    changePercent: null,
  };
}

function formatCalendarDate(value: string, locale: string): string {
  return formatIsoDate(value, {
    day: "2-digit",
    month: "short",
    year: "numeric",
  }, locale);
}
