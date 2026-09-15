// New Work surface (workspace tab). Owns independent project selection, the
// view switch and the merged loaded-row presentation across sources. Does not
// edit trackers, infer holder identity, or run suggestions.
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type { Project } from "../types";
import type { WorkFilters, WorkItem } from "../work-visualizer-api";
import { useWorkList } from "./use-work-list";
import { WorkBeadDetails } from "./WorkBeadDetails";
import { WorkItemDetails } from "./WorkItemDetails";
import type { WorkSelection } from "./WorkRow";
import { WORK_SORT_COLUMNS, defaultWorkSortDirection, sortWorkRows, type WorkSort, type WorkSortKey } from "./work-sort";
import { WorkTime } from "./work-time";
import { WorkTable } from "./WorkTable";
import { WorkTree } from "./WorkTree";
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
  // Adopt a fallback before rendering children so later focus changes cannot
  // remount their filters/details. Only a selector action writes localStorage.
  if (projectId !== selected) setSelected(projectId);
  return <section className="work-panel" aria-label="Work">
    <label>Project <select aria-label="Work project" value={projectId} onChange={e => {
      setSelected(e.target.value);
      try { localStorage.setItem(PROJECT_KEY, e.target.value); } catch { /* Selection remains usable in memory. */ }
    }}>{projects.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>
    <p className="work-panel-caption">Read-only work visualizer · Engram and Beads. No task edits, claims, or automatic enablement.</p>
    {projectId ? <WorkProjectView key={projectId} projectId={projectId} /> : <p>No project available. Add a project to inspect its work sources.</p>}
  </section>;
}

type WorkView = "dependencies" | "hierarchy" | "table";
const VIEWS: { id: WorkView; label: string }[] = [
  { id: "dependencies", label: "Dependencies" }, { id: "hierarchy", label: "Hierarchy" }, { id: "table", label: "Table" },
];
const DEFAULT_KINDS = ["task", "bug", "feature", "epic", "chore", "research"];

function WorkProjectView({ projectId }: { projectId: string }) {
  const [draft, setDraft] = useState<WorkFilters>({ search: "", label: "", availability: "" });
  const [filters, setFilters] = useState(draft);
  const [submission, setSubmission] = useState(0);
  // Presentation choices outlive a read generation: applying source filters
  // remounts the results below, and the chosen view and sort must survive it.
  const [view, setView] = useState<WorkView>("dependencies");
  const [sort, setSort] = useState<WorkSort | null>(null);
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
    <WorkResults key={`${submission}:${JSON.stringify(filters)}`} projectId={projectId} filters={filters}
      view={view} onViewChange={setView} sort={sort} onSortChange={setSort} />
  </>;
}

