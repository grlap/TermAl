// Owns native scrollbar extent for nonresident history and bounded position seeks.
// Does not replace native input, pane FOLLOW/STAY, or checkpoint selection.
// Replaces history-navigation wiring from conversation-overview-controller.ts.
import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import {
  MESSAGE_STACK_NAVIGATION_START_EVENT,
  MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  markMessageStackVirtualizerPositionCorrection,
  writeMessageStackScrollTopImmediately,
  type MessageStackUserScrollIntentDetail,
  type MessageStackScrollWriteDetail,
} from "../message-stack-scroll-sync";
import { useCommittedRef } from "./use-committed-ref";
import { createConversationPositionNavigation, type PositionNavigationContext } from "./conversation-position-navigation";
import { historyScrollExtent, unloadedHistoryPosition } from "./conversation-history-scroll-geometry";
import type { VirtualizedConversationMessageListHandle } from "./virtualized-conversation-types";

type PendingPrefixCorrection = {
  top: number;
  generation: number | undefined;
  delta: number;
  scope: string;
  sessionId: string;
  start: number;
  resident: number;
};

export function useConversationHistoryScroll(options: Omit<PositionNavigationContext, "virtualizerHandleRef"> & {
  scrollContainerRef: RefObject<HTMLElement | null>;
  virtualizerHandleRef?: PositionNavigationContext["virtualizerHandleRef"];
  tailFollowIntent?: boolean;
}) {
  const internalHandle = useRef<VirtualizedConversationMessageListHandle | null>(null);
  const handle = options.virtualizerHandleRef ?? internalHandle;
  const contentRef = useRef<HTMLDivElement | null>(null);
  const current = useCommittedRef({ ...options, virtualizerHandleRef: handle });
  const [extent, setExtent] = useState(() => ({ ...historyScrollExtent(options.messageCount, options.messageStartIndex, options.renderedMessageCount, 0), scope: options.scrollStateKey, sessionId: options.sessionId }));
  const extentRef = useCommittedRef(extent);
  const navigationRef = useRef<ReturnType<typeof createConversationPositionNavigation> | null>(null);
  if (navigationRef.current === null) navigationRef.current = createConversationPositionNavigation(() => current.current);
  const navigation = navigationRef.current;
  const restoreRef = useRef<PendingPrefixCorrection | null>(null);

  const measure = () => {
    const context = current.current;
    const snapshot = handle.current?.getLayoutSnapshot();
    const next = { ...historyScrollExtent(context.messageCount, context.messageStartIndex, context.renderedMessageCount,
      snapshot?.sessionId === context.sessionId ? (snapshot.estimatedResidentHeightPx ?? snapshot.estimatedTotalHeightPx) : 0), scope: context.scrollStateKey, sessionId: context.sessionId };
    const previous = extentRef.current;
    if (next.scope === previous.scope && next.sessionId === previous.sessionId && next.prefix === previous.prefix && next.suffix === previous.suffix && next.residentHeight === previous.residentHeight && next.start === previous.start && next.count === previous.count && next.resident === previous.resident) return;
    restoreRef.current = null;
    const node = context.scrollContainerRef.current;
    const sharesResidentMessages = Math.max(previous.start, next.start) <
      Math.min(previous.start + previous.resident, next.start + next.resident);
    if (node && !context.tailFollowIntent && sharesResidentMessages && previous.scope === next.scope && previous.sessionId === next.sessionId && previous.prefix !== next.prefix) {
      restoreRef.current = {
        top: node.scrollTop,
        generation: handle.current?.getUserScrollGeneration(),
        delta: next.prefix - previous.prefix,
        scope: context.scrollStateKey,
        sessionId: context.sessionId,
        start: context.messageStartIndex,
        resident: context.renderedMessageCount,
      };
    }
    setExtent(next);
  };
  const measureRef = useCommittedRef(measure);

  useLayoutEffect(() => {
    const restore = restoreRef.current;
    restoreRef.current = null;
    const node = options.scrollContainerRef.current;
    if (restore && node && !options.tailFollowIntent &&
      restore.scope === options.scrollStateKey && restore.sessionId === options.sessionId &&
      restore.start === options.messageStartIndex && restore.resident === options.renderedMessageCount &&
      restore.generation === handle.current?.getUserScrollGeneration()) {
      const target = Math.min(Math.max(0, node.scrollHeight - node.clientHeight), Math.max(0, restore.top + restore.delta));
      markMessageStackVirtualizerPositionCorrection(node, target);
      writeMessageStackScrollTopImmediately(node, target);
    }
    navigation.committed();
  }, [extent, handle, navigation, options.scrollContainerRef]);

  useLayoutEffect(() => {
    measureRef.current();
    navigation.committed();
  }, [options.sessionId, options.scrollStateKey, options.messageCount, options.messageStartIndex, options.renderedMessageCount, handle, navigation, measureRef]);

  useLayoutEffect(() => {
    navigation.cancel();
    const node = options.scrollContainerRef.current;
    if (!options.isActive || !node) return;
    let frame: number | null = null;
    let timer: ReturnType<typeof window.setTimeout> | null = null;
    let pointerDown = false;
    let manualUntil = Number.NEGATIVE_INFINITY;
    let scrollTicket = 0;
    let disposed = false;
    const cancelTimer = () => { if (timer !== null) window.clearTimeout(timer); timer = null; };
    const cancel = () => { cancelTimer(); navigation.cancel(); };
    const manual = () => { manualUntil = performance.now() + 250; cancel(); };
    const intent = (event: Event) => {
      const detail = (event as CustomEvent<MessageStackUserScrollIntentDetail>).detail;
      if (!detail?.pendingNativeMovement && detail?.viewportCanMove) manual();
    };
    const down = (event: MouseEvent) => { if (event.target === node && event.button === 0) { pointerDown = true; manual(); } };
    const up = () => { if (pointerDown) { pointerDown = false; manualUntil = performance.now() + 250; } };
    const move = () => { if (pointerDown) manual(); };
    const scheduleMeasure = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => { frame = null; if (!disposed) measureRef.current(); });
    };
    const scroll = () => {
      scheduleMeasure();
      const ticket = ++scrollTicket;
      const top = node.scrollTop;
      // The virtualizer may publish its native-input generation before or
      // after this listener. Read that generation after all synchronous owners.
      queueMicrotask(() => {
        if (disposed || ticket !== scrollTicket || node.scrollTop !== top ||
          (!pointerDown && performance.now() > manualUntil) || navigation.isPending()) return;
        const content = contentRef.current;
        if (!content) return;
        const contentRect = content.getBoundingClientRect();
        // A hidden/unmeasured pane has no trustworthy position-to-history map.
        if (contentRect.height <= 0 || node.clientHeight <= 0) return;
        const origin = contentRect.top - node.getBoundingClientRect().top + node.scrollTop - node.clientTop;
        const target = unloadedHistoryPosition(node.scrollTop + node.clientHeight / 2 - origin, extentRef.current);
        cancelTimer();
        if (target === null) return;
        const generation = handle.current?.getUserScrollGeneration();
        // Native drag ticks coalesce to the latest position before any request.
        timer = window.setTimeout(() => {
          timer = null;
          if (!disposed && generation === handle.current?.getUserScrollGeneration() && node.scrollTop === top) void navigation.navigate(target);
        }, 80);
      });
    };
    const written = (event: Event) => {
      const detail = (event as CustomEvent<MessageStackScrollWriteDetail>).detail;
      if (detail?.scrollSource === "user" && (detail.scrollKind === "incremental" || detail.scrollKind === "page_jump")) return;
      // Explicit seeks, restoration and follow writes are not native drag
      // continuation. They must not reuse an earlier wheel/key lease.
      manualUntil = Number.NEGATIVE_INFINITY;
      pointerDown = false;
      scrollTicket += 1;
      cancelTimer();
    };
    node.addEventListener("scroll", scroll, { passive: true });
    node.addEventListener("mousedown", down);
    node.addEventListener("wheel", manual, { passive: true });
    node.addEventListener("touchstart", manual, { passive: true });
    node.addEventListener("touchmove", manual, { passive: true });
    node.addEventListener(MESSAGE_STACK_USER_SCROLL_INTENT_EVENT, intent);
    node.addEventListener(MESSAGE_STACK_NAVIGATION_START_EVENT, cancel);
    node.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, written);
    window.addEventListener("mouseup", up);
    window.addEventListener("mousemove", move);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(scheduleMeasure);
    if (contentRef.current) observer?.observe(contentRef.current);
    const unsubscribe = handle.current?.subscribeToCommittedLayout?.(scheduleMeasure);
    scheduleMeasure();
    return () => {
      disposed = true;
      cancel();
      if (frame !== null) window.cancelAnimationFrame(frame);
      observer?.disconnect();
      unsubscribe?.();
      node.removeEventListener("scroll", scroll);
      node.removeEventListener("mousedown", down);
      node.removeEventListener("wheel", manual);
      node.removeEventListener("touchstart", manual);
      node.removeEventListener("touchmove", manual);
      node.removeEventListener(MESSAGE_STACK_USER_SCROLL_INTENT_EVENT, intent);
      node.removeEventListener(MESSAGE_STACK_NAVIGATION_START_EVENT, cancel);
      node.removeEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, written);
      window.removeEventListener("mouseup", up);
      window.removeEventListener("mousemove", move);
    };
  }, [options.isActive, options.sessionId, options.scrollStateKey, options.scrollContainerRef, handle, navigation, measureRef, extentRef]);

  return { contentRef, prefixHeight: extent.prefix, suffixHeight: extent.suffix, virtualizerHandleRef: handle, navigate: navigation.navigate };
}
