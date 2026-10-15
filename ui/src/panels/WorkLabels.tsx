// Work's label chips and collapsible groups. Group membership stays logical
// while the shared measured window owns which occurrences are mounted.
import { Fragment, useMemo, useState } from "react";
import { useWorkWindow, type WorkViewport } from "./use-work-window";
import type { WorkItem } from "../work-visualizer-api";
import { groupWorkLabels, workLabels } from "./work-labels";
import { WorkRowUpdated, WorkRowButton, WorkRowChips, WorkRowLead, isSelectedWorkItem, type WorkSelection, type WorkDetailsRenderer } from "./WorkRow";

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

export function WorkLabels({ rows, mixedSources, selection, onSelect, onLabel, renderDetails, onHideSelection, viewport }: {
  rows: readonly WorkItem[]; mixedSources: boolean; selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement, group?: string) => void; onLabel: (label: string) => void;
  renderDetails?: WorkDetailsRenderer; onHideSelection?: () => void; viewport?: WorkViewport;
}) {
  const groups = useMemo(() => groupWorkLabels(rows), [rows]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const entries = useMemo(() => groups.flatMap(group => [
    { key: `heading:${group.key}`, group, item: null as WorkItem | null },
    ...(collapsed.has(group.key) ? [] : group.items.map(item => ({ key: JSON.stringify([group.key, item.source, item.id]), group, item }))),
  ]), [groups, collapsed]);
  const keys = useMemo(() => entries.map(entry => entry.key), [entries]);
  const selectedKey = entries.find(entry => entry.item && entry.group.key === selection?.group && isSelectedWorkItem(selection, entry.item))?.key;
  const window = useWorkWindow(keys, viewport, selectedKey);
  const visibleGroups = new Map<string, { group: typeof groups[number]; entries: { index: number; position: number }[] }>();
  window.indices.forEach((index, position) => {
    const group = entries[index].group;
    if (!visibleGroups.has(group.key)) visibleGroups.set(group.key, { group, entries: [] });
    visibleGroups.get(group.key)!.entries.push({ index, position });
  });
  const toggle = (key: string, button: HTMLButtonElement) => {
    if (!collapsed.has(key) && selection?.group === key) { onHideSelection?.(); button.focus({ preventScroll: true }); }
    setCollapsed(current => { const next = new Set(current); if (next.has(key)) next.delete(key); else next.add(key); return next; });
  };
  return <section className="work-label-groups" aria-label="Work grouped by label">
    <p className="work-panel-caption">{rows.length} unique items · multi-label items appear in each group. Label groups are not dependencies.</p>
    {groups.length > 0 && <div className="work-panel-actions">
      <button type="button" onClick={() => setCollapsed(new Set())}>Expand all labels</button>
      <button type="button" onClick={event => {
        if (selection) onHideSelection?.();
        event.currentTarget.focus({ preventScroll: true });
        setCollapsed(new Set(groups.map(group => group.key)));
      }}>Collapse all labels</button>
    </div>}
    <div ref={window.content} className="work-label-window">
      {[...visibleGroups.values()].map(({ group, entries: visible }) => <section key={group.key}
        className="work-label-group" data-work-group={group.key} aria-label={`Label group: ${group.label}`} style={{ display: "contents" }}>
        {visible.map(({ index, position }) => {
          const { key, item } = entries[index];
          return <Fragment key={key}>
            {window.before(position) > 0 && <div aria-hidden="true" style={{ height: window.before(position) }} />}
            {item ? <ul style={{ display: "contents" }}><li ref={window.rowRef(key)}>
              <div className="work-node-row"><div className="work-node-main">
                <WorkRowLead item={item} /><WorkRowButton item={item} selected={isSelectedWorkItem(selection, item) && selection?.group === group.key} onSelect={(row, trigger) => onSelect(row, trigger, group.key)} />
              </div><WorkRowChips item={item} mixedSources={mixedSources} /></div>
              <div className="work-row-secondary"><WorkLabelChips item={item} onLabel={onLabel} /><WorkRowUpdated item={item} /></div>{renderDetails?.(item, group.key)}
            </li></ul> : <h3 ref={window.rowRef(key)}><button type="button" aria-expanded={!collapsed.has(group.key)} onClick={event => toggle(group.key, event.currentTarget)}>
              <span aria-hidden="true">{collapsed.has(group.key) ? "▸" : "▾"}</span> {group.label || "(empty label)"} <span className="work-label-count">{group.items.length}</span>
            </button></h3>}
          </Fragment>;
        })}
      </section>)}
      {window.after > 0 && <div aria-hidden="true" style={{ height: window.after }} />}
    </div>
  </section>;
}
