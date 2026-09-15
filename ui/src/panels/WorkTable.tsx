// New flat table view for Work rows. Owns column presentation and the sortable
// headers over the caller's one sort model; filtering, sorting and selection
// state belong to the caller. Moved out of WorkPanel.tsx when the tree views
// were added and extended in the same change with the source, waits-for and
// parent columns (not a pure move).
import type { WorkItem } from "../work-visualizer-api";
import { WORK_SORT_COLUMNS, nextWorkSort, type WorkSort } from "./work-sort";
import { WorkTime } from "./work-time";
import { WorkRowButton, isSelectedWorkItem, type WorkSelection } from "./WorkRow";

export function WorkTable({ rows, selection, onSelect, sort, onSortChange }: {
  rows: readonly WorkItem[]; selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void;
  sort: WorkSort | null; onSortChange: (sort: WorkSort) => void;
}) {
  return <div className="work-table-scroll" role="region" tabIndex={0} aria-label="Work table scroll area">
    <table className="work-table"><caption>Work items</caption><thead><tr>
      {WORK_SORT_COLUMNS.map(column => {
        const direction = sort?.key === column.key ? sort.direction : null;
        return <th key={column.key} scope="col" aria-sort={direction === "asc" ? "ascending" : direction === "desc" ? "descending" : "none"}>
          <button type="button" className="work-table-sort" onClick={() => onSortChange(nextWorkSort(sort, column.key))}
            title={direction ? `Sorted ${direction === "asc" ? "ascending" : "descending"} — click to reverse` : "Sort the loaded rows by this column"}>
            {column.label}{direction && <span aria-hidden="true">{direction === "asc" ? " ▲" : " ▼"}</span>}
          </button>
        </th>;
      })}
    </tr></thead><tbody>{rows.map(row => <tr key={`${row.source}:${row.id}`}>
      <td>P{row.priority}</td>
      <td><WorkRowButton item={row} selected={isSelectedWorkItem(selection, row)} onSelect={onSelect} />
        {!!row.labels.length && <small>{row.labels.join(", ")}</small>}
        {row.parentId && <small>in {row.parentId}</small>}</td>
      <td>{row.source}</td><td>{row.kind}</td><td>{row.lifecycle}</td><td>{row.availability}</td>
      <td>{row.prerequisites.length ? row.prerequisites.map(p => `${p.id}${p.satisfied ? " (satisfied)" : ""}`).join(", ") : "—"}</td>
      <td title="Assignment is not the holder. Inspect details for the holder label.">{row.assignedTo ?? "—"}</td><td className="work-table-time"><WorkTime value={row.updatedAt} /></td>
    </tr>)}</tbody></table>
  </div>;
}
