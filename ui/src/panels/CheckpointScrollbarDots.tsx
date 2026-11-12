// Owns checkpoint buttons and the grouped chooser beside a native scrollbar.
// Does not own the thumb, wheel/keyboard scrolling, or history hydration.
// Replaces checkpoint presentation from ConversationOverviewRail.tsx.
import { useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import type { ConversationMarker } from "../types";
import { groupCheckpointScrollbarDots } from "./checkpoint-scrollbar-dots";
import "./checkpoint-scrollbar-dots.css";

type ScrollbarFrame = { top: number; left: number; height: number };

export function CheckpointScrollbarDots({
  markers, messageCount, scrollContainerRef, isActive, onJump,
}: {
  markers: readonly ConversationMarker[];
  messageCount: number;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  onJump: (marker: ConversationMarker) => void;
}) {
  const [frame, setFrame] = useState<ScrollbarFrame | null>(null);
  const [openGroup, setOpenGroup] = useState<string | null>(null);
  const chooserRef = useRef<HTMLDivElement | null>(null);
  const overlayRef = useRef<HTMLDivElement | null>(null);
  const groups = useMemo(() => groupCheckpointScrollbarDots(
    markers, messageCount, frame?.height ?? 0,
  ), [markers, messageCount, frame?.height]);
  const hasCheckpoints = markers.some(marker => marker.kind === "checkpoint");

  useLayoutEffect(() => {
    setOpenGroup(null);
    const node = scrollContainerRef.current;
    if (!isActive || !hasCheckpoints || !node) {
      setFrame(null);
      return;
    }
    let scheduled: number | null = null;
    const measure = () => {
      scheduled = null;
      const rect = node.getBoundingClientRect();
      const next = { top: rect.top + node.clientTop, left: rect.left + node.clientLeft + node.clientWidth - 7, height: node.clientHeight };
      setFrame(current => current?.top === next.top && current.left === next.left && current.height === next.height ? current : next);
    };
    const schedule = () => {
      if (scheduled === null) scheduled = window.requestAnimationFrame(measure);
    };
    const ancestorScroll = (event: Event) => {
      if (event.target !== node) schedule();
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(node);
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", ancestorScroll, true);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", ancestorScroll, true);
      if (scheduled !== null) window.cancelAnimationFrame(scheduled);
    };
  }, [hasCheckpoints, isActive, scrollContainerRef]);

  useLayoutEffect(() => {
    if (openGroup === null) return;
    chooserRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    const dismissOutside = (event: Event) => {
      if (event.target instanceof Node && !overlayRef.current?.contains(event.target)) {
        setOpenGroup(null);
      }
    };
    document.addEventListener("pointerdown", dismissOutside, true);
    document.addEventListener("focusin", dismissOutside);
    return () => {
      document.removeEventListener("pointerdown", dismissOutside, true);
      document.removeEventListener("focusin", dismissOutside);
    };
  }, [openGroup]);

  if (!isActive || !frame || groups.length === 0) return null;
  return createPortal(
    <div ref={overlayRef} className="checkpoint-scrollbar-dots" style={{ top: frame.top, left: frame.left, height: frame.height }}>
      {groups.map(group => {
        const first = group.markers[0];
        const multiple = group.markers.length > 1;
        const expanded = openGroup === first.id;
        return <div key={first.id} className="checkpoint-scrollbar-dot-group" style={{ top: group.topPx }}>
          <button type="button" className="checkpoint-scrollbar-dot"
            aria-label={multiple ? `${group.markers.length} checkpoints` : `Go to checkpoint ${first.name}`}
            title={multiple ? `${group.markers.length} checkpoints` : first.name}
            aria-haspopup={multiple ? "menu" : undefined}
            aria-expanded={multiple ? expanded : undefined}
            onClick={() => multiple ? setOpenGroup(expanded ? null : first.id) : onJump(first)}>
            {multiple ? group.markers.length : null}
          </button>
          {multiple && expanded ? <div ref={chooserRef} className={`checkpoint-scrollbar-chooser${group.topPx > frame.height / 2 ? " is-above" : ""}`} role="menu" aria-label="Choose checkpoint"
            onKeyDown={event => {
              if (event.key === "Escape") {
                event.preventDefault();
                setOpenGroup(null);
                event.currentTarget.parentElement?.querySelector<HTMLButtonElement>(".checkpoint-scrollbar-dot")?.focus();
              }
              const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button"));
              const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
              const next = event.key === "ArrowDown" ? (index + 1) % buttons.length : event.key === "ArrowUp" ? (index - 1 + buttons.length) % buttons.length : event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : null;
              if (next !== null) { event.preventDefault(); buttons[next]?.focus(); }
            }}>
            {group.markers.map(marker => <button key={marker.id} type="button" role="menuitem" onClick={() => {
              chooserRef.current?.parentElement?.querySelector<HTMLButtonElement>(".checkpoint-scrollbar-dot")?.focus();
              setOpenGroup(null);
              onJump(marker);
            }}>{marker.name}</button>)}
          </div> : null}
        </div>;
      })}
    </div>, document.body,
  );
}
