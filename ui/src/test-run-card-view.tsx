// Command-family rendering of persisted run evidence. Does not join live runs,
// mutate verdicts, launch commands, or control transcript scrolling.
import { useContext } from "react";
import type { TestRunCardMessage } from "./types";
import { TestRunsOpenContext } from "./test-runs-context";
import { testRunInvocation, testRunStatusLine, testRunUnknownText, testRunStageText } from "./test-run-card";
import { MessageMeta } from "./message-card-meta";
import { MessageActivityStatus } from "./message-activity-status";
import { renderHighlightedText, type SearchHighlightTone } from "./search-highlight";
import "./test-run-card.css";
import { mapCommandStatus } from "./app-utils";

export function TestRunCard({ message, searchQuery = "", searchHighlightTone = "match" }: {
  message: TestRunCardMessage; searchQuery?: string; searchHighlightTone?: SearchHighlightTone;
}) {
  const open = useContext(TestRunsOpenContext);
  const { run } = message;
  const text = (value: string) => renderHighlightedText(value, searchQuery, searchHighlightTone);
  const tone = run.state === "unknown" ? "neutral" : mapCommandStatus(run.state === "passed" ? "success" : run.state === "failed" ? "error" : "running");
  return <article className="message-card utility-card command-card test-run-card">
    <MessageMeta author={message.author} timestamp={message.timestamp} trailing={
      <span className={`chip chip-status chip-status-${tone} command-status-chip`}>
        <MessageActivityStatus state={run.state === "running" ? "running" : "inactive"} label={run.state} />
      </span>
    } />
    <div className="command-card-header"><div className="card-label">Test run</div>
      <button type="button" className="command-icon-button command-card-details-toggle"
        aria-label={`Details for test run ${run.runId}`}
        onClick={() => open?.(null, { runId: run.runId, runDir: run.runDir })}>Details</button>
    </div>
    <pre className="test-run-invocation">{text(testRunInvocation(run))}</pre>
    <p>{text(testRunStatusLine(run))}</p>
    {run.state === "unknown" ? <p>{text(testRunUnknownText(run))}</p> : null}
    {run.currentStage ? <p>Stage: {text(run.currentStage)}</p> : null}
    <ul>{run.stages.map((stage, index) => <li key={`${index}-${stage.name}`}>
      {text(testRunStageText(run, stage))}
    </li>)}</ul>
    {run.stagesOmitted > 0 ? <p>{run.stagesOmitted} stages omitted from this card. See Details.</p> : null}
    {run.error ? <p>{text(run.error)}{run.errorTruncated ? " … (truncated)" : ""}</p> : null}
    {run.state === "failed" && run.failure ? <details open><summary>{text(`${run.failure.phase}: ${run.failure.name}`)}</summary>
      <pre className="test-run-failure">{text(run.failure.excerpt)}</pre>
      {run.failure.truncated ? <p>Excerpt truncated; full log is available in Details.</p> : null}
    </details> : null}
  </article>;
}
