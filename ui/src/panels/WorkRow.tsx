// New shared row presentation for the Work tree and table. Owns the
// accessible row name ("ref — title") and the state chips; does not own
// selection state, fetching, or the relation a row is nested under.
// Split out of WorkPanel.tsx (its table's row button and chips).
import type { MouseEvent } from "react";
import type { WorkItem } from "../work-visualizer-api";

export type WorkSelection = { source: string; ref: string };

export function isSelectedWorkItem(selection: WorkSelection | null, item: WorkItem) {
  return !!selection && selection.source === item.source && selection.ref === item.shortRef;
}

export function WorkRowButton({ item, selected, onSelect }: {
  item: WorkItem; selected: boolean; onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void;
}) {
  return <button type="button" className="work-row-title" aria-current={selected ? "true" : undefined}
    onClick={(event: MouseEvent<HTMLButtonElement>) => onSelect(item, event.currentTarget)}>
    {item.shortRef} — {item.title}
  </button>;
}

export function WorkRowChips({ item }: { item: WorkItem }) {
  return <>
    <span className="work-chip" data-priority={item.priority}>P{item.priority}</span>
    <span className="work-chip" data-state={item.availability}>{item.availability}</span>
    <span className="work-chip work-chip-source" data-source={item.source}>{item.source}</span>
    {item.assignedTo && <span className="work-chip" title="Assignment is not the holder. Inspect details for the holder label.">{item.assignedTo}</span>}
  </>;
}
