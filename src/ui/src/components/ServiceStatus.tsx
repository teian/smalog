import { CircleAlert } from "lucide-react";
import type { Status } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverDescription,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "@/components/ui/popover";
import { useI18n } from "@/lib/i18n";

export function ServiceStatus({ status }: { status: Status | null }) {
  const { t } = useI18n();

  if (!status) {
    return <Badge variant="destructive">{t("offline")}</Badge>;
  }

  return (
    <div className="flex min-w-0 items-center gap-1">
      <Badge variant="outline">v{status.version}</Badge>
      {status.lastError && (
        <Popover>
          <PopoverTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              aria-label={t("showErrorDetails")}
              className="px-1 sm:max-w-52"
            >
              <Badge variant="destructive">
                <CircleAlert data-icon="inline-start" aria-hidden="true" />
                <span className="hidden truncate sm:inline">
                  {t("connectionFailed")}
                </span>
              </Badge>
            </Button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-80 max-w-[calc(100vw-2rem)]">
            <PopoverHeader>
              <PopoverTitle>{t("connectionError")}</PopoverTitle>
              <PopoverDescription>{t("pollContinues")}</PopoverDescription>
            </PopoverHeader>
            <code className="block max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs">
              {deduplicateError(status.lastError)}
            </code>
          </PopoverContent>
        </Popover>
      )}
    </div>
  );
}

function deduplicateError(error: string): string {
  return [
    ...new Set(
      error
        .split(";")
        .map((part) => part.trim())
        .filter(Boolean),
    ),
  ].join(";\n");
}
