// Read-only details and bounded log tails. Requests are cancelled on selection
// change; a stale response cannot overwrite another run or a newer delta.
import { useEffect, useState, type ReactNode } from "react";
import { copyTextToClipboard } from "../clipboard";
import { readTestRun, readTestRunLog } from "../test-runs-api";
import { testRunStageLabel, type TestRunSummary } from "../test-runs";

export function TestRunDuration({ start, end, running = false }: { start: string | null; end: string | null; running?: boolean }) {
  if (!start || (!end && !running)) return <>duration unknown</>;
  const ms = (end ? Date.parse(end) : Date.now()) - Date.parse(start);
  return <>{Number.isFinite(ms) && ms >= 0 ? `${Math.floor(ms / 1000)}s${end ? "" : " elapsed at update"}` : "duration unknown"}</>;
}

export function TestRunDetails(props: { run: TestRunSummary } | { runId: string; fallback?: ReactNode }) {
  const requestedId = "run" in props ? props.run.runId : props.runId;
  const liveRun = "run" in props ? props.run : undefined;
  const [reload, setReload] = useState(0);
  const [result, setResult] = useState<{ requestedId: string; source: TestRunSummary | undefined; reload: number; data?: Awaited<ReturnType<typeof readTestRun>>; error?: string } | null>(null);
  const [stageName, setStageName] = useState<string | null>(null);
  const [copyNotice, setCopyNotice] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    void readTestRun(requestedId, controller.signal).then(data => {
      if (!controller.signal.aborted) setResult({ requestedId, source: liveRun, reload, data });
    }, error => {
      if (!controller.signal.aborted) setResult(previous => ({
        requestedId, source: liveRun, reload,
        // A failed refresh is not evidence that the previously loaded detail
        // disappeared. Keep it visible, explicitly stale, for this run only.
        data: previous?.requestedId === requestedId ? previous.data : undefined,
        error: String(error instanceof Error ? error.message : error),
      }));
    });
    return () => controller.abort();
  }, [requestedId, liveRun, reload]);
  const current = result?.requestedId === requestedId && result.source === liveRun && result.reload === reload ? result : null;
  const loaded = result?.requestedId === requestedId ? result.data : undefined;
  const run = liveRun ?? loaded?.run;
  const detail = loaded?.detail;
  const firstFailure = detail?.stages.find(stage => stage.state === "failed");
  if (!run) return <>{"runId" in props ? props.fallback : null}<section className="test-run-details" aria-label={`Run details ${requestedId}`}>
    <h3>{requestedId}</h3>
    {current?.error ? <p role="alert">{current.error}</p> : <p role="status">Loading details…</p>}
    <button type="button" onClick={() => setReload(value => value + 1)}>Refresh details</button>
  </section></>;
  return <section className="test-run-details" aria-label={`Run details ${run.runId}`}>
    <h3>{run.runId}</h3>
    <p>State: {run.state}{run.interrupted ? " (interrupted)" : ""} · Exit: {run.exitCode ?? "not recorded"}</p>
    <p>Tracking: {run.registrationState ?? "discovered"} · {run.trackingDurability ?? "unconfirmed"}</p>
    <p>Restart evidence impact: {run.restartWouldVoidEvidence === true ? "would void evidence" : run.restartWouldVoidEvidence === false ? "no pending evidence" : "unknown"}. {run.restartEvidenceReason ?? "Exact verification correlation is unavailable."}</p>
    {run.monitoringInterruptedAt ? <p>Monitoring interrupted at {run.monitoringInterruptedAt}.</p> : null}
    {run.plannedStageNames?.length ? <p>Planned stages: {run.plannedStageNames.join(", ")}. Actual progress comes from the result artifact.</p> : null}
    {run.artifactLocations?.map(path => <p key={path}>Declared result file: <code>{path}</code></p>)}
    {run.state === "unknown" ? <p>Terminal evidence is missing. Unknown does not mean passed or safely finished.</p> : null}
    {run.error ? <p role="alert">{run.error}</p> : null}
    <p className="test-run-path">{run.runDir}</p>
    {run.runDir ? <button type="button" onClick={() => {
      void copyTextToClipboard(run.runDir).then(() => setCopyNotice("Run directory copied"), () => setCopyNotice("Copy failed; select the path above"));
    }}>Copy run directory</button> : <p>No launcher artifact directory was reported.</p>}
    {copyNotice ? <p role="status">{copyNotice}</p> : null}
    <button type="button" onClick={() => setReload(value => value + 1)}>Refresh details</button>
    {!current ? <p role="status">{detail ? "Refreshing details… Showing the previous detail snapshot." : "Loading details…"}</p>
      : current.error ? <p role="alert">{current.error}{detail ? " Showing the previous detail snapshot; it may be stale." : ""}</p> : null}
    {detail ? <>
      {firstFailure ? <section aria-label="First failing stage"><h4>First failure: {firstFailure.name}</h4>
        <p>{firstFailure.error}</p><pre>{firstFailure.diagnostics?.text || "No extracted diagnostics. Inspect the stage log."}</pre>
        {firstFailure.diagnostics?.truncated ? <p>Diagnostics truncated; full output remains in the run directory.</p> : null}
      </section> : null}
      <div className="test-run-table-scroll"><table><caption>Stages</caption><thead><tr><th>Stage</th><th>State</th><th>Exit</th><th>Duration</th><th>Log</th></tr></thead><tbody>
        {detail.stages.map(stage => <tr key={stage.name}><th scope="row">{stage.name}</th><td>{testRunStageLabel(run, stage)}</td>
          <td>{stage.exitCode ?? "—"}</td><td><TestRunDuration start={stage.startedAt} end={stage.endedAt} running={run.state === "running" && stage.state === "running"} /></td>
          <td><button type="button" disabled={!stage.log} aria-pressed={stage.name === stageName} onClick={() => setStageName(stage.name)}>View {stage.name} log</button></td>
        </tr>)}
      </tbody></table></div>
      <details><summary>Commands and preflight</summary>
        {detail.stages.map(stage => <section key={stage.name}><h4>{stage.name}</h4><pre>{stage.command?.join(" ") ?? "Command not recorded"}</pre><p>Cwd: {stage.cwd ?? "not recorded"}</p><p>Log: {stage.log ?? "not available"}</p>
          {stage.error ? <p>{stage.error}</p> : null}{stage.diagnostics ? <pre>{stage.diagnostics.text}{stage.diagnostics.truncated ? "\n[diagnostics truncated]" : ""}</pre> : null}
        </section>)}
        {detail.preflight.map((check, index) => <section key={`${index}:${check.name}`}><h4>{check.name}</h4><p>Exit: {check.exitCode ?? "not recorded"}</p><pre>{check.command?.join(" ")}</pre><p>Log: {check.log ?? "not available"}</p>
          {check.diagnostics ? <pre>{check.diagnostics.text}{check.diagnostics.truncated ? "\n[diagnostics truncated]" : ""}</pre> : null}
        </section>)}
      </details>
      <details><summary>Fingerprint</summary><dl>
        <dt>Expected</dt><dd>{detail.expectedFingerprint ?? "not recorded"}</dd>
        <dt>Before</dt><dd>{detail.before ?? "not recorded"}</dd><dt>After</dt><dd>{detail.after ?? "not recorded"}</dd>
      </dl></details>
      {detail.limitations ? <p>{detail.limitations}</p> : null}
    </> : null}
    {stageName ? <TestRunLog key={`${run.runId}:${stageName}`} runId={run.runId} stage={stageName} /> : null}
  </section>;
}

function TestRunLog({ runId, stage }: { runId: string; stage: string }) {
  const [reload, setReload] = useState(0);
  const [result, setResult] = useState<{ reload: number; data?: Awaited<ReturnType<typeof readTestRunLog>>; error?: string } | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    void readTestRunLog(runId, stage, controller.signal).then(data => {
      if (!controller.signal.aborted) setResult({ reload, data });
    }, error => {
      if (!controller.signal.aborted) setResult({ reload, error: String(error instanceof Error ? error.message : error) });
    });
    return () => controller.abort();
  }, [runId, stage, reload]);
  const current = result?.reload === reload ? result : null;
  return <section aria-label={`${stage} log tail`}>
    <h4>{stage} — last 64 KiB</h4>
    <button type="button" onClick={() => setReload(value => value + 1)}>Refresh log tail</button>
    {!current ? <p role="status">Loading log…</p> : current.error ? <p role="alert">{current.error}</p> : null}
    {current?.data ? <><p>{current.data.size} bytes total{current.data.truncated ? " · truncated tail" : ""}</p><pre>{current.data.text || "Log is empty."}</pre></> : null}
  </section>;
}
