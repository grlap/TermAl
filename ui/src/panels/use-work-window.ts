// Work row windowing shared by the tree, table and label views. Extracted
// from their row rendering responsibility; owns measurements/scroll anchors,
// never source pagination, relation building or selection policy.
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";

export type WorkViewport = RefObject<HTMLDivElement | null>;
export const WORK_WINDOW_ROWS = 60;
const ESTIMATE = 96;
// Compare geometry at browser layout precision, without changing user scrolls.
const layoutPixel = (value: number) => Math.round(value * 64) / 64;

export function useWorkWindow(keys: readonly string[], viewport?: WorkViewport, selectedKey?: string) {
  const content = useRef<HTMLElement | null>(null);
  const heights = useRef(new Map<string, number>());
  const nodes = useRef(new Map<string, HTMLElement>());
  const callbacks = useRef(new Map<string, (node: HTMLElement | null) => void>());
  const observer = useRef<ResizeObserver | null>(null);
  const measuredWidth = useRef<number>();
  const [revision, setRevision] = useState(0);
  const [view, setView] = useState({ top: 0, height: 600 });
  const reportedView = useRef(view);
  const [focused, setFocused] = useState<string>();
  const enabled = !!viewport && keys.length > WORK_WINDOW_ROWS;
  const offsets = useMemo(() => {
    const result = [0];
    for (const key of keys) result.push(result[result.length - 1] + (heights.current.get(key) ?? ESTIMATE));
    return result;
  }, [keys, revision]);
  const layout = useRef({ keys, offsets, enabled });
  // The long-lived scroll/resize listeners use the latest committed offsets.
  useLayoutEffect(() => { layout.current = { keys, offsets, enabled }; });
  const anchor = useRef<{ key: string; top: number }>();
  const pendingReveal = useRef<string>();
  const origin = () => {
    const root = viewport?.current, element = content.current;
    return root && element ? element.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop : 0;
  };
  const visibleRow = () => {
    const root = viewport?.current;
    if (!root) return;
    const top = root.getBoundingClientRect().top;
    const visible = [...nodes.current].map(([key, node]) => ({ key, rect: node.getBoundingClientRect() }))
      .filter(({ rect }) => rect.height > 0 && rect.bottom > top && rect.top < top + (root.clientHeight || 600))
      .sort((a, b) => a.rect.top - b.rect.top)[0];
    return visible ? { key: visible.key, top: visible.rect.top - top } : undefined;
  };
  const capture = () => { anchor.current = visibleRow(); };
  const read = () => {
    const root = viewport?.current;
    if (!root) return;
    const visible = visibleRow();
    const index = visible ? layout.current.keys.indexOf(visible.key) : -1;
    // DOM spacers include estimated heights. Project the visible row into
    // that same coordinate system, so restoring its position cannot select
    // a different window merely because mounted rows have real heights.
    const top = layout.current.enabled && visible && index >= 0
      ? layout.current.offsets[index] - visible.top : root.scrollTop - origin();
    const next = { top: layoutPixel(Math.max(0, top)), height: root.clientHeight || 600 };
    if (reportedView.current.top === next.top && reportedView.current.height === next.height) return;
    reportedView.current = next;
    setView(next);
  };
  const firstAt = (positions: readonly number[], top: number) => {
    let low = 0, high = positions.length - 1;
    while (low < high) { const mid = (low + high) >>> 1; if (positions[mid + 1] <= top) low = mid + 1; else high = mid; }
    return low;
  };
  useLayoutEffect(() => {
    const root = viewport?.current;
    if (!root) return;
    if (!enabled) {
      // Keep the real prefix before crossing the windowing threshold, even
      // when an appended page arrives before ResizeObserver's first delivery.
      let changed = false;
      for (const [key, node] of nodes.current) {
        const height = layoutPixel(node.getBoundingClientRect().height);
        if (height > 0 && heights.current.get(key) !== height) { heights.current.set(key, height); changed = true; }
      }
      if (changed) setRevision(value => value + 1);
    }
    const target = pendingReveal.current && nodes.current.get(pendingReveal.current);
    if (target) {
      // Consume before read schedules the render; reveal is a one-shot action.
      pendingReveal.current = undefined;
      root.scrollTop += target.getBoundingClientRect().top - root.getBoundingClientRect().top;
    } else if (!pendingReveal.current) {
      const saved = anchor.current, node = saved && nodes.current.get(saved.key);
      if (saved && node) {
        const delta = layoutPixel(node.getBoundingClientRect().top - root.getBoundingClientRect().top - saved.top);
        if (delta) root.scrollTop += delta;
      }
    }
    read();
    capture();
  });
  useEffect(() => {
    const root = viewport?.current;
    if (!root) return;
    const focus = (event: Event) => {
      const row = (event.target as Element).closest<HTMLElement>("[data-work-window-key]");
      setFocused(row?.dataset.workWindowKey);
    };
    const scroll = () => { read(); capture(); };
    root.addEventListener("scroll", scroll, { passive: true });
    root.addEventListener("focusin", focus);
    if (typeof ResizeObserver !== "undefined") {
      observer.current = new ResizeObserver(entries => {
        let changed = false;
        const width = layoutPixel(root.getBoundingClientRect().width);
        const resized = measuredWidth.current !== undefined && measuredWidth.current !== width;
        measuredWidth.current = width;
        if (resized) { heights.current.clear(); changed = true; }
        for (const node of resized ? nodes.current.values() : entries.map(entry => entry.target as HTMLElement)) {
          const key = node.dataset.workWindowKey;
          const height = layoutPixel(node.getBoundingClientRect().height);
          if (key && height > 0 && heights.current.get(key) !== height) { heights.current.set(key, height); changed = true; }
        }
        if (changed) setRevision(value => value + 1);
        read();
      });
      observer.current.observe(root);
      for (const node of nodes.current.values()) observer.current.observe(node);
    }
    read();
    return () => { root.removeEventListener("scroll", scroll); root.removeEventListener("focusin", focus); observer.current?.disconnect(); observer.current = null; };
  }, [viewport]);
  // Forget retired query/forest occurrences; caches must not grow with every
  // collapse, regrouping or filter edit over the lifetime of the panel.
  useEffect(() => {
    const current = new Set(keys);
    for (const key of heights.current.keys()) if (!current.has(key)) heights.current.delete(key);
    for (const key of callbacks.current.keys()) if (!current.has(key)) callbacks.current.delete(key);
  }, [keys]);
  const indices = useMemo(() => {
    if (!enabled) return keys.map((_, index) => index);
    const start = Math.max(0, firstAt(offsets, view.top) - 4);
    const end = Math.min(keys.length, start + WORK_WINDOW_ROWS, firstAt(offsets, view.top + view.height) + 5);
    const visible = new Set(Array.from({ length: Math.max(0, end - start) }, (_, index) => start + index));
    for (const key of [selectedKey, focused, pendingReveal.current ?? anchor.current?.key]) { const index = key ? keys.indexOf(key) : -1; if (index >= 0) visible.add(index); }
    return [...visible].sort((a, b) => a - b);
  }, [enabled, keys, offsets, view, selectedKey, focused]);
  return {
    content: (node: HTMLElement | null) => { content.current = node; }, enabled, indices,
    before: (position: number) => enabled ? offsets[indices[position]] - offsets[position ? indices[position - 1] + 1 : 0] : 0,
    after: enabled ? offsets[keys.length] - offsets[(indices[indices.length - 1] ?? -1) + 1] : 0,
    rowRef: (key: string) => {
      if (!callbacks.current.has(key)) callbacks.current.set(key, node => {
        const old = nodes.current.get(key);
        if (old) observer.current?.unobserve(old);
        if (node) { node.dataset.workWindowKey = key; nodes.current.set(key, node); observer.current?.observe(node); }
        else nodes.current.delete(key);
      });
      return callbacks.current.get(key)!;
    },
    reveal: (key: string) => {
      const index = keys.indexOf(key), root = viewport?.current;
      if (index >= 0 && root) { pendingReveal.current = key; anchor.current = undefined; root.scrollTop = origin() + offsets[index]; read(); }
    },
  };
}
