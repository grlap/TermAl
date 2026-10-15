// New nested list renderer for the dependency and hierarchy Work views. Owns
// expand/collapse state per node path and memoises the forest per row set;
// does not build the forest (see work-tree.ts), fetch, or decide which
// relation the caller shows.
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useWorkWindow, type WorkViewport } from "./use-work-window";
import type { WorkItem } from "../work-visualizer-api";
import { MAX_TREE_DEPTH, buildDependencyForest, buildHierarchyForest, workItemKey, type WorkTreeNode } from "./work-tree";
import { WorkRowUpdated, WorkRowButton, WorkRowChips, WorkRowLead, isSelectedWorkItem, type WorkSelection, type WorkDetailsRenderer } from "./WorkRow";
import { WorkLabelChips } from "./WorkLabels";

export type WorkTreeMode = "dependencies" | "hierarchy";

export function WorkTree({ rows, universe, mode, selection, onSelect, onLabel, renderDetails, onHideSelection, viewport }: {
  rows: readonly WorkItem[]; universe: readonly WorkItem[]; mode: WorkTreeMode; selection: WorkSelection | null;
  onSelect: (item: WorkItem, trigger: HTMLButtonElement) => void;
  onLabel?: (label: string) => void;
  renderDetails?: WorkDetailsRenderer; onHideSelection?: () => void;
  viewport?: WorkViewport;
}) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const forest = useMemo(
    () => (mode === "dependencies" ? buildDependencyForest(rows, universe) : buildHierarchyForest(rows, universe)),
    [rows, universe, mode],
  );
  // The source is worth a glyph only when the loaded rows come from more
  // than one tracker.
  const mixedSources = useMemo(() => new Set(universe.map(row => row.source)).size > 1, [universe]);
  const toggle = (key: string) => setCollapsed(current => {
    const next = new Set(current);
    if (next.has(key)) next.delete(key); else next.add(key);
    return next;
  });
  // A repeated reference has one canonical row elsewhere in this forest,
  // possibly under a collapsed branch: revealing expands every collapsed
  // ancestor of that row and moves focus to it, so the reference is never a
  // dead end.
  const [pendingReveal, setPendingReveal] = useState<string | null>(null);
  const section = useRef<HTMLElement | null>(null);
  const entries = useMemo(() => {
    const result: { key: string; node?: WorkTreeNode; depth: number; heading?: string }[] = [];
    const append = (roots: WorkTreeNode[]) => {
      const pending = roots.map(node => ({ node, depth: 0 })).reverse();
      while (pending.length) {
        const entry = pending.pop()!;
        result.push({ ...entry, key: workItemKey(entry.node.item) });
        if (!collapsed.has(entry.node.key)) for (let i = entry.node.children.length - 1; i >= 0; i--)
          pending.push({ node: entry.node.children[i], depth: entry.depth + 1 });
      }
    };
    result.push({ key: "@roots", depth: 0, heading: mode === "dependencies"
      ? `Blocked chains · ${forest.roots.length} ${forest.roots.length === 1 ? "goal" : "goals"}` : `${forest.roots.length} ${forest.roots.length === 1 ? "root" : "roots"} · nested = subtask of` });
    append(forest.roots);
    if (mode === "dependencies") { result.push({ key: "@loose", depth: 0, heading: `No visible dependency links · ${forest.loose.length} items` }); append(forest.loose); }
    return result;
  }, [forest, collapsed, mode]);
  const keys = useMemo(() => entries.map(entry => entry.key), [entries]);
  const selectedKey = entries.find(entry => entry.node && isSelectedWorkItem(selection, entry.node.item))?.key;
  const window = useWorkWindow(keys, viewport, selectedKey);
  const reveal = (itemKeys: readonly string[]) => {
    const target = itemKeys.map(key => forest.placedAt.get(key)).find((nodeKey): nodeKey is string => !!nodeKey);
    if (!target) return;
    setCollapsed(current => {
      const next = new Set([...current].filter(key => !target.startsWith(`${key}/`)));
      return next.size === current.size ? current : next;
    });
    setPendingReveal(target);
  };
  useEffect(() => {
    if (!pendingReveal) return;
    for (const row of section.current?.querySelectorAll<HTMLLIElement>("li[data-node-key]") ?? []) {
      if (row.getAttribute("data-node-key") !== pendingReveal) continue;
      // The first row div inside the node is its own; nested nodes follow it.
      const title = row.querySelector<HTMLButtonElement>(".work-node-row > .work-node-main > .work-row-title");
      title?.focus();
      title?.scrollIntoView?.({ block: "nearest" });
      setPendingReveal(null);
      return;
    }
    const entry = entries.find(entry => entry.node?.key === pendingReveal);
    if (entry) window.reveal(entry.key);
  }, [pendingReveal, window.indices]);
  const renderNode = (node: WorkTreeNode, depth = 0) => {
    const open = !collapsed.has(node.key);
    const allSatisfied = node.children.length > 0 && node.children.every(child => child.satisfied === true);
    return <li key={workItemKey(node.item)} data-node-key={node.key} ref={viewport ? window.rowRef(workItemKey(node.item)) : undefined}
      role={viewport ? "treeitem" : undefined} aria-level={viewport ? depth + 1 : undefined}
      aria-expanded={viewport && node.children.length ? open : undefined}
      style={viewport ? { paddingInlineStart: `${depth * 1.4}rem`, paddingBottom: 6 } : undefined}
      className={`work-node${node.item.lifecycle === "completed" ? " is-completed" : ""}`}>
      <div className="work-node-heading">
          {node.children.length > 0
            ? <button type="button" className="work-node-toggle" aria-expanded={open}
              aria-label={`${open ? "Collapse" : "Expand"} ${node.item.shortRef}`} onClick={event => {
                const selectedDescendant = (children: WorkTreeNode[]): boolean => children.some(child => isSelectedWorkItem(selection, child.item) || selectedDescendant(child.children));
                if (open && selectedDescendant(node.children)) { onHideSelection?.(); event.currentTarget.focus({ preventScroll: true }); }
                toggle(node.key);
              }}>▾</button>
            : <span className="work-node-toggle work-node-leaf" aria-hidden="true" />}
          <WorkRowLead item={node.item} />
        <div className="work-node-content">
        <div className="work-node-row">
        <div className="work-node-main">
          <strong className="work-row-ref">{node.item.shortRef}</strong>
          <WorkRowButton item={node.item} selected={isSelectedWorkItem(selection, node.item)} onSelect={onSelect} showRef={false} />
        </div>
        <div className="work-node-metadata">
        <WorkRowChips item={node.item} mixedSources={mixedSources} />
        {mode === "dependencies" && node.satisfied === true && <span className="work-chip" data-satisfied="true">satisfied</span>}
        {node.repeatedCount > 0 && <button type="button" className="work-tree-relation" data-repeated="true"
          title="Reveal that row (expanding its branch) and move focus to it" onClick={() => reveal(node.repeatedItems)}>
          +{node.repeatedCount} shown above ({node.repeatedPreview.join(", ")}{node.repeatedCount > node.repeatedPreview.length ? ", …" : ""})
        </button>}
        {node.deeperCount > 0 && <span className="work-tree-relation" data-deeper="true">
          +{node.deeperCount} shown as {node.deeperCount === 1 ? "a later root" : "later roots"} — nesting stops at {MAX_TREE_DEPTH} levels
        </span>}
        {mode === "dependencies" && node.absentUnsatisfied > 0 && <span className="work-tree-relation">
          waits for {node.absentUnsatisfied} not loaded
        </span>}
        {mode === "dependencies" && node.absentSatisfied > 0 && <span className="work-tree-relation" data-satisfied="true">
          {node.absentSatisfied} satisfied · not loaded
        </span>}
        {node.hiddenByFilter > 0 && <span className="work-tree-relation" data-hidden-by-filter="true">
          {mode === "dependencies" ? `${node.hiddenByFilter} hidden by filter` : "parent hidden by filter"}
        </span>}
        </div>
      </div>
      <div className="work-row-secondary"><WorkLabelChips item={node.item} onLabel={onLabel} /><WorkRowUpdated item={node.item} /></div>
        </div>
      </div>
      {renderDetails?.(node.item)}
      {node.children.length > 0 && open && viewport && mode === "dependencies" && <p className="work-tree-relation">{allSatisfied ? "all prerequisites satisfied" : "waits for"}</p>}
      {node.children.length > 0 && open && !viewport && <>
        {mode === "dependencies" && <p className="work-tree-relation">{allSatisfied ? "all prerequisites satisfied" : "waits for"}</p>}
        <ul data-relation={mode} data-satisfied={allSatisfied ? "true" : undefined}>{node.children.map(renderNode)}</ul>
      </>}
    </li>;
  };
  return <section ref={node => { section.current = node; window.content(node); }} className="work-tree" aria-label={mode === "dependencies" ? "Dependency tree" : "Hierarchy tree"}>
    {viewport ? <ul role="tree" data-relation={mode}>
      {window.indices.map((index, position) => <Fragment key={entries[index].key}>
        {window.before(position) > 0 && <li aria-hidden="true" style={{ height: window.before(position) }} />}
        {entries[index].node ? renderNode(entries[index].node!, entries[index].depth)
          : <li role="presentation" ref={window.rowRef(entries[index].key)}><p className="work-tree-group">{entries[index].heading}</p></li>}
      </Fragment>)}
      {window.after > 0 && <li aria-hidden="true" style={{ height: window.after }} />}
    </ul> : <>
    {mode === "dependencies" ? <>
      <p className="work-tree-group">Blocked chains · {forest.roots.length} {forest.roots.length === 1 ? "goal" : "goals"}</p>
      {forest.roots.length === 0 && <p className="work-panel-caption">No blocking relations among the visible rows.</p>}
      <ul data-relation="dependencies">{forest.roots.map(renderNode)}</ul>
      <p className="work-tree-group">No visible dependency links · {forest.loose.length} {forest.loose.length === 1 ? "item" : "items"}</p>
      <ul>{forest.loose.map(renderNode)}</ul>
    </> : <>
      <p className="work-tree-group">{forest.roots.length} {forest.roots.length === 1 ? "root" : "roots"} · nested = subtask of · blocking relations live in the Dependencies view</p>
      <ul data-relation="hierarchy">{forest.roots.map(renderNode)}</ul>
    </>}
    </>}
  </section>;
}
