// Owns measured run-row rendering and keyboard traversal. Logical filtering,
// canonical history and selected evidence remain with the panel and its reader.
import { Fragment, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { testRunTrackingLabel, type TestRunSummary } from "../test-runs";
import { TestRunDuration } from "./TestRunDetails";
import { useWorkWindow } from "./use-work-window";

export function TestRunList({ runs, names, selectedId, onSelect, queryKey, ready, onVisible }: {
  runs: readonly TestRunSummary[];
  names: ReadonlyMap<string, string>;
  selectedId: string | null;
  onSelect: (id: string) => void;
  queryKey: string;
  ready: boolean;
  onVisible?: (ids: readonly string[]) => void;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const keys = useMemo(() => runs.map(run => run.runId), [runs]);
  const window = useWorkWindow(keys, viewport, selectedId ?? undefined);
  const visibleKey = JSON.stringify(window.indices.map(index => keys[index]));
  useLayoutEffect(() => { onVisible?.(JSON.parse(visibleKey) as string[]); }, [visibleKey, onVisible]);
  const previousQuery = useRef(queryKey);
  const [pendingFocus, setPendingFocus] = useState<{ key: string; keys: readonly string[] } | null>(null);
  useLayoutEffect(() => {
    if (previousQuery.current === queryKey) return;
    previousQuery.current = queryKey;
    setPendingFocus(null);
    if (viewport.current) viewport.current.scrollTop = 0;
    if (keys[0]) window.reveal(keys[0]);
  });
  useLayoutEffect(() => {
    if (!pendingFocus) return;
    // A reveal is consumed once. A changed query/live order cannot focus a
    // different row later, after the user has moved to a filter or detail.
    setPendingFocus(null);
    if (pendingFocus.keys !== keys) return;
    const node = Array.from(viewport.current?.querySelectorAll<HTMLElement>("[data-work-window-key]") ?? [])
      .find(element => element.dataset.workWindowKey === pendingFocus.key);
    node?.querySelector<HTMLButtonElement>(".test-run-row")?.focus({ preventScroll: true });
  }, [pendingFocus, keys]);
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!window.enabled || event.key !== "Tab" || event.altKey || event.ctrlKey || event.metaKey
      || !(event.target instanceof Element)) return;
    const current = event.target.closest<HTMLElement>("[data-work-window-key]");
    if (!current) return;
    const index = keys.indexOf(current.dataset.workWindowKey!) + (event.shiftKey ? -1 : 1);
    const key = keys[index];
    if (!key) return; // Native Tab leaves the list at its logical boundaries.
    event.preventDefault();
    const mounted = Array.from(viewport.current?.querySelectorAll<HTMLElement>("[data-work-window-key]") ?? [])
      .find(element => element.dataset.workWindowKey === key);
    const button = mounted?.querySelector<HTMLButtonElement>(".test-run-row");
    if (button) { setPendingFocus(null); button.focus(); return; }
    setPendingFocus({ key, keys });
    window.reveal(key);
  };
  const spacer = (height: number) => height > 0 ? <div aria-hidden="true" style={{ height }} /> : null;
  return <div ref={viewport} className="test-runs-list" role="list" aria-label="Discovered runs" onKeyDown={onKeyDown}>
    <div ref={window.content}>
      {!runs.length ? <p>{ready ? "No test runs match these filters." : "Waiting for the test-run index…"}</p> :
        window.indices.map((index, position) => {
          const run = runs[index];
          return <Fragment key={run.runId}>{spacer(window.before(position))}
            <div ref={window.rowRef(run.runId)} className="test-run-list-item" role="listitem"
              aria-posinset={index + 1} aria-setsize={runs.length}>
              <button type="button" className="test-run-row" aria-pressed={selectedId === run.runId}
                onClick={() => onSelect(run.runId)}>
                <span className={`test-run-state is-${run.state}`}>{run.state}{run.interrupted ? " · interrupted" : ""}</span>
                <strong>{run.command?.join(" ") || run.preset}{run.commandTruncated ? " … (truncated)" : ""}</strong>
                <span>{run.runId}</span><span title={run.worktree}>{run.worktree || "Worktree not recorded"}</span>
                <span>Owner: {run.ownerSessionId ? names.get(run.ownerSessionId) ?? run.ownerSessionId : "not recorded"}</span>
                <span>Notify: {run.notifySessionId ? names.get(run.notifySessionId) ?? run.notifySessionId : run.notifyTo ?? "none"}</span>
                <span>Tracking: {testRunTrackingLabel(run)}</span>
                <span>Observed through: {run.trackingProvenances?.join(", ") ?? "discovery"}</span>
                <span>Creator PID: {run.creatorPid ?? "unknown"} · Runner PID: {run.runnerPid ?? "unknown"}</span>
                <span>Evidence impact on restart: {run.restartWouldVoidEvidence === true ? "would void evidence" : run.restartWouldVoidEvidence === false ? "no pending evidence" : "unknown"}</span>
                <span>{run.restartEvidenceReason ?? "Exact verification correlation is not available."}</span>
                {run.monitoringInterruptedAt ? <span>Monitoring interrupted: {run.monitoringInterruptedAt}</span> : null}
                <span>{run.startTimeOrigin === "hostCommandStart" ? "Command start observed" : "Started"}: {run.startedAt ?? "not recorded"} · <TestRunDuration start={run.startedAt} end={run.endedAt} running={run.state === "running"} /></span>
                {run.state === "running" ? <span>Stage: {run.currentStage ?? "not recorded"}</span> : null}
              </button>
            </div>
          </Fragment>;
        })}
      {spacer(window.after)}
    </div>
  </div>;
}
