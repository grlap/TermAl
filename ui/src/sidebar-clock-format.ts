// Owns UTC/local clock labels. Scheduling and panel layout live separately.
export function formatSidebarClock(timestamp: number) {
  const date = new Date(timestamp);
  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const options: Intl.DateTimeFormatOptions = {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  };
  const utc = new Intl.DateTimeFormat("en-US", {
    ...options,
    timeZone: "UTC",
  }).format(date);
  const parts = new Intl.DateTimeFormat("en-US", {
    ...options,
    timeZone: localZone,
    timeZoneName: "short",
  }).formatToParts(date);
  const part = (type: Intl.DateTimeFormatPartTypes) =>
    parts.find((entry) => entry.type === type)?.value ?? "";

  return {
    utc,
    local: `${part("hour")}:${part("minute")}`,
    zone: part("timeZoneName"),
    dateTime: date.toISOString(),
  };
}