function WorkResults({ projectId, filters, view, onViewChange, sort, onSortChange }: {
  projectId: string; filters: WorkFilters; view: WorkView; onViewChange: (view: WorkView) => void;
  sort: WorkSort | null; onSortChange: (sort: WorkSort | null) => void;
}) {
  const { result, busy, error, reload, more } = useWorkList(projectId, filters);
  // The kind filter is per read generation: its options come from the loaded
  // rows, which a new generation replaces.
  const [kind, setKind] = useState("");
  const [selection, setSelection] = useState<WorkSelection | null>(null);
  const detailTrigger = useRef<HTMLButtonElement | null>(null);
  const listContainer = useRef<HTMLDivElement | null>(null);
  const page = result?.page;
  const beads = result?.beads;
  // Stable row identity lets the tree memoise its forest across selection and
  // collapse re-renders.
  const loaded = useMemo(() => [...(page?.items ?? []), ...(beads?.items ?? [])], [page, beads]);
  // The selected kind stays an option even when a refreshed snapshot has no
  // rows of it: the filter is then visibly active with no matches, never
  // silently applied behind "All".
  const kinds = useMemo(
    () => [...new Set([...DEFAULT_KINDS, ...loaded.map(row => row.kind), ...(kind ? [kind] : [])])],
    [loaded, kind],
  );
  // One sort model serves the control below and the table headers; it orders
  // loaded rows only and never touches the sources' pages or identities.
  const rows = useMemo(() => sortWorkRows(loaded.filter(row => !kind || row.kind === kind), sort), [loaded, kind, sort]);
  const total = (page?.total ?? 0) + (beads?.total ?? 0);
  const loadedSummary = [
    page && `${page.items.length} of ${page.total} Engram items loaded`,
    beads && `${beads.items.length} of ${beads.total} Beads items loaded`,
  ].filter(Boolean).join(" · ");
  const select = (item: WorkItem, trigger: HTMLButtonElement) => { detailTrigger.current = trigger; setSelection({ source: item.source, ref: item.shortRef }); };
  const closeDetails = () => {
    setSelection(null);
    // The row button may be gone (collapsed ancestor, view switch, filter):
    // fall back to the list so keyboard focus is never dropped on the body.
    const trigger = detailTrigger.current;
    if (trigger?.isConnected) trigger.focus(); else listContainer.current?.focus();
  };
  // An Engram selection cannot outlive its reader generation: once the
  // generation is dropped there is nothing to show for it. The drawer, and
  // the control that dropped the generation, may have unmounted with the
  // focus: like closeDetails, park it on the list rather than the body.
  const readerId = result?.readerId ?? null;
  useEffect(() => {
    if (!selection || selection.source === "beads" || readerId) return;
    setSelection(null);
    const active = document.activeElement;
    if (active && active !== document.body && active.isConnected) return;
    const trigger = detailTrigger.current;
    if (trigger?.isConnected) trigger.focus(); else listContainer.current?.focus();
  }, [selection, readerId]);
  const details = !selection ? null
    : selection.source === "beads" ? <WorkBeadDetails key={`beads:${selection.ref}`} projectId={projectId} issueId={selection.ref} onClose={closeDetails} />
      : result?.readerId ? <WorkItemDetails key={`${result.readerId}:${selection.ref}`} projectId={projectId} workRef={selection.ref} readerId={result.readerId} onClose={closeDetails} />
        : null;
  return <>
    <div className="work-panel-actions">
      <div className="work-view-switch" role="group" aria-label="Work view">
        {VIEWS.map(option => <button key={option.id} type="button" aria-pressed={view === option.id} onClick={() => onViewChange(option.id)}>{option.label}</button>)}
      </div>
      <button type="button" disabled={busy} onClick={() => { setSelection(null); reload(); }}>Refresh work</button>
      {busy && <span role="status">Reading work…{page?.more ? ` ${page.items.length} of ${page.total} Engram items so far` : ""}</span>}</div>
    {error && <p role="alert">{error} Use Refresh work for a new snapshot.</p>}
    {result?.sources.map(source => <p key={source.source} className="work-source-status" data-state={source.state}>
      <strong>{source.source}: {source.state}</strong> — {source.message}
    </p>)}
    {(page || beads) && result && <>
      <div className="work-panel-filters">
        <label>Kind (loaded rows) <select value={kind} onChange={e => setKind(e.target.value)}><option value="">All</option>
          {kinds.map(value => <option key={value}>{value}</option>)}</select></label>
        <label>Sort (loaded rows) <select value={sort?.key ?? ""} onChange={e => {
          const key = e.target.value as WorkSortKey | "";
          onSortChange(key ? { key, direction: sort?.key === key ? sort.direction : defaultWorkSortDirection(key) } : null);
        }}><option value="">Source order</option>
          {WORK_SORT_COLUMNS.map(column => <option key={column.key} value={column.key}>{column.label}</option>)}</select></label>
        {sort && <button type="button" className="work-sort-direction" title="Reverse the sort direction"
          onClick={() => onSortChange({ ...sort, direction: sort.direction === "asc" ? "desc" : "asc" })}>
          {sort.direction === "asc" ? "Ascending" : "Descending"}<span aria-hidden="true">{sort.direction === "asc" ? " ▲" : " ▼"}</span>
        </button>}
      </div>
      <p>{loadedSummary}; {rows.length} visible. Filters and sorting (the control above or a table header) apply only to loaded rows.</p>
      <p className="work-panel-caption">{result.beadsObservedAt && result.beadsObservedAt !== result.observedAt
        ? <>Engram read completed: <WorkTime value={result.observedAt} /> · Beads snapshot from <WorkTime value={result.beadsObservedAt} />. Refresh for current data.</>
        : <>Read completed: <WorkTime value={result.observedAt} />. Refresh for current data.</>}</p>
      <div className="work-panel-body" data-details={details ? "open" : "closed"}>
        <div className="work-panel-list" ref={listContainer} tabIndex={-1}>
          {view === "table"
            ? <WorkTable rows={rows} selection={selection} onSelect={select} sort={sort} onSortChange={onSortChange} />
            : <WorkTree key={view} rows={rows} universe={loaded} mode={view} selection={selection} onSelect={select} />}
          {rows.length === 0 && <p>{result.sources.some(source => source.state === "error")
            ? "No rows from the readable sources. A source failed (see its status above), so this is not a complete tracker view."
            : total === 0 ? "No work items match the source filters." : "No matching rows in the loaded page(s). More source items may exist."}</p>}
          {page?.hint && <p>{page.hint}</p>}
          {beads?.hint && <p>{beads.hint}</p>}
          {page?.more && <p>More Engram items remain.{!page.after && " No continuation is available; the source page is byte-limited. See its hint above."}</p>}
          {page?.more && page.after && <button type="button" disabled={busy} onClick={more}>Load more work</button>}
        </div>
        {details}
      </div>
    </>}
  </>;
}
