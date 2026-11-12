// Owns the sidebar's UTC/local time footer; it does not own deadline rules.
import { useSyncExternalStore } from "react";
import { formatSidebarClock } from "./sidebar-clock-format";
import {
  getSidebarMinuteSnapshot,
  subscribeSidebarMinuteClock,
} from "./sidebar-minute-clock";
import "./SidebarClockBar.css";

export function SidebarClockBar() {
  const timestamp = useSyncExternalStore(
    subscribeSidebarMinuteClock,
    getSidebarMinuteSnapshot,
    getSidebarMinuteSnapshot,
  );
  const { utc, local, zone, dateTime } = formatSidebarClock(timestamp);

  return (
    <footer className="sidebar-clock-bar">
      <time
        dateTime={dateTime}
        aria-label={`UTC time ${utc}; local time ${local} ${zone}`}
      >
        <span>UTC {utc}</span>
        <span>Local {local} {zone}</span>
      </time>
    </footer>
  );
}
