// Work's label chips and flat, collapsible label groups. Repeated membership
// is intentional, not a dependency edge; selection still uses source + ref.
import { useMemo, useState } from "react";
import type { WorkItem } from "../work-visualizer-api";
import { groupWorkLabels, workLabels } from "./work-labels";
import { WorkRowButton, WorkRowChips, WorkRowLead, isSelectedWorkItem, type WorkSelection } from "./WorkRow";

export function WorkLabelChips({ item, onLabel }: { item: WorkItem; onLabel?: (label: string) => void }) {
  const labels = workLabels(item);
  if (!labels.length) return null;
  return <div className="work-label-chips" aria-label={`Labels for ${item.shortRef}`}>
    {labels.map(label => onLabel
      ? <button key={label} type="button" className="work-label-chip" title={`Filter loaded items by label: ${label}`}
        aria-label={`Filter by label ${label}`} onClick={() => onLabel(label)}>{label || "(empty label)"}</button>
      : <span key={label} className="work-label-chip">{label || "(empty label)"}</span>)}
  </div>;
}

export function WorkLabels({ rows, mixedSources, selection, onSelect, onLabel }: {
  rows: readonly WorkItem[]; mixedSources: boolean; selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void; onLabel: (label: string) => void;
}) {
  const groups = useMemo(() => groupWorkLabels(rows), [rows]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  return <section className="work-label-groups" aria-label="Work grouped by label">
    <p className="work-panel-caption">{rows.length} unique items · multi-label items appear in each group. Label groups are not dependencies.</p>
    {groups.length > 0 && <div className="work-panel-actions">
      <button type="button" onClick={() => setCollapsed(new Set())}>Expand all labels</button>
      <button type="button" onClick={() => setCollapsed(new Set(groups.map(group => group.key)))}>Collapse all labels</button>
    </div>}
    {groups.map(group => <section className="work-label-group" key={group.key} aria-label={`Label group: ${group.label}`}>
      <h3><button type="button" aria-expanded={!collapsed.has(group.key)} onClick={() => setCollapsed(current => {
        const next = new Set(current);
        if (next.has(group.key)) next.delete(group.key); else next.add(group.key);
        return next;
      })}><span aria-hidden="true">{collapsed.has(group.key) ? "▸" : "▾"}</span> {group.label || "(empty label)"} <span className="work-label-count">{group.items.length}</span></button></h3>
      {!collapsed.has(group.key) && <ul>{group.items.map(item => <li key={`${item.source}:${item.id}`}>
        <div className="work-node-row"><div className="work-node-main">
          <WorkRowLead item={item} /><WorkRowButton item={item} selected={isSelectedWorkItem(selection, item)} onSelect={onSelect} />
        </div><WorkRowChips item={item} mixedSources={mixedSources} /></div>
        <WorkLabelChips item={item} onLabel={onLabel} />
      </li>)}</ul>}
    </section>)}
  </section>;
}
