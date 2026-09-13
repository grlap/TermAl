// New dock Work surface. Owns independent project selection and loaded-row
// presentation. Does not edit trackers, infer holder identity, or run suggestions.
import { useRef, useState, type FormEvent } from "react";
import type { Project } from "../types";
import type { WorkFilters } from "../work-visualizer-api";
import { useWorkList } from "./use-work-list";
import { WorkItemDetails } from "./WorkItemDetails";
import "./work-panel.css";

const PROJECT_KEY = "termal-work-project";
function initialProject(projects: readonly Project[], focusedProjectId: string | null) {
  try { const saved = localStorage.getItem(PROJECT_KEY); if (projects.some(p => p.id === saved)) return saved!; } catch { /* Storage may be unavailable. */ }
  if (projects.some(p => p.id === focusedProjectId)) return focusedProjectId!;
  return projects[0]?.id ?? "";
}

export function WorkPanel({ projects, focusedProjectId }: { projects: readonly Project[]; focusedProjectId: string | null }) {
  const [selected, setSelected] = useState(() => initialProject(projects, focusedProjectId));
  const projectId = projects.some(p => p.id === selected) ? selected : initialProject(projects, focusedProjectId);
  return <section className="work-panel" aria-label="Work">
    <label>Project <select aria-label="Work project" value={projectId} onChange={e => {
      setSelected(e.target.value);
      try { localStorage.setItem(PROJECT_KEY, e.target.value); } catch { /* Selection remains usable in memory. */ }
    }}>{projects.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>
    <p className="work-panel-caption">Read-only work visualizer · Engram first. No task edits, claims, or automatic enablement.</p>
    {projectId ? <WorkProjectView key={projectId} projectId={projectId} /> : <p>No project available. Add a project to inspect its work sources.</p>}
  </section>;
}

function WorkProjectView({ projectId }: { projectId: string }) {
  const [draft, setDraft] = useState<WorkFilters>({ search: "", label: "", availability: "" });
  const [filters, setFilters] = useState(draft);
  const [submission, setSubmission] = useState(0);
  function submit(event: FormEvent) { event.preventDefault(); setFilters({ ...draft }); setSubmission(value => value + 1); }
  return <>
    <form className="work-panel-filters" onSubmit={submit}>
      <label>Search <input value={draft.search} onChange={e => setDraft({ ...draft, search: e.target.value })} /></label>
      <label>Label <input value={draft.label} onChange={e => setDraft({ ...draft, label: e.target.value })} /></label>
      <label>Availability <select value={draft.availability} onChange={e => setDraft({ ...draft, availability: e.target.value })}>
        <option value="">All</option><option value="ready">Ready</option><option value="blocked">Blocked</option>
      </select></label>
      <button type="submit">Apply filters</button>
    </form>
    <WorkResults key={`${submission}:${JSON.stringify(filters)}`} projectId={projectId} filters={filters} />
  </>;
}

function WorkResults({ projectId, filters }: { projectId: string; filters: WorkFilters }) {
  const { result, busy, error, reload, more } = useWorkList(projectId, filters);
  const [kind, setKind] = useState("");
  const [sort, setSort] = useState("source");
  const [selectedRef, setSelectedRef] = useState<string | null>(null);
  const detailTrigger = useRef<HTMLButtonElement | null>(null);
  const page = result?.page;
  const rows = (page?.items ?? []).filter(row => !kind || row.kind === kind);
  if (sort === "priority") rows.sort((a, b) => a.priority - b.priority || a.id.localeCompare(b.id));
  else if (sort === "title") rows.sort((a, b) => a.title.localeCompare(b.title) || a.id.localeCompare(b.id));
  return <>
    <div className="work-panel-actions"><button type="button" disabled={busy} onClick={() => { setSelectedRef(null); reload(); }}>Refresh work</button>
      {busy && <span role="status">Reading work…</span>}</div>
    {error && <p role="alert">{error} Use Refresh work for a new snapshot.</p>}
    {result?.sources.map(source => <p key={source.source} className="work-source-status" data-state={source.state}>
      <strong>{source.source}: {source.state}</strong> — {source.message}
    </p>)}
    {page && <>
      <div className="work-panel-filters">
        <label>Kind (loaded rows) <select value={kind} onChange={e => setKind(e.target.value)}><option value="">All</option>
          {["task", "bug", "feature", "epic", "chore", "research"].map(value => <option key={value}>{value}</option>)}</select></label>
        <label>Sort (loaded rows) <select value={sort} onChange={e => setSort(e.target.value)}><option value="source">Source order</option><option value="priority">Priority</option><option value="title">Title</option></select></label>
      </div>
      <p>{page.items.length} of {page.total} source items loaded; {rows.length} visible. Filters and sorting above apply only to loaded rows.</p>
      <p className="work-panel-caption">Read completed: {result.observedAt}. Refresh for current data.</p>
      <div className="work-table-scroll" role="region" tabIndex={0} aria-label="Work table scroll area">
        <table className="work-table"><caption>Engram work</caption><thead><tr>
          <th>Priority</th><th>Reference / title</th><th>Kind</th><th>Lifecycle</th><th>Availability</th><th>Assignment</th><th>Updated</th>
        </tr></thead><tbody>{rows.map(row => <tr key={row.id}>
          <td>P{row.priority}</td><td><button type="button" onClick={event => { detailTrigger.current = event.currentTarget; setSelectedRef(row.shortRef); }}>{row.shortRef} — {row.title}</button>
            {!!row.labels.length && <small>{row.labels.join(", ")}</small>}</td>
          <td>{row.kind}</td><td>{row.lifecycle}</td><td>{row.availability}</td>
          <td title="Assignment is not the holder. Inspect details for the holder label.">{row.assignedTo ?? "—"}</td><td>{row.updatedAt}</td>
        </tr>)}</tbody></table>
      </div>
      {rows.length === 0 && <p>{page.total === 0 ? "No work items match the source filters." : "No matching rows in the loaded page(s). More source items may exist."}</p>}
      {page.hint && <p>{page.hint}</p>}
      {page.more && <p>More source items remain.{!page.after && " No continuation is available; the source page is byte-limited. See its hint above."}</p>}
      {page.more && page.after && <button type="button" disabled={busy} onClick={more}>Load more work</button>}
      {selectedRef && result.readerSessionId && <WorkItemDetails key={`${result.readerSessionId}:${selectedRef}`} projectId={projectId} workRef={selectedRef} readerSessionId={result.readerSessionId} onClose={() => { setSelectedRef(null); detailTrigger.current?.focus(); }} />}
    </>}
  </>;
}
