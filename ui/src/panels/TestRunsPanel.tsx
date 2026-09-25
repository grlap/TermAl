// Read-only test-run list, filters and selection. Live summaries come from the
// app's existing revision-gated stream; details/logs are bounded explicit reads.
import { useContext, useMemo, useState } from "react";
import type { Project, Session } from "../types";
import { TestRunsContext } from "../test-runs-context";
import { sortTestRuns, testRunMatchesSession, type TestRunState } from "../test-runs";
import { TestRunDetails, TestRunDuration } from "./TestRunDetails";
import "./test-runs-panel.css";

export function TestRunsPanel({ projects, sessions, initialProjectId = null, initialSessionId = null }: {
  projects: readonly Project[]; sessions: readonly Session[];
  initialProjectId?: string | null; initialSessionId?: string | null;
}) {
  const { runs } = useContext(TestRunsContext);
  const [projectId, setProjectId] = useState(initialSessionId ? "" : initialProjectId ?? "");
  const [sessionId, setSessionId] = useState(initialSessionId ?? "");
  const [state, setState] = useState<TestRunState | "">("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const names = useMemo(() => new Map(sessions.map(session => [session.id, session.name])), [sessions]);
  const sessionIds = [...new Set([...sessions.map(session => session.id), ...runs.flatMap(run =>
    [run.ownerSessionId, run.notifySessionId].filter((id): id is string => id !== null)), ...(sessionId ? [sessionId] : [])])];
  const visible = sortTestRuns(runs.filter(run => (!projectId || run.projectId === projectId) &&
    (!sessionId || testRunMatchesSession(run, sessionId)) && (!state || run.state === state)));
  const selected = visible.find(run => run.runId === selectedId);
  return <section className="test-runs-panel" aria-label="Test Runs">
    <header><h2>Test Runs</h2><p>Read-only launcher discovery. A notification target is not a registered wait.</p></header>
    <div className="test-runs-filters">
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
        {sessionIds.map(id => <option key={id} value={id}>{names.get(id) ?? id}</option>)}
      </select></label>
    </div>
    <div className="test-runs-layout">
      <div className="test-runs-list" aria-label="Discovered runs">
        {!visible.length ? <p>No test runs match these filters.</p> : visible.map(run => <button
          key={run.runId} type="button" className="test-run-row" aria-pressed={selectedId === run.runId}
          onClick={() => setSelectedId(run.runId)}>
          <span className={`test-run-state is-${run.state}`}>{run.state}{run.interrupted ? " · interrupted" : ""}</span>
          <strong>{run.command?.join(" ") || run.preset}{run.commandTruncated ? " … (truncated)" : ""}</strong>
          <span>{run.runId}</span><span title={run.worktree}>{run.worktree}</span>
          <span>Owner: {run.ownerSessionId ? names.get(run.ownerSessionId) ?? run.ownerSessionId : "not recorded"}</span>
          <span>Notify: {run.notifySessionId ? names.get(run.notifySessionId) ?? run.notifySessionId : run.notifyTo ?? "none"}</span>
          <span>Started: {run.startedAt ?? "not recorded"} · <TestRunDuration start={run.startedAt} end={run.endedAt} running={run.state === "running"} /></span>
          {run.state === "running" ? <span>Stage: {run.currentStage ?? "none"}</span> : null}
        </button>)}
      </div>
      {selected ? <TestRunDetails key={selected.runId} run={selected} /> : <p>Select a run to inspect stages and logs.</p>}
    </div>
  </section>;
}
