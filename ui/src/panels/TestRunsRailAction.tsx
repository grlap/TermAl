// Owns the test-run rail action, flask icon and active-count presentation.
// Does not own run tracking or panel details. Extracted from ControlPanelSurface.tsx.
import { useContext } from "react";
import { TestRunsContext } from "../test-runs-context";

export function TestRunsRailAction() {
  const { runs, open, snapshotReady } = useContext(TestRunsContext);
  if (!open) return null;
  const activeCount = snapshotReady ? runs.filter(run => run.state === "running").length : 0;
  const label = activeCount > 0
    ? `Open Test Runs (${activeCount} active ${activeCount === 1 ? "run" : "runs"})`
    : "Open Test Runs";

  return <button
    className="control-panel-activity-button control-panel-action-button"
    type="button"
    aria-label={label}
    title={label}
    onClick={() => open(null, undefined, true)}
  >
    <span className="control-panel-activity-icon" aria-hidden="true">
      <svg viewBox="0 0 20 20" focusable="false" aria-hidden="true">
        <path
          d="M7.5 3.25h5M8.5 3.25v4.6l-4.1 7.2a1.4 1.4 0 0 0 1.2 2.1h8.8a1.4 1.4 0 0 0 1.2-2.1l-4.1-7.2v-4.6"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="1.5"
        />
        <path d="M6.2 12.25h7.6" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.5" />
      </svg>
    </span>
    {activeCount > 0 ? <span className="control-panel-activity-badge" aria-hidden="true">
      {Math.min(activeCount, 99)}
    </span> : null}
  </button>;
}
