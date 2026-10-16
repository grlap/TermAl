// New flat table view for Work rows. Owns column presentation and the sortable
// headers over the caller's one sort model; filtering, sorting and selection
// state belong to the caller. Moved out of WorkPanel.tsx when the tree views
// were added and extended in the same change with the source, waits-for and
// parent columns (not a pure move).
import { Fragment, useMemo } from "react";
import { useWorkWindow, type WorkViewport } from "./use-work-window";
import type { WorkItem } from "../work-visualizer-api";
import { WORK_SORT_COLUMNS, nextWorkSort, type WorkSort, type WorkSortKey } from "./work-sort";
import { WorkTime } from "./work-time";
import { WorkRowUpdated, WorkRowButton, isSelectedWorkItem, type WorkSelection, type WorkDetailsRenderer } from "./WorkRow";
import { WorkLabelChips } from "./WorkLabels";

// Virtual rows must share one column layout: mounted content cannot change
// widths and invalidate the heights of rows that have already been measured.
const COLUMN_WIDTHS: Record<WorkSortKey, string> = {
  priority: "5%", title: "27%", source: "6%", kind: "6%", lifecycle: "7%",
  availability: "8%", waitsFor: "16%", assignment: "10%", updated: "15%",
};

export function WorkTable({ rows, selection, onSelect, sort, onSortChange, onLabel, renderDetails, viewport }: {
  rows: readonly WorkItem[]; selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void;
  sort: WorkSort | null; onSortChange: (sort: WorkSort) => void;
  onLabel?: (label: string) => void;
  renderDetails?: WorkDetailsRenderer;
  viewport?: WorkViewport;
}) {
  const keys = useMemo(() => rows.map(row => `${row.source}:${row.id}`), [rows]);
  const selected = rows.findIndex(row => isSelectedWorkItem(selection, row));
  const window = useWorkWindow(keys, viewport, keys[selected]);
  const spacer = (height: number) => height > 0 && <tbody aria-hidden="true"><tr><td colSpan={WORK_SORT_COLUMNS.length} style={{ height, padding: 0, border: 0 }} /></tr></tbody>;
  return <div ref={window.content} className="work-table-scroll" role="region" tabIndex={0} aria-label="Work table scroll area">
    <table className="work-table"><caption>Work items</caption>
      <colgroup>{WORK_SORT_COLUMNS.map(column => <col key={column.key} style={{ width: COLUMN_WIDTHS[column.key] }} />)}</colgroup>
      <thead><tr>
      {WORK_SORT_COLUMNS.map(column => {
        const direction = sort?.key === column.key ? sort.direction : null;
        return <th key={column.key} scope="col" aria-sort={direction === "asc" ? "ascending" : direction === "desc" ? "descending" : "none"}>
          <button type="button" className="work-table-sort" onClick={() => onSortChange(nextWorkSort(sort, column.key))}
            title={direction ? `Sorted ${direction === "asc" ? "ascending" : "descending"} — click to reverse` : "Sort the loaded rows by this column"}>
            {column.label}{direction && <span aria-hidden="true">{direction === "asc" ? " ▲" : " ▼"}</span>}
          </button>
        </th>;
      })}
    </tr></thead>{window.indices.map((index, position) => {
      const row = rows[index];
      const details = renderDetails?.(row);
      return <Fragment key={keys[index]}>{spacer(window.before(position))}<tbody ref={window.rowRef(keys[index])}><tr>
      <td>P{row.priority}</td>
      <td><WorkRowButton item={row} selected={isSelectedWorkItem(selection, row)} onSelect={onSelect} />
        <div className="work-row-secondary"><WorkLabelChips item={row} onLabel={onLabel} /><WorkRowUpdated item={row} /></div>
        {row.parentId && <small>in {row.parentId}</small>}</td>
      <td>{row.source}</td><td>{row.kind}</td><td>{row.lifecycle}</td><td>{row.availability}</td>
      <td>{row.prerequisites.length ? row.prerequisites.map(p => `${p.id}${p.satisfied ? " (satisfied)" : ""}`).join(", ") : "—"}</td>
      <td title="Assignment is not the holder. Inspect details for the holder label.">{row.assignedTo ?? "—"}</td><td className="work-table-time"><WorkTime value={row.updatedAt} /></td>
    </tr>{details && <tr className="work-table-details"><td colSpan={WORK_SORT_COLUMNS.length}><div className="work-table-details-content">{details}</div></td></tr>}</tbody></Fragment>;
    })}{spacer(window.after)}</table>
  </div>;
}
