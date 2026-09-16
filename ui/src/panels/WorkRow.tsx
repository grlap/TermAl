// New shared row presentation for the Work tree and table. Owns the
// accessible row name ("ref — title"), the lead before it (priority coloured
// by availability) and the trailing chips (source icon, assignment); does not
// own selection state, fetching, or the relation a row is nested under.
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

// Before the title: one chip carrying the priority as text and the
// availability as its colour. The availability word stays on hover and as
// hidden text, so colour is never the only carrier.
export function WorkRowLead({ item }: { item: WorkItem }) {
  return <span className="work-chip work-priority" data-priority={item.priority} data-state={item.availability} title={item.availability}>
    P{item.priority}<span className="visually-hidden">, {item.availability}</span>
  </span>;
}

// After the title: the source, only when the loaded rows mix sources (a
// single-source project has nothing to tell apart), then the assignment.
export function WorkRowChips({ item, mixedSources }: { item: WorkItem; mixedSources: boolean }) {
  return <>
    {mixedSources && <WorkSourceIcon source={item.source} />}
    {item.assignedTo && <span className="work-chip" title="Assignment is not the holder. Inspect details for the holder label.">{item.assignedTo}</span>}
  </>;
}

const SOURCE_LABELS: Record<string, string> = { engram: "Engram", beads: "Beads" };

// Beads: three beads on a string. Engram: a cell with its nucleus. A source
// this UI does not know keeps its name as text rather than an invented glyph.
export function WorkSourceIcon({ source }: { source: string }) {
  const label = SOURCE_LABELS[source];
  if (!label) return <span className="work-chip work-chip-source" data-source={source}>{source}</span>;
  return <span className="work-source" data-source={source} role="img" aria-label={`Source: ${label}`} title={label}>
    {source === "beads"
      ? <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
        <path d="M1 8h14" stroke="currentColor" strokeWidth="1" />
        <circle cx="4" cy="8" r="2.2" fill="currentColor" /><circle cx="8" cy="8" r="2.2" fill="currentColor" /><circle cx="12" cy="8" r="2.2" fill="currentColor" />
      </svg>
      : <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
        <path d="M8 1.5 13.6 4.75v6.5L8 14.5 2.4 11.25v-6.5Z" fill="none" stroke="currentColor" strokeWidth="1.2" />
        <circle cx="8" cy="8" r="2.2" fill="currentColor" />
      </svg>}
  </span>;
}
