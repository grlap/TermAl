// The Working sessions STATUS chip's label and its spelled-out description.
// The chip shows the foreground/background split inside itself: the full form
// "· F foreground / B background", or "(F/B)" when the Status section is
// narrower than the threshold a container query in styles.css sets. The split
// is visual only (aria-hidden), so the button's accessible name stays
// "Working sessions (N)". The description element, rendered next to the button
// and referenced by its aria-describedby, spells the split out for assistive
// technology. It is `hidden`, so it is read once, as the button's description,
// and not again in reading order. The compact form carries the same text as its
// tooltip.
// Does not own: the counts or what they mean (session-list-filter.ts), the
// filter button and its click handling, or its title (AppControlSurface.tsx).
// New file; not split from an existing one.
import type { SessionWorkingBreakdown } from "./session-list-filter";

export function workingBreakdownText(breakdown: SessionWorkingBreakdown): string {
  return `${breakdown.foreground} foreground / ${breakdown.background} background`;
}

export function workingBreakdownDescription(breakdown: SessionWorkingBreakdown): string {
  return `${workingBreakdownText(breakdown)}. A session with a foreground turn and background work counts in both.`;
}

export function WorkingSessionsChipLabel({
  count,
  breakdown,
}: {
  count: number;
  breakdown: SessionWorkingBreakdown;
}) {
  const label = `Working sessions (${count})`;
  if (count === 0) return <>{label}</>;
  return (
    <>
      <span>{label}</span>
      <span
        className="sidebar-status-split sidebar-status-split-full"
        aria-hidden="true"
      >
        · {workingBreakdownText(breakdown)}
      </span>
      <span
        className="sidebar-status-split sidebar-status-split-compact"
        aria-hidden="true"
        title={workingBreakdownDescription(breakdown)}
      >
        ({breakdown.foreground}/{breakdown.background})
      </span>
    </>
  );
}

export function WorkingSessionsBreakdownDescription({
  id,
  count,
  breakdown,
}: {
  id: string;
  count: number;
  breakdown: SessionWorkingBreakdown;
}) {
  if (count === 0) return null;
  return (
    <span id={id} hidden>
      {workingBreakdownDescription(breakdown)}
    </span>
  );
}
