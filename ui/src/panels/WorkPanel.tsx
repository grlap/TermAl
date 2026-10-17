// New Work surface (workspace tab). Owns independent project selection, the
// view switch and the loaded Engram row presentation. Does not
// edit trackers, infer holder identity, or run suggestions.
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type { Project } from "../types";
import type { WorkFilters, WorkItem } from "../work-visualizer-api";
import { useWorkList } from "./use-work-list";
import { WorkItemDetails } from "./WorkItemDetails";
import type { WorkSelection } from "./WorkRow";
import { WORK_SORT_COLUMNS, defaultWorkSortDirection, sortWorkRows, type WorkSort, type WorkSortKey } from "./work-sort";
import { WorkTime } from "./work-time";
import { WorkTable } from "./WorkTable";
import { WorkTree } from "./WorkTree";
import { WorkLabels } from "./WorkLabels";
import { WorkMemories } from "./WorkMemories";
import { WorkLabelPicker } from "./WorkLabelPicker";
import { groupWorkLabels, matchesWorkLabels, type WorkLabelMode } from "./work-labels";
import { matchesWorkDisplayFilters, workSourceKey } from "./work-display-filters";
import { WorkPaging } from "./WorkPaging";
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
    <p className="work-panel-caption">Read-only work visualizer · Engram. No task edits, claims, or automatic enablement.</p>
    {projectId ? <WorkProjectView key={workSourceKey(projects.find(project => project.id === projectId)!)} projectId={projectId} /> : <p>No project available. Add a project to inspect its work sources.</p>}
  </section>;
}

type WorkView = "dependencies" | "hierarchy" | "labels" | "table" | "memories";
const VIEWS: { id: WorkView; label: string }[] = [
  { id: "dependencies", label: "Dependencies" }, { id: "hierarchy", label: "Hierarchy" }, { id: "labels", label: "Labels" }, { id: "table", label: "Table" },
  { id: "memories", label: "Memories" },
];
const DEFAULT_KINDS = ["task", "bug", "feature", "epic", "chore", "research"];

function WorkProjectView({ projectId }: { projectId: string }) {
  const searchHint = useId();
  const [draft, setDraft] = useState<WorkFilters>({ search: "", label: "", availability: "" });
  const [filters, setFilters] = useState(draft);
  const [view, setView] = useState<Exclude<WorkView, "memories">>("dependencies");
  const [memories, setMemories] = useState(false);
  const [sort, setSort] = useState<WorkSort | null>(null);
  function submit(event: FormEvent) { event.preventDefault(); setFilters({ ...draft }); }
  return <>
    <div className="work-view-switch" role="group" aria-label="Work view">
      {VIEWS.map(option => <button key={option.id} type="button" aria-pressed={(memories ? "memories" : view) === option.id} onClick={() => {
        setMemories(option.id === "memories");
        if (option.id !== "memories") setView(option.id);
      }}>{option.label}</button>)}
    </div>
    {memories && <WorkMemories projectId={projectId} />}
    <div className="work-results" hidden={memories}>
    <form className="work-panel-filters" onSubmit={submit}>
      <label>Search <input aria-describedby={searchHint} value={draft.search} onChange={e => setDraft({ ...draft, search: e.target.value })} /></label>
      <label>Label (loaded rows) <input value={draft.label} onChange={e => setDraft({ ...draft, label: e.target.value })} /></label>
      <label>Availability <select value={draft.availability} onChange={e => setDraft({ ...draft, availability: e.target.value })}>
        <option value="">All</option><option value="ready">Ready</option><option value="blocked">Blocked</option>
      </select></label>
      <button type="submit">Apply filters</button>
      {filters.availability && <small>Ready and Blocked include only open work.</small>}
    </form>
    <p id={searchHint} className="work-panel-caption">Search matches loaded IDs, titles, labels and assignment, ignoring case. It does not search notes, gates or outcomes.</p>
    <WorkResults projectId={projectId} filters={filters}
      view={view} active={!memories} sort={sort} onSortChange={setSort} />
    </div>
  </>;
}

