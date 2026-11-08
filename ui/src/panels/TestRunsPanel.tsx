// Read-only paged history, filters and selection. The app's revision-gated
// stream supplies live overlays; details/logs remain bounded explicit reads.
import { useContext, useMemo, useState } from "react";
import type { Project, Session } from "../types";
import { TestRunsContext } from "../test-runs-context";
import { testRunMatchesSession, type TestRunState } from "../test-runs";
import { TestRunDetails } from "./TestRunDetails";
import "./test-runs-panel.css";
import type { TestRunCardTarget } from "../test-run-card";
import { useTestRunHistory } from "./use-test-run-history";
import { TestRunList } from "./TestRunList";

export function TestRunsPanel({ projects, sessions, initialProjectId = null, initialSessionId = null, initialRun = null }: {
  projects: readonly Project[]; sessions: readonly Session[];
  initialProjectId?: string | null; initialSessionId?: string | null;
  initialRun?: TestRunCardTarget | null;
}) {
  const { runs: liveRuns, snapshotReady, overview, viewRevision } = useContext(TestRunsContext);
  const [projectId, setProjectId] = useState(initialSessionId ? "" : initialProjectId ?? "");
  const [sessionId, setSessionId] = useState(initialSessionId ?? "");
  const [state, setState] = useState<TestRunState | "">("");
  const [view, setView] = useState<"history" | "active" | "attention">("history");
  const [selectedId, setSelectedId] = useState<string | null>(initialRun?.runId ?? null);
  const history = useTestRunHistory(projectId, liveRuns, { sessionId, state, view }, viewRevision);
  const runs = history.runs;
  const names = useMemo(() => new Map(sessions.map(session => [session.id, session.name])), [sessions]);
  const sessionIds = useMemo(() => [...new Set([...sessions.map(session => session.id), ...runs.flatMap(run =>
    [run.ownerSessionId, run.notifySessionId].filter((id): id is string => id !== null)), ...(sessionId ? [sessionId] : [])])],
  [sessions, runs, sessionId]);
  const sessionOptions = useMemo(() => sessionIds.map(id => <option key={id} value={id}>{names.get(id) ?? id}</option>),
    [sessionIds, names]);
  // History already has the canonical time/id order; filtering preserves it.
  const visible = useMemo(() => runs.filter(run => (!projectId || run.projectId === projectId) &&
    (!sessionId || testRunMatchesSession(run, sessionId)) && (!state || run.state === state)),
  [runs, projectId, sessionId, state]);
  const selected = visible.find(run => run.runId === selectedId);
  return <section className="test-runs-panel" aria-label="Test Runs">
    <header><h2>Test Runs</h2><p>Registered launches and host-observed tests. Execution results and verification settlement are shown separately.</p>
      <p>Run details load as needed. Use Load older to continue through history.</p>
      {overview ? <p>{overview.total} runs · {overview.running} running · {overview.needsAttention} need attention · {overview.unknown} unknown</p> : null}</header>
    <div className="test-runs-filters">
      {overview ? <label>View <select value={view} onChange={event => setView(event.target.value as typeof view)}>
        <option value="history">History</option><option value="active">Active</option><option value="attention">Needs attention</option>
      </select></label> : null}
      <label>Project <select value={projectId} onChange={event => setProjectId(event.target.value)}>
        <option value="">All projects</option>
        {projectId && !projects.some(project => project.id === projectId) ? <option value={projectId}>{projectId}</option> : null}
        {projects.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}
      </select></label>
      <label>State <select value={state} onChange={event => setState(event.target.value as TestRunState | "")}>
        <option value="">All states</option>
        {(["running", "passed", "failed", "unknown"] as const).map(value => <option key={value}>{value}</option>)}
      </select></label>
      <label>Session <select value={sessionId} onChange={event => setSessionId(event.target.value)}>
        <option value="">All sessions</option>
        {sessionOptions}
      </select></label>
    </div>
    {history.error ? <p role="alert">{history.error} <button type="button" disabled={history.busy} onClick={history.retry}>Retry history</button></p> : null}
    {history.busy ? <p role="status">Loading test-run history…</p> : null}
    {history.hasMore ? <p>{overview ? "More matching runs are available." : "Filters apply to loaded history."} <button type="button" disabled={history.busy} onClick={history.more}>Load older</button></p> : null}
    <div className="test-runs-layout">
      <TestRunList runs={visible} names={names} selectedId={selectedId} onSelect={setSelectedId}
        queryKey={JSON.stringify([projectId, sessionId, state, view])} ready={history.loaded || (!overview && snapshotReady)} onVisible={history.visible} />
      {selected ? <TestRunDetails key={selected.runId} run={selected} /> : initialRun && selectedId === initialRun.runId && !runs.some(run => run.runId === initialRun.runId) ?
        <TestRunDetails key={initialRun.runId} runId={initialRun.runId} fallback={
          <section aria-label="Recorded run evidence"><h3>{overview ? "Recorded run evidence" : snapshotReady ? "Run not currently indexed" : "Waiting for the test-run index…"}</h3>
            <p>{initialRun.runId}</p><p>Run directory (literal path): <code>{initialRun.runDir}</code></p>
            <pre>node scripts/test-launcher.mjs summary RUN_DIRECTORY</pre>
            <p>RUN_DIRECTORY is a placeholder. Pass the literal path above as one argument using your shell’s quoting rules.</p>
            <p>{overview ? "Details are read using the recorded run identity; the global state does not contain run details." : snapshotReady ? "Discovery may still be in progress; absence from this snapshot does not prove the run was removed." : "The first state snapshot has not arrived yet."}</p>
            <p>The transcript card retains its recorded evidence. No new run has been started.</p>
          </section>
        } /> : overview && selectedId ? <TestRunDetails key={selectedId} runId={selectedId} /> : <p>Select a run to inspect stages and logs.</p>}
    </div>
  </section>;
}
