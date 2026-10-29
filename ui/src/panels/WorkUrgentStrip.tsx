// New Work "Urgent" strip. Owns the bounded overview above the Work views
// that lists every open P0 and P1 among the rows it is given (the loaded rows
// matching the display filters; P0 first, then P1, each by id), its count
// heading and its collapse toggle. Deliberately does not own selection,
// details, reveal, fetching or the views' structure: its buttons hand a row to
// the panel, which selects it and asks the active view to reveal it. New file;
// not split out of another.
import { useId, useMemo, useState, type MouseEvent } from "react";
import type { WorkItem } from "../work-visualizer-api";
import { isSelectedWorkItem, WorkRowLead, type WorkSelection } from "./WorkRow";
import { sortWorkRows } from "./work-sort";

const URGENT_SORT = { key: "priority", direction: "asc" } as const;

export function WorkUrgentStrip({ rows, selection, onSelect }: {
  rows: readonly WorkItem[];
  selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void;
}) {
  const [expanded, setExpanded] = useState(true);
  const listId = useId();
  const urgent = useMemo(
    () => sortWorkRows(rows.filter(row => row.lifecycle === "open" && row.priority <= 1), URGENT_SORT),
    [rows],
  );
  if (urgent.length === 0) return null;
  const p0 = urgent.filter(row => row.priority === 0).length;
  return <section className="work-urgent" aria-label="Urgent work">
    <button type="button" className="work-urgent-toggle" aria-expanded={expanded} aria-controls={listId} onClick={() => setExpanded(value => !value)}>
      Urgent: {p0} P0, {urgent.length - p0} P1
    </button>
    {expanded && <ul id={listId} className="work-urgent-list">
      {urgent.map(item => <li key={`${item.source}:${item.id}`} className="work-urgent-row">
        <WorkRowLead item={item} />
        <button type="button" className="work-urgent-title" aria-current={isSelectedWorkItem(selection, item) ? "true" : undefined}
          aria-label={`Urgent ${item.shortRef} — ${item.title}`}
          onClick={(event: MouseEvent<HTMLButtonElement>) => onSelect(item, event.currentTarget)}>
          <span className="work-urgent-ref">{item.shortRef}</span> {item.title}
        </button>
        {item.assignedTo && <span className="work-chip" title="Assignment is not the holder. Inspect details for the holder label.">{item.assignedTo}</span>}
      </li>)}
    </ul>}
  </section>;
}
