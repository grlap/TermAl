// New item drawer for inert Beads detail (show + comments). Owns the detail
// read lifetime and focus handling; does not edit, claim, close, or run any
// command the receipt suggests.
import { useEffect, useRef, useState } from "react";
import { readWorkBeadsDetail, type WorkBeadsDetailResponse } from "../work-visualizer-api";
import { WorkTime } from "./work-time";
import { WorkDetailsHeader } from "./WorkDetailsHeader";

export function WorkBeadDetails({ projectId, issueId, onClose }: {
  projectId: string; issueId: string; onClose: () => void;
}) {
  const [data, setData] = useState<WorkBeadsDetailResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const panel = useRef<HTMLElement | null>(null);

  useEffect(() => {
    panel.current?.focus();
    panel.current?.scrollIntoView?.({ block: "nearest" });
  }, [projectId, issueId]);

  useEffect(() => {
    const controller = new AbortController();
    setData(null); setError(null); setBusy(true);
    readWorkBeadsDetail(projectId, issueId, controller.signal)
      .then(next => { if (!controller.signal.aborted) setData(next); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e)); })
      .finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => { controller.abort(); };
  }, [projectId, issueId, revision]);

  return <aside ref={panel} tabIndex={-1} className="work-item-details" aria-label="Work item details" aria-busy={busy} onKeyDown={event => {
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onClose(); }
  }}>
    <WorkDetailsHeader workRef={issueId} onClose={onClose} onReload={() => setRevision(v => v + 1)} />
    {error && <p role="alert">{error}</p>}
    {busy && <p role="status">Reading Beads details…</p>}
    {data && <>
      <h3>{data.item.title}</h3>
      <p>P{data.item.priority} · {data.item.kind} · {data.item.lifecycle} / {data.item.availability}</p>
      <p>Assignment: {data.item.assignedTo ?? "—"} · Updated <WorkTime value={data.item.updatedAt} /></p>
      {data.parent && <p>Subtask of {data.parent}</p>}
      <p className="work-inert-text">{data.description || "No description recorded."}</p>
      <h4>Relations ({data.dependencies.length})</h4>
      {data.dependencies.length === 0 && data.dependenciesUnread === 0 && <p>No relations recorded by source.</p>}
      {data.dependencies.length > 0 && <ul aria-label="Beads relations">{data.dependencies.map(dependency => <li key={`${dependency.dependencyType}:${dependency.id}`}>
        <small>{dependency.dependencyType}</small> <span className="work-inert-text">{dependency.id} — {dependency.title}</span> <small>{dependency.status} · P{dependency.priority} · {dependency.kind}</small>
      </li>)}</ul>}
      {data.dependenciesUnread > 0 && <p>{data.dependenciesUnread} relation {data.dependenciesUnread === 1 ? "record" : "records"} declared by the source could not be read; availability is not guessed from a partial receipt.</p>}
      <p>{data.dependentCount} {data.dependentCount === 1 ? "item depends" : "items depend"} on this item (source count).</p>
      <h4>Comments ({data.comments.length} of {data.commentCount})</h4>
      <p>Snapshot: <WorkTime value={data.observedAt} />. Detail and list are separate reads.</p>
      {data.comments.length === 0 ? <p>No comments recorded.</p>
        : <ol>{data.comments.map(comment => <li key={comment.id}>
          <small>{comment.author ?? "Unknown author"} · <WorkTime value={comment.createdAt} /></small>
          <p className="work-inert-text">{comment.text}</p>
        </li>)}</ol>}
    </>}
  </aside>;
}
