// Read-only project memories, separate from work-item notes and dependencies.
// Each source owns an abortable read generation; bodies are inert text and
// fetched on demand. No remembered instructions are executed or injected.
import { useEffect, useId, useRef, useState } from "react";
import { ApiRequestError } from "../api-request";
import { readWorkMemories, type WorkMemory, type WorkMemoryResponse, type WorkMemorySource } from "../work-visualizer-api";
import { WorkTime } from "./work-time";

export function WorkMemories({ projectId }: { projectId: string }) {
  const [draft, setDraft] = useState("");
  const [search, setSearch] = useState("");
  const [generation, setGeneration] = useState(0);
  const [source, setSource] = useState<"all" | WorkMemorySource>("all");
  return <section className="work-memories" aria-label="Project memories">
    <p className="work-panel-caption">Retained project memories — not tasks, issue comments, or private session context. Read-only; memory text is never executed.</p>
    <form className="work-panel-filters" onSubmit={event => { event.preventDefault(); setSearch(draft.trim()); setGeneration(value => value + 1); }}>
      <label>Search memories <input value={draft} maxLength={2048} onChange={event => setDraft(event.target.value)} /></label>
      <label>Memory source <select value={source} onChange={event => setSource(event.target.value as typeof source)}>
        <option value="all">Both sources</option><option value="engram">Engram</option><option value="beads">Beads</option>
      </select></label>
      <button type="submit">Search memories</button>
      <button type="button" onClick={() => setGeneration(value => value + 1)}>Refresh memories</button>
    </form>
    <p className="work-panel-caption">Pages load automatically. Expand a memory in place to read its full text. Search uses each tracker’s own memory search; source limits are shown below.</p>
    {(["engram", "beads"] as const).filter(value => source === "all" || source === value).map(value =>
      <MemorySource key={`${projectId}:${value}:${generation}:${search}`} projectId={projectId} source={value} search={search} />)}
  </section>;
}

function MemorySource({ projectId, source, search }: { projectId: string; source: WorkMemorySource; search: string }) {
  const [data, setData] = useState<WorkMemoryResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  useEffect(() => {
    const controller = new AbortController();
    const read = async () => {
      let invalidated = false;
      try {
        let current = await readWorkMemories(projectId, source, search ? { search } : {}, controller.signal);
        if (controller.signal.aborted) return;
        setData(current);
        const keys = new Set(current.items.map(item => item.key));
        const cursors = new Set<string>();
        while (current.state === "ready" && current.nextAfter && !controller.signal.aborted) {
          const cursor = current.nextAfter;
          if (!current.readerId || source !== "engram" || current.exhausted || search || cursors.has(cursor)) {
            invalidated = true;
            throw new Error("Memory listing changed; refresh memories before continuing.");
          }
          cursors.add(cursor);
          const next = await readWorkMemories(projectId, source, { after: cursor, readerId: current.readerId }, controller.signal);
          if (controller.signal.aborted) return;
          const nextKeys = new Set(next.items.map(item => item.key));
          if (next.state !== "ready" || next.source !== source || next.readerId !== current.readerId
            || (next.nextAfter && (cursors.has(next.nextAfter) || next.items.length === 0 || next.exhausted))
            || nextKeys.size !== next.items.length || next.items.some(item => keys.has(item.key))) {
            invalidated = true;
            throw new Error("Memory listing changed; refresh memories before continuing.");
          }
          nextKeys.forEach(key => keys.add(key));
          current = { ...next, items: [...current.items, ...next.items] };
          setData(current);
        }
      } catch (reason) {
        if (!controller.signal.aborted) {
          if (invalidated || (reason instanceof ApiRequestError && reason.status === 409)) setData(null);
          setError(String(reason instanceof Error ? reason.message : reason));
        }
      } finally {
        if (!controller.signal.aborted) setBusy(false);
      }
    };
    void read();
    return () => controller.abort();
  }, [projectId, source, search]);
  return <section className="work-memory-source" aria-label={`${source} memories`}>
    <h3><span className="work-chip work-chip-source">{source}</span> Project memories</h3>
    {busy && <p role="status">Reading {source} memories…</p>}
    {error && <p role="alert">Loading stopped: {error} Use Refresh memories to retry.</p>}
    {data && <>
      {data.state !== "ready" ? <p className="work-source-status" data-state={data.state}>{data.state}: {data.message}</p> : <>
        <p className="work-panel-caption">{data.items.length} loaded{busy ? " · Loading more…" : data.exhausted && !error ? " · All returned memories loaded" : " · Incomplete listing"} · Last page read: <WorkTime value={data.observedAt} /></p>
        {source === "beads" && <p className="work-panel-caption">Beads does not provide memory dates.</p>}
        {data.items.length === 0 && <p>No matching project memories in {source}.</p>}
        <ul className="work-memory-list">{data.items.map(memory => <MemoryRow key={memory.key} memory={memory} projectId={projectId} source={source} readerId={data.readerId} />)}</ul>
        {data.omitted > 0 && <p>{data.omitted} more matches omitted; refine Search memories.</p>}
        {!data.exhausted && !data.nextAfter && data.omitted === 0 && <p>Listing is incomplete; refine Search memories.</p>}
      </>}
    </>}
  </section>;
}

