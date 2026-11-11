// Standalone Urgent view, adapted from WorkUrgentStrip. Lists the same open
// P0/P1 rows in priority order, with inline details supplied by the panel.
// Fetching, filters, paging and logical selection remain panel-owned.
import { useMemo } from "react";
import type { WorkItem } from "../work-visualizer-api";
import { isSelectedWorkItem, WorkRowButton, WorkRowLead, type WorkDetailsRenderer, type WorkSelection } from "./WorkRow";
import { sortWorkRows } from "./work-sort";

const URGENT_SORT = { key: "priority", direction: "asc" } as const;

export function WorkUrgentView({ rows, selection, onSelect, renderDetails }: {
  rows: readonly WorkItem[];
  selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void;
  renderDetails: WorkDetailsRenderer;
}) {
  const urgent = useMemo(
    () => sortWorkRows(rows.filter(row => row.lifecycle === "open" && row.priority <= 1), URGENT_SORT),
    [rows],
  );
  const p0 = urgent.filter(row => row.priority === 0).length;
  return <section className="work-urgent" aria-label="Urgent work">
    <h2>Urgent: {p0} P0, {urgent.length - p0} P1</h2>
    {urgent.length === 0 ? <p>No urgent work in the loaded rows.</p> : <ul className="work-urgent-list">
      {urgent.map(item => <li key={`${item.source}:${item.id}`}>
        <div className="work-urgent-row">
          <WorkRowLead item={item} />
          <WorkRowButton item={item} selected={isSelectedWorkItem(selection, item)} onSelect={onSelect} />
          {item.assignedTo && <span className="work-chip" title="Assignment is not the holder. Inspect details for the holder label.">{item.assignedTo}</span>}
        </div>
        {renderDetails(item)}
      </li>)}
    </ul>}
  </section>;
}