function WorkResults({ projectId, filters, view, active, sort, onSortChange }: {
  projectId: string; filters: WorkFilters; view: Exclude<WorkView, "memories">;
  active: boolean; sort: WorkSort | null; onSortChange: (sort: WorkSort | null) => void;
}) {
  const { result, busy, error, needsReload, reload, retry } = useWorkList(projectId);
  const [kind, setKind] = useState("");
  const [lifecycle, setLifecycle] = useState("open");
  const [labels, setLabels] = useState<string[]>([]);
  const [labelMode, setLabelMode] = useState<WorkLabelMode>("any");
  const filterLabel = (label: string) => { setLabels([label]); setLabelMode("any"); };
  const [selection, setSelection] = useState<(WorkSelection & { view: WorkView }) | null>(null);
  const detailTrigger = useRef<HTMLButtonElement | null>(null);
  const focusNewSelection = useRef(false);
  useEffect(() => { focusNewSelection.current = false; });
  const listContainer = useRef<HTMLDivElement | null>(null);
  const scrollPosition = useRef(0);
  // Child windows may restore a row anchor when rows reorder. A display
  // action keeps the viewport position instead, bounded by browser geometry.
  useLayoutEffect(() => {
    const viewport = listContainer.current;
    if (!active || !viewport) return;
    viewport.scrollTop = scrollPosition.current;
    // Windows captured their row anchors before this parent layout effect.
    // Recapture now, before a child layout pass can reapply the old anchor.
    viewport.dispatchEvent(new Event("scroll"));
  }, [filters, kind, lifecycle, labels, labelMode, sort, view, active]);
  const page = result?.page;
  // Stable row identity lets the tree memoise its forest across selection and
  // collapse re-renders.
  const loaded = useMemo(() => page?.items ?? [], [page]);
  // The selected kind stays an option even when a refreshed snapshot has no
  // rows of it: the filter is then visibly active with no matches, never
  // silently applied behind "All".
  const kinds = useMemo(
    () => [...new Set([...DEFAULT_KINDS, ...loaded.map(row => row.kind), ...(kind ? [kind] : [])])],
    [loaded, kind],
  );
  // One sort model serves the control below and the table headers; it orders
  // loaded rows only and never touches the sources' pages or identities.
  const rows = useMemo(() => sortWorkRows(loaded.filter(row => matchesWorkDisplayFilters(row, filters) && (!kind || row.kind === kind)
    && (!lifecycle || (lifecycle === "closed" ? ["completed", "cancelled", "superseded"].includes(row.lifecycle) : row.lifecycle === lifecycle))
    && matchesWorkLabels(row, labels, labelMode)), sort), [loaded, filters, kind, lifecycle, labels, labelMode, sort]);
  const selectedItem = selection && rows.find(row => row.source === selection.source && row.shortRef === selection.ref);
  const visibleSelection = selectedItem && selection ? { ...selection, group: view === "labels"
    ? selection.view === view ? selection.group : groupWorkLabels([selectedItem])[0]?.key
    : undefined } : null;
  const total = page?.total ?? 0;
  const loadedSummary = [
    page && `${page.items.length} of ${page.total} Engram items loaded`,
  ].filter(Boolean).join(" · ");
  const select = (item: WorkItem, trigger: HTMLButtonElement, group?: string) => {
    if (detailTrigger.current === trigger && selection) { closeDetails(); return; }
    detailTrigger.current = trigger;
    focusNewSelection.current = true;
    setSelection({ source: item.source, ref: item.shortRef, group, view });
  };
  const hideSelection = () => { setSelection(null); detailTrigger.current = null; };
  const closeDetails = () => {
    // The row button may be gone (collapsed ancestor, view switch, filter):
    // fall back to the list so keyboard focus is never dropped on the body.
    const trigger = detailTrigger.current;
    hideSelection();
    if (trigger?.isConnected) trigger.focus(); else listContainer.current?.focus();
  };
  // Keep a visible logical selection across presentation changes, adopting
  // its new occurrence without moving focus from the display control.
  const readerId = result?.readerId ?? null;
  useEffect(() => {
    if (!selection || (visibleSelection && readerId && detailTrigger.current?.isConnected)) return;
    // A later page can reparent a tree occurrence. Its logical selection and
    // details survive; adopt the replacement trigger rather than closing it.
    if (visibleSelection && readerId && rows.some(item => item.source === selection.source && item.shortRef === selection.ref)) {
      const replacement = [...listContainer.current?.querySelectorAll<HTMLButtonElement>("[data-work-ref]") ?? []].find(button =>
        button.dataset.workRef === selection.ref && button.dataset.workSource === selection.source
        && button.closest<HTMLElement>("[data-work-group]")?.dataset.workGroup === visibleSelection.group);
      if (replacement) { detailTrigger.current = replacement; return; }
    }
    const trigger = detailTrigger.current;
    hideSelection();
    const active = document.activeElement;
    // A reparented tree row may briefly remount its details. That focus target
    // is about to disappear; only leave focus on a surviving external control.
    if (active && active !== document.body && active.isConnected && !active.closest(".work-item-details")) return;
    if (trigger?.isConnected) trigger.focus(); else listContainer.current?.focus();
  });
  const renderDetails = (item: WorkItem, group?: string) => visibleSelection && readerId
    && visibleSelection.source === item.source && visibleSelection.ref === item.shortRef && visibleSelection.group === group
    ? <WorkItemDetails key={`${readerId}:${visibleSelection.ref}`} projectId={projectId} workRef={visibleSelection.ref} readerId={readerId} onClose={closeDetails} autoFocus={focusNewSelection.current} />
    : null;
  return <>
    <div className="work-panel-actions">
      <button type="button" disabled={busy} onClick={() => { setSelection(null); reload(); }}>Refresh work</button>
      {busy && <span role="status">Reading work…{page?.more ? ` ${page.items.length} of ${page.total} Engram items so far` : ""}</span>}</div>
    {error && <div role="alert">{error} {needsReload ? "Use Refresh work for a new snapshot."
      : <button type="button" disabled={busy} onClick={retry}>Retry</button>}</div>}
    {result?.sources.map(source => <p key={source.source} className="work-source-status" data-state={source.state}>
      <strong>{source.source}: {source.state}</strong> — {source.message}
    </p>)}
    {result && !page && result.sources.some(source => source.state === "absent") && <p>No tracker is bound to this project.</p>}
    {result && !page && result.sources.some(source => source.state === "error") && <div className="work-panel-list" ref={listContainer} tabIndex={-1}><p>No rows from the readable sources. A source failed (see its status above), so this is not a complete tracker view.</p></div>}
    {page && result && <>
      <div className="work-panel-filters">
        <WorkLabelPicker rows={loaded} selected={labels} onChange={setLabels} mode={labelMode} onModeChange={setLabelMode} />
        <label>Kind (loaded rows) <select value={kind} onChange={e => setKind(e.target.value)}><option value="">All</option>
          {kinds.map(value => <option key={value}>{value}</option>)}</select></label>
        <label>Lifecycle (loaded rows) <select value={lifecycle} onChange={e => setLifecycle(e.target.value)}>
          <option value="open">Open</option><option value="closed">Closed</option><option value="">All</option>
          <option value="completed">Completed</option><option value="cancelled">Cancelled</option><option value="superseded">Superseded</option>
        </select></label>
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
      <p className="work-panel-caption">Read completed: <WorkTime value={result.observedAt} />. Refresh for current data.</p>
      <div className="work-panel-body">
        <div className="work-panel-list" ref={listContainer} onScroll={event => { if (active) scrollPosition.current = event.currentTarget.scrollTop; }} tabIndex={0} role="region" aria-label="Work list">
          {view === "table"
            ? <WorkTable viewport={listContainer} rows={rows} selection={visibleSelection} onSelect={select} sort={sort} onSortChange={onSortChange} onLabel={filterLabel} renderDetails={renderDetails} />
            : view === "labels" ? <WorkLabels viewport={listContainer} rows={rows} mixedSources={new Set(loaded.map(row => row.source)).size > 1} selection={visibleSelection} onSelect={select} onLabel={filterLabel} renderDetails={renderDetails} onHideSelection={hideSelection} />
              : <WorkTree key={view} viewport={listContainer} rows={rows} universe={loaded} mode={view} selection={visibleSelection} onSelect={select} onLabel={filterLabel} renderDetails={renderDetails} onHideSelection={hideSelection} />}
          {rows.length === 0 && <p>{result.sources.some(source => source.state === "error")
            ? "No rows from the readable sources. A source failed (see its status above), so this is not a complete tracker view."
            : total === 0 ? "No work items in this snapshot." : "No matching rows in the loaded page(s). More source items may exist."}</p>}
          <WorkPaging page={page} busy={busy} error={error} />
        </div>
      </div>
    </>}
  </>;
}
