const UNIT_SEPARATOR = "\u00a0";
const POWER_UNITS = ["W", "KW", "MW", "GW", "TW", "PW"] as const;
const ENERGY_UNITS = ["Wh", "KWh", "MWh", "GWh", "TWh", "PWh"] as const;

export function formatNumber(value: number, locale?: string): string {
  return new Intl.NumberFormat(locale, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(value);
}

export function formatPowerKilowatts(
  kilowatts: number,
  locale?: string,
): string {
  return formatPowerWatts(kilowatts * 1000, locale);
}

export function formatPowerWatts(watts: number, locale?: string): string {
  return formatScaled(watts, POWER_UNITS, locale);
}

export function formatEnergyKilowattHours(
  kilowattHours: number,
  locale?: string,
): string {
  return formatEnergyWattHours(kilowattHours * 1000, locale);
}

export function formatEnergyWattHours(
  wattHours: number,
  locale?: string,
): string {
  return formatScaled(wattHours, ENERGY_UNITS, locale);
}

function formatScaled(
  baseValue: number,
  units: readonly string[],
  locale?: string,
): string {
  let value = baseValue;
  let unitIndex = 0;

  while (
    Number.isFinite(value) &&
    Math.abs(value) >= 999.995 &&
    unitIndex < units.length - 1
  ) {
    value /= 1000;
    unitIndex += 1;
  }

  return `${formatNumber(value, locale)}${UNIT_SEPARATOR}${units[unitIndex]}`;
}
