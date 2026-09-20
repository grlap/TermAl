// New item drawer for inert Engram detail/notes. Owns window identity and
// explicit pagination, not holder identity inference or full-body CLI actions.
import { useEffect, useRef, useState } from "react";
import { ApiRequestError } from "../api-request";
import { readWorkDetail, type WorkDetailResponse } from "../work-visualizer-api";
import { WorkTime } from "./work-time";
import { WorkDetailsHeader } from "./WorkDetailsHeader";

export function WorkItemDetails({ projectId, workRef, readerId, onClose }: {
  projectId: string; workRef: string; readerId: string; onClose: () => void;
}) {
  const [data, setData] = useState<WorkDetailResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const lifetime = useRef<AbortController | null>(null);
  const locked = useRef(false);
  const latest = useRef<WorkDetailResponse | null>(null);
  const panel = useRef<HTMLElement | null>(null);

  useEffect(() => {
    panel.current?.focus();
    panel.current?.scrollIntoView?.({ block: "nearest" });
  }, [projectId, workRef, readerId]);

  async function load(controller: AbortController, more: boolean) {
    if (locked.current || controller.signal.aborted) return;
    const previous = latest.current;
    const after = more ? previous?.notesWindow.after : undefined;
    if (more && !after) return;
    locked.current = true; setBusy(true); setError(null);
    try {
      const next = await readWorkDetail(projectId, workRef, readerId, controller.signal, after ?? undefined);
      if (lifetime.current !== controller || controller.signal.aborted) return;
      if (more && previous) {
        const old = previous.notesWindow;
        const window = next.notesWindow;
        if (window.readCut.projectPosition !== old.readCut.projectPosition
          || window.total !== old.total || window.newer !== previous.notes.length
          || next.notes.some(note => previous.notes.some(oldNote => oldNote.locator === note.locator))) {
          latest.current = null; setData(null);
          throw new Error("Notes window changed. Reload details to start a fresh window.");
        }
        next.status = previous.status; next.holder = previous.holder; next.heldUntil = previous.heldUntil;
        // Engram windows are oldest-first internally. An older window goes
        // before the retained window; only presentation reverses the timeline.
        next.notes = [...next.notes, ...previous.notes];
      }
      latest.current = next; setData(next);
    } catch (e) {
      if (lifetime.current !== controller || controller.signal.aborted) return;
      if (e instanceof ApiRequestError && e.status === 409) { latest.current = null; setData(null); }
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (lifetime.current === controller && !controller.signal.aborted) { locked.current = false; setBusy(false); }
    }
  }

  useEffect(() => {
    const controller = new AbortController(); lifetime.current = controller;
    locked.current = false; latest.current = null; setData(null); setError(null);
    void load(controller, false);
    return () => { controller.abort(); };
    // Each identity owns its async callback; stale replies are fenced above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, workRef, readerId, revision]);

  return <aside ref={panel} tabIndex={-1} className="work-item-details" aria-label="Work item details" aria-busy={busy} onKeyDown={event => {
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onClose(); }
  }}>
    <WorkDetailsHeader workRef={workRef} onClose={onClose} onReload={() => setRevision(v => v + 1)} />
    {error && <p role="alert">{error}</p>}
    {busy && <p role="status">Reading Engram details…</p>}
    {data?.status && <>
      <h3>{data.status.work.title}</h3>
      <p>P{data.status.work.priority} · {data.status.work.kind} · {data.status.work.lifecycle} / {data.status.availability}</p>
      <p>Holder: {data.holder ?? "Not reported"}{data.heldUntil && <> · until <WorkTime value={data.heldUntil} /></>}</p>
      <p className="work-inert-text">{data.status.work.outcome}</p>
      <h4>Acceptance</h4><ul>{data.status.work.acceptance.map((text, i) => <li key={i}>{text}</li>)}</ul>
      {!!data.status.work.acceptanceOmitted && <p>{data.status.work.acceptanceOmitted} acceptance entries omitted by source.</p>}
    </>}
    {data && <>
      <h4>Notes and gate evidence ({data.notes.length} of {data.notesWindow.total})</h4>
      <p>Snapshot: <WorkTime value={data.notesWindow.readCut.observedAt} />. Detail and list are separate reads.</p>
      <ol>{[...data.notes].reverse().map(note => <li key={note.locator}>
        <small>{note.family} / {note.kind} · {note.by ?? "Unknown author"} · <WorkTime value={note.createdAt} /></small>
        {note.statusOwner === false ? <p>Peer status observation, no commitment.</p>
          : note.statusOwner === true ? <p>Owner status commitment.</p>
            : note.kind === "status" ? <p>Status ownership not reported by source.</p> : null}
        {note.nonHolder === true && <p>Recorded by a non-holder.</p>}
        <p className="work-inert-text">{note.summary ?? "Body omitted by source"}</p>
        {note.refs == null ? <p>References not included by source.</p>
          : note.refs.length > 0 && <ul aria-label="Note references">{note.refs.map((ref, i) => <li className="work-inert-text" key={i}>{ref}</li>)}</ul>}
        {(note.bodyOmitted || note.summaryTruncated) && <p>Partial body. Full-body reads are not available here yet. Locator: <code>{note.locator}</code></p>}
      </li>)}</ol>
      {data.notesWindow.older > 0 && <p>{data.notesWindow.older} older records remain.</p>}
      {data.notesWindow.after && <button type="button" disabled={busy} onClick={() => { if (lifetime.current) void load(lifetime.current, true); }}>Load older notes</button>}
    </>}
  </aside>;
}
