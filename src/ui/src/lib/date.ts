/** Strict ISO calendar parsing and formatting for API values.
 * Invalid or legacy labels are returned unchanged instead of crashing the UI. */
export function formatIsoDate(
  value: string,
  options: Intl.DateTimeFormatOptions,
  locale?: string,
): string {
  const date = parseIsoDate(value);
  return date
    ? new Intl.DateTimeFormat(locale, {
        ...options,
        timeZone: "UTC",
      }).format(date)
    : value || "—";
}

export function formatIsoMonth(
  value: string,
  options: Intl.DateTimeFormatOptions,
  locale?: string,
): string {
  const date = parseIsoMonth(value);
  return date
    ? new Intl.DateTimeFormat(locale, {
        ...options,
        timeZone: "UTC",
      }).format(date)
    : value || "—";
}

export function shiftIsoDay(value: string, amount: number): string {
  const date = parseIsoDate(value);
  if (!date) return value;
  date.setUTCDate(date.getUTCDate() + amount);
  return date.toISOString().slice(0, 10);
}

export function shiftIsoMonth(value: string, amount: number): string {
  const date = parseIsoMonth(value);
  if (!date) return value;
  date.setUTCMonth(date.getUTCMonth() + amount);
  return date.toISOString().slice(0, 7);
}

function parseIsoDate(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return null;
  return createUtcDate(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
}

function parseIsoMonth(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})$/.exec(value);
  if (!match) return null;
  return createUtcDate(Number(match[1]), Number(match[2]) - 1, 1);
}

function createUtcDate(year: number, month: number, day: number): Date | null {
  const date = new Date(0);
  date.setUTCHours(0, 0, 0, 0);
  date.setUTCFullYear(year, month, day);
  return date.getUTCFullYear() === year &&
    date.getUTCMonth() === month &&
    date.getUTCDate() === day
    ? date
    : null;
}