function MemoryMetadata({ memory }: { memory: WorkMemory }) {
  return <p className="work-panel-caption">
    {memory.revision !== null && `Revision ${memory.revision}`}{memory.actor && ` · ${memory.actor}`}
    {memory.rememberedAt && <> · Revision date: <WorkTime value={memory.rememberedAt} /></>}
  </p>;
}

function MemoryRow({ memory, projectId, source, readerId }: {
  memory: WorkMemory; projectId: string; source: WorkMemorySource; readerId: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  return <li onKeyDown={event => {
    if (event.key === "Escape" && expanded) { event.stopPropagation(); setExpanded(false); trigger.current?.focus(); }
  }}>
    <button ref={trigger} type="button" className="work-memory-key" aria-expanded={expanded} aria-controls={bodyId}
      onClick={() => setExpanded(value => !value)}>
      <span className="work-memory-chevron" aria-hidden="true">{expanded ? "▾" : "▸"}</span><span>{memory.key}</span>
    </button>
    {!expanded && <><p className="work-inert-text">{memory.summary}</p><MemoryMetadata memory={memory} /></>}
    <div id={bodyId} hidden={!expanded}>
      {expanded && <MemoryBody projectId={projectId} source={source} memoryKey={memory.key} readerId={readerId} />}
    </div>
  </li>;
}

function MemoryBody({ projectId, source, memoryKey, readerId }: {
  projectId: string; source: WorkMemorySource; memoryKey: string; readerId: string | null;
}) {
  const [data, setData] = useState<WorkMemoryResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    readWorkMemories(projectId, source, { key: memoryKey, ...(readerId ? { readerId } : {}) }, controller.signal)
      .then(next => {
        if (controller.signal.aborted) return;
        if (next.state !== "ready" || next.items.length !== 1 || next.items[0]?.key !== memoryKey || next.items[0]?.body == null || (source === "engram" && next.readerId !== readerId)) throw new Error("Memory changed or body unavailable; refresh memories.");
        setData(next);
      }).catch(reason => { if (!controller.signal.aborted) setError(String(reason instanceof Error ? reason.message : reason)); });
    return () => controller.abort();
  }, [projectId, source, memoryKey, readerId]);
  const memory = data?.items[0];
  return <section className="work-memory-body" aria-label={`${memoryKey} full memory`}>
    {error ? <p role="alert">{error}</p> : !memory ? <p role="status">Reading memory…</p> : <>
      <p className="work-inert-text">{memory.body}</p>
      <MemoryMetadata memory={memory} />
    </>}
  </section>;
}
