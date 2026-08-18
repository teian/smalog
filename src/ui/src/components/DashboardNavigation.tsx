import {
  Activity,
  Bell,
  ChartNoAxesCombined,
  RadioTower,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export type DashboardSection =
  | "energy"
  | "events"
  | "device"
  | "grid"
  | "system";

const ITEMS: Array<{
  id: DashboardSection;
  label:
    | "energyData"
    | "events"
    | "deviceStatistics"
    | "gridQuality"
    | "system";
  icon: LucideIcon;
}> = [
  { id: "energy", label: "energyData", icon: ChartNoAxesCombined },
  { id: "events", label: "events", icon: Bell },
  { id: "device", label: "deviceStatistics", icon: Activity },
  { id: "grid", label: "gridQuality", icon: RadioTower },
  { id: "system", label: "system", icon: Terminal },
];

export function DashboardNavigation({
  active,
  onChange,
}: {
  active: DashboardSection;
  onChange: (section: DashboardSection) => void;
}) {
  const { t } = useI18n();

  const navigation = (
    <nav aria-label={t("dataNavigation")}>
      <div className="grid grid-cols-2 gap-2 lg:flex lg:flex-col">
        {ITEMS.map(({ id, label, icon: Icon }) => (
          <Button
            key={id}
            type="button"
            variant={active === id ? "default" : "ghost"}
            aria-current={active === id ? "page" : undefined}
            onClick={() => onChange(id)}
            className={cn(
              "h-auto min-h-12 justify-start whitespace-normal px-3 py-2 text-left",
              active !== id && "text-muted-foreground",
            )}
          >
            <Icon data-icon="inline-start" aria-hidden="true" />
            <span>{t(label)}</span>
          </Button>
        ))}
      </div>
    </nav>
  );

  return (
    <>
      <div className="lg:hidden">
        <p className="mb-2 text-xs font-medium uppercase tracking-wider text-muted-foreground">
          {t("dataAreas")}
        </p>
        {navigation}
      </div>
      <aside className="hidden lg:block">
        <div className="sticky top-6 rounded-lg border bg-card p-3 shadow-sm">
          <p className="mb-3 px-3 text-xs font-medium uppercase tracking-wider text-muted-foreground">
            {t("dataAreas")}
          </p>
          {navigation}
        </div>
      </aside>
    </>
  );
}
