import type { Status } from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  formatEnergyWattHours,
  formatPowerWatts,
} from "@/lib/format";
import { useI18n } from "@/lib/i18n";

/** Live per-inverter tiles: current power, today's and lifetime yield. */
export function StatusCards({ status }: { status: Status | null }) {
  const { locale, t } = useI18n();

  if (!status) {
    return <p className="text-muted-foreground">{t("connecting")}</p>;
  }
  if (status.inverters.length === 0) {
    return <p className="text-muted-foreground">{t("noInverterData")}</p>;
  }

  const totalNow = status.inverters.reduce((sum, i) => sum + i.totalPac, 0);
  const totalToday = status.inverters.reduce((sum, i) => sum + i.eToday, 0);

  return (
    <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
      <Card>
        <CardHeader>
          <CardTitle>{t("currentPower")}</CardTitle>
        </CardHeader>
        <CardContent>
          <span className="text-2xl font-semibold">
            {formatPowerWatts(totalNow, locale)}
          </span>
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{t("yieldToday")}</CardTitle>
        </CardHeader>
        <CardContent>
          <span className="text-2xl font-semibold">
            {formatEnergyWattHours(totalToday, locale)}
          </span>
        </CardContent>
      </Card>
      {status.inverters.map((inverter) => (
        <Card key={inverter.serial}>
          <CardHeader>
            <CardTitle>{inverter.name || `#${inverter.serial}`}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold">
              {formatPowerWatts(inverter.totalPac, locale)}
            </div>
            <div className="text-xs text-muted-foreground">
              {formatEnergyWattHours(inverter.eToday, locale)} {t("today")} ·{" "}
              {inverter.status}
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}
