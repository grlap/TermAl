// New Work timestamp presentation. Owns the one way the Work panel prints a
// source timestamp: to the minute, in the viewer's local time, with the raw
// value kept on the element. Does not interpret source semantics or order
// anything — sorting stays on the raw values in work-sort.ts.
import type { ReactNode } from "react";

// Sources carry fractions up to nanoseconds; Date parsing is only defined up
// to milliseconds, so the fraction is cut before parsing on every engine.
function millisecondPrecision(value: string): string {
  return value.replace(/(\.\d{3})\d+/, "$1");
}

// `YYYY-MM-DD HH:mm` in `timeZone` (the viewer's zone when omitted). A value
// that is not a timestamp comes back unchanged, so nothing is invented.
export function formatWorkTime(value: string, timeZone?: string): string {
  const time = Date.parse(millisecondPrecision(value));
  if (Number.isNaN(time)) return value;
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone, year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23",
  }).formatToParts(new Date(time));
  const part = (type: Intl.DateTimeFormatPartTypes) => parts.find(entry => entry.type === type)?.value ?? "";
  return `${part("year")}-${part("month")}-${part("day")} ${part("hour")}:${part("minute")}`;
}

// The full source value stays readable on hover and machine-readable in
// `dateTime`; a value that is not a timestamp is shown as the source sent it.
export function WorkTime({ value }: { value: string }): ReactNode {
  const shown = formatWorkTime(value);
  if (shown === value) return value;
  return <time dateTime={millisecondPrecision(value)} title={value}>{shown}</time>;
}
