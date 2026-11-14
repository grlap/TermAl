// Owns native scrollbar extent for nonresident history and bounded position seeks.
// Does not replace native input, pane FOLLOW/STAY, or checkpoint selection.
// Replaces history-navigation wiring from conversation-overview-controller.ts.
import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import { flushSync } from "react-dom";
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
import { classifyScrollbarPress, freezeMessageStackView } from "./message-stack-scrollbar-press";
import type { VirtualizedConversationMessageListHandle } from "./virtualized-conversation-types";

// Longest pause between scroll frames of one continuing native motion.
const NATIVE_MOTION_FRAME_GAP_MS = 120;
// How long after the last scroll input or frame the reader counts as at rest.
const READER_REST_MS = 400;
// A hold of the loaded messages further than this from their own place is a
// scrollbar jump waiting for its history, not an edge waiting for a page.
const HELD_FAR_PX = 2_000;

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
  // `adopted`: the part of the prefix that is an adopted placement (see below),
  // not an estimate of history; it moves no content.
  const [extent, setExtent] = useState(() => ({ ...historyScrollExtent(options.messageCount, options.messageStartIndex, options.renderedMessageCount, 0), scope: options.scrollStateKey, sessionId: options.sessionId, adopted: 0 }));
  const extentRef = useCommittedRef(extent);
  const navigationRef = useRef<ReturnType<typeof createConversationPositionNavigation> | null>(null);
  // Set below, once the slide it moves exists.
  const placeTargetRef = useRef<((localIndex: number) => boolean) | null>(null);
  // Only a seek of the reader's own scroll position keeps that position; a
  // navigation to a checkpoint or message scrolls there, moving the thumb.
  const seekKeepsPosition = useRef(false);
  if (navigationRef.current === null) {
    navigationRef.current = createConversationPositionNavigation(() => ({
      ...current.current,
      placeTarget: (localIndex: number) => placeTargetRef.current?.(localIndex) ?? false,
    }));
  }
  const navigation = navigationRef.current;
  const restoreRef = useRef<PendingPrefixCorrection | null>(null);
  // Set by the input listeners: re-requests the history in view after a
  // correction write moved a reader who was still moving or waiting for one.
  const rearmAfterCorrectionRef = useRef<(() => void) | null>(null);
  // Until when the reader counts as scrolling, and the re-measure due then.
  const readerActiveUntilRef = useRef(Number.NEGATIVE_INFINITY);
  const deferredMeasureRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  // How far the rendered estimate above the loaded messages is moved from the
  // real one. While the viewport is outside the loaded messages (a scrollbar
  // drag or jump), they stay at its edge instead of leaving blank estimate in
  // view; the position seek then loads the history there and jumps to it. The
  // total extent is unchanged, so the thumb stays where the reader put it.
  const [slidePx, setSlidePx] = useState(0);
  const slideRef = useRef(0);
  // A seek that lands while the loaded messages are held at the viewport
  // adopts that placement as the window's estimate: the scroll position the
  // reader chose then maps to the history it shows. Without it, each window's
  // own per-message estimate maps the same position elsewhere, and the view
  // chases one seek after another. Cleared when another window replaces it.
  const adoptedPx = useRef(0);
  const adopting = useRef(false);
  // Set by a navigation that is not the reader's own scrolling (a checkpoint,
  // message or latest-message jump): it lands at the target's estimated
  // place, so the thumb shows where it is. Holds resume with reader input.
  const holdSuspended = useRef(false);
  const resolveSlide = (node: HTMLElement) => {
    // A navigation to a checkpoint or message scrolls to its real place; the
    // loaded messages are not held at the old position meanwhile.
    if (holdSuspended.current || (!seekKeepsPosition.current && navigationRef.current?.isPending())) return 0;
    const content = contentRef.current;
    const real = extentRef.current;
    const unloaded = real.prefix + real.suffix;
    if (!content || node.clientHeight <= 0 || real.residentHeight <= 0 || unloaded <= 0) return 0;
    const contentRect = content.getBoundingClientRect();
    // An unmeasured pane has no trustworthy geometry; keep the real estimate.
    if (contentRect.height <= 0) return 0;
    // The viewport top in content coordinates; the content starts with the
    // rendered estimate above the loaded messages.
    const origin = contentRect.top - node.getBoundingClientRect().top + node.scrollTop - node.clientTop;
    const top = node.scrollTop - origin;
    // Rendered estimates that keep the loaded messages covering the viewport
    // (or, for a window shorter than the viewport, inside it). While a seek of
    // the reader's position loads, the current placement moves only as far as
    // that needs, so a placed target stays put until adopted. Otherwise the
    // loaded messages return to their own place whenever that covers the
    // viewport: a hold that followed the viewport would let keyboard and wheel
    // input carry the thumb on while the view stands still.
    const coverLow = top + node.clientHeight - real.residentHeight;
    const low = Math.min(coverLow, top);
    const high = Math.max(coverLow, top);
    const sticky = seekKeepsPosition.current && navigationRef.current?.isPending();
    const rendered = Math.min(Math.max(real.prefix + (sticky ? slideRef.current : 0), low, 0), high, unloaded);
    return Math.round(rendered - real.prefix);
  };
  const applySlide = (node: HTMLElement, sync: boolean) => {
    const next = resolveSlide(node);
    if (Math.abs(next - slideRef.current) < 1) return;
    slideRef.current = next;
    if (sync) flushSync(() => setSlidePx(next));
    else setSlidePx(next);
  };
  const applySlideRef = useCommittedRef(applySlide);
  // Before a navigation that shows its target at its estimated place: no
  // hold until the reader moves again, and with `adoption`, the window's own
  // estimate back (this hook's navigate; a foreign navigation start keeps an
  // adopted placement, which a new window clears anyway).
  const dropPlacement = (adoption: boolean) => {
    holdSuspended.current = true;
    const dropAdoption = adoption && adoptedPx.current !== 0;
    if (slideRef.current !== 0) {
      slideRef.current = 0;
      setSlidePx(0);
    }
    if (dropAdoption) {
      adoptedPx.current = 0;
      adopting.current = true;
      measureRef.current();
    }
  };
  const dropPlacementRef = useCommittedRef(dropPlacement);
  // A seek's loaded target, centred in the viewport by the rendered estimate
  // above it rather than by a scroll write: the browser may still be animating
  // a track press there, and a write would fight it.
  placeTargetRef.current = (localIndex: number) => {
    if (!seekKeepsPosition.current) return false;
    const node = current.current.scrollContainerRef.current;
    const content = contentRef.current;
    const messages = handle.current?.getLayoutSnapshot().messages;
    const target = messages?.[localIndex];
    const first = messages?.[0];
    if (!node || !content || !target || !first || node.clientHeight <= 0) return false;
    const contentRect = content.getBoundingClientRect();
    if (contentRect.height <= 0) return false;
    const origin = contentRect.top - node.getBoundingClientRect().top + node.scrollTop - node.clientTop;
    const top = node.scrollTop - origin;
    const real = extentRef.current;
    // A window that ends the conversation belongs at the end of the scroll
    // range, where its last message sits on the bottom edge: a plain scroll
    // to it moves the thumb there rather than leaving a gap below.
    if (real.suffix <= 0) return false;
    const within = target.estimatedTopPx - first.estimatedTopPx;
    const centred = top + node.clientHeight / 2 - within - Math.min(target.estimatedHeightPx, node.clientHeight) / 2;
    // The loaded messages still fill the viewport: near either end of the
    // window the target moves off centre instead of leaving blank space.
    const coverLow = top + node.clientHeight - real.residentHeight;
    const rendered = Math.min(Math.max(
      Math.min(Math.max(centred, Math.min(coverLow, top)), Math.max(coverLow, top)), 0,
    ), real.prefix + real.suffix);
    const next = Math.round(rendered - real.prefix);
    if (Math.abs(next - slideRef.current) >= 1) {
      slideRef.current = next;
      setSlidePx(next);
    }
    return true;
  };

  const measure = () => {
    const context = current.current;
    const snapshot = handle.current?.getLayoutSnapshot();
    const previous = extentRef.current;
    // Pages are measured while the reader scrolls. Re-estimating the missing
    // history from each new resident average would move the prefix, and its
    // correction write would abort the reader's smooth or momentum scroll.
    // The estimate changes only when the resident window itself moves.
    const sameWindow = previous.scope === context.scrollStateKey && previous.sessionId === context.sessionId &&
      previous.start === context.messageStartIndex && previous.residentHeight > 0;
    let next = { ...historyScrollExtent(context.messageCount, context.messageStartIndex, context.renderedMessageCount,
      snapshot?.sessionId === context.sessionId ? (snapshot.estimatedResidentHeightPx ?? snapshot.estimatedTotalHeightPx) : 0,
      sameWindow ? previous.unit : undefined), scope: context.scrollStateKey, sessionId: context.sessionId, adopted: 0 };
    // While the reader scrolls, an older page loading above changes nothing
    // but the content above: the list's own prepend anchoring keeps the view
    // still. Moving the estimate as well would need a second correction, and
    // one that lands mid-scroll is lost or cuts the scroll short; either way
    // the transcript jumps. The estimate is reconciled once the reader rests.
    // A window that replaces the resident messages (a position seek) is placed
    // by its own restore; holding its old prefix would park the view far from
    // its position until the reader rests.
    const sharesResidentMessages = Math.max(previous.start, next.start) <
      Math.min(previous.start + previous.resident, next.start + next.resident);
    if (!sharesResidentMessages || previous.scope !== next.scope || previous.sessionId !== next.sessionId) adoptedPx.current = 0;
    next = { ...next, prefix: next.prefix + adoptedPx.current, suffix: next.suffix - adoptedPx.current, adopted: adoptedPx.current };
    const readerActive = performance.now() < readerActiveUntilRef.current;
    if (readerActive && !adopting.current && sharesResidentMessages && previous.scope === next.scope && previous.sessionId === next.sessionId &&
      previous.residentHeight > 0 && next.prefix !== previous.prefix) {
      // Hold the estimate, not an adoption made meanwhile.
      next = { ...next, prefix: previous.prefix + next.adopted - previous.adopted };
      if (deferredMeasureRef.current === null) {
        deferredMeasureRef.current = window.setTimeout(() => {
          deferredMeasureRef.current = null;
          measureRef.current();
        }, Math.max(16, readerActiveUntilRef.current - performance.now() + 16));
      }
    }
    if (next.scope === previous.scope && next.sessionId === previous.sessionId && next.prefix === previous.prefix && next.suffix === previous.suffix && next.residentHeight === previous.residentHeight && next.start === previous.start && next.count === previous.count && next.resident === previous.resident) return;
    restoreRef.current = null;
    // An adopted placement changes the estimate, not the layout: no correction.
    const adoptedNow = adopting.current;
    adopting.current = false;
    if (!sharesResidentMessages && slideRef.current !== 0) {
      // A new window has its own estimate; the layout effect places it.
      slideRef.current = 0;
      setSlidePx(0);
    }
    const node = context.scrollContainerRef.current;
    if (node && !adoptedNow && !context.tailFollowIntent && sharesResidentMessages && previous.scope === next.scope && previous.sessionId === next.sessionId && previous.prefix !== next.prefix) {
      restoreRef.current = {
        top: node.scrollTop,
        generation: handle.current?.getUserScrollGeneration(),
        // An adoption moves the estimate, not the content: no correction.
        delta: (next.prefix - next.adopted) - (previous.prefix - previous.adopted),
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
      // The write moves scrollTop under a pending request, which then lapses.
      rearmAfterCorrectionRef.current?.();
    }
    navigation.committed();
    if (node) applySlideRef.current(node, false);
  }, [extent, handle, navigation, options.scrollContainerRef, applySlideRef]);

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
    // Smooth and momentum scrolling keep moving, frame by frame, long after
    // the wheel or touch event that started them. Native frames that follow
    // that motion without a pause remain it until a programmatic write.
    let motionUntil = Number.NEGATIVE_INFINITY;
    let lastFrameGeneration = handle.current?.getUserScrollGeneration();
    let scrollTicket = 0;
    let correctionTop: number | null = null;
    let disposed = false;
    // While the reader drags the scrollbar, or a track press waits for its
    // history, a static copy of the view covers the stack: the browser moves
    // the dragged content before page code can keep loaded messages in view.
    // The copy shows the view as of the last pause, and goes once the reader
    // has let go and the history in view is loaded.
    let frozen: ReturnType<typeof freezeMessageStackView> | null = null;
    // The button is held on the scrollbar. Unlike `pointerDown`, a seek's own
    // jump does not end it: only the release does.
    let scrollbarHeld = false;
    const releaseFrozen = () => { frozen?.release(); frozen = null; };
    const settleFrozen = () => {
      if (!frozen) return;
      // Copy or reveal the view only once it is drawn: a seek's placement and
      // its adoption commit after the seek settles.
      window.requestAnimationFrame(() => window.requestAnimationFrame(() => {
        if (disposed || !frozen) return;
        if (scrollbarHeld) frozen.take();
        else if (!navigation.isPending()) releaseFrozen();
      }));
    };
    const cancelTimer = () => { if (timer !== null) window.clearTimeout(timer); timer = null; };
    const cancel = () => { cancelTimer(); navigation.cancel(); };
    // The unloaded history position at the viewport centre, null while the
    // centre is resident, undefined while the pane cannot be measured.
    const positionInView = () => {
      const content = contentRef.current;
      if (!content) return undefined;
      const contentRect = content.getBoundingClientRect();
      // A hidden/unmeasured pane has no trustworthy position-to-history map.
      if (contentRect.height <= 0 || node.clientHeight <= 0) return undefined;
      const origin = contentRect.top - node.getBoundingClientRect().top + node.scrollTop - node.clientTop;
      return unloadedHistoryPosition(node.scrollTop + node.clientHeight / 2 - origin, extentRef.current);
    };
    // Requests the history in view once the position holds for 80 ms. Called
    // only for user input, after all synchronous owners of that input ran.
    const arm = () => {
      if (navigation.isPending()) return;
      const top = node.scrollTop;
      const target = positionInView();
      if (target === undefined) return;
      cancelTimer();
      if (target === null) {
        // Loaded messages are in view: once the position holds, show them.
        const ticket = scrollTicket;
        timer = window.setTimeout(() => {
          timer = null;
          if (!disposed && ticket === scrollTicket && node.scrollTop === top) settleFrozen();
        }, 80);
        return;
      }
      // Native drag ticks coalesce to the latest position before any request.
      // Settled means no newer scroll frame or programmatic write since: during
      // a thumb drag the virtualizer advances its input generation after this
      // runs, and the browser may never deliver the drag's mouseup, so the
      // generation cannot tell a settled drag from a moving one. A competing
      // navigation cancels this timer through its navigation-start event.
      const ticket = scrollTicket;
      timer = window.setTimeout(() => {
        timer = null;
        if (disposed || ticket !== scrollTicket || node.scrollTop !== top) return;
        // The loaded messages stay at the viewport edge (the slide above)
        // until the seek jumps to the loaded target.
        seekKeepsPosition.current = true;
        void navigation.navigate(target).then(applied => {
          if (disposed) return;
          if (applied && slideRef.current !== 0) {
            adoptedPx.current += slideRef.current;
            slideRef.current = 0;
            adopting.current = true;
            setSlidePx(0);
            measureRef.current();
          }
          if (applied) settleFrozen();
          else if (!scrollbarHeld) releaseFrozen();
        });
      }, 80);
    };
    // Input that does not move the viewport (a pointer move that leaves the
    // thumb where it is, a release) still ends with the view where it is;
    // the history in view is requested even if no later scroll event comes.
    // A scroll event or a programmatic write after the input owns the outcome.
    const armAfterInput = () => {
      const ticket = scrollTicket;
      queueMicrotask(() => { if (!disposed && ticket === scrollTicket) arm(); });
    };
    const active = () => { readerActiveUntilRef.current = performance.now() + READER_REST_MS; };
    const manual = () => { holdSuspended.current = false; active(); manualUntil = performance.now() + 250; cancel(); armAfterInput(); };
    const intent = (event: Event) => {
      const detail = (event as CustomEvent<MessageStackUserScrollIntentDetail>).detail;
      if (!detail?.pendingNativeMovement && detail?.viewportCanMove) manual();
    };
    const down = (event: MouseEvent) => {
      if (event.target !== node || event.button !== 0) return;
      pointerDown = true;
      const press = classifyScrollbarPress(node, event.clientX, event.clientY);
      if (press && press.kind !== "arrow") {
        scrollbarHeld = true;
        frozen ??= freezeMessageStackView(node);
        frozen.take();
      }
      manual();
      if (press?.kind === "track") {
        // A track press goes to the pressed position at once, like a drag
        // there; the frozen view stays until that history is loaded. The
        // browser's page-by-page track scrolling would need thousands of
        // pages to cross the estimated history.
        writeMessageStackScrollTopImmediately(node, press.scrollTop);
      }
    };
    const up = () => {
      const wasHeld = scrollbarHeld;
      scrollbarHeld = false;
      if (wasHeld && frozen && !navigation.isPending() && positionInView() === null) {
        // Released over loaded messages: show them once this frame lays out.
        window.requestAnimationFrame(() => { if (!disposed && !scrollbarHeld) settleFrozen(); });
      }
      if (!pointerDown) return;
      active(); pointerDown = false; manualUntil = performance.now() + 250; armAfterInput();
    };
    // Chrome often keeps a scrollbar drag's mouseup from the page; a move
    // with no button held shows the release.
    const move = (event: MouseEvent) => {
      if ((pointerDown || scrollbarHeld) && event.buttons === 0) { up(); return; }
      if (pointerDown) manual();
    };
    // The virtualizer announces a navigation start for every native frame it
    // counts as reader movement, after this hook armed that frame. While the
    // reader is moving, that is their own motion: drop any in-flight seek but
    // keep requesting the position in view. Otherwise a navigation (a jump to a
    // checkpoint, search result or the latest message) takes over.
    const navigationStart = () => {
      const readerMoving = pointerDown || performance.now() <= Math.max(manualUntil, motionUntil);
      cancel();
      if (readerMoving) armAfterInput();
      else { releaseFrozen(); scrollbarHeld = false; dropPlacementRef.current(false); }
    };
    const scheduleMeasure = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => { frame = null; if (!disposed) measureRef.current(); });
    };
    const scroll = () => {
      // Before this frame paints: keep loaded messages in view.
      applySlideRef.current(node, true);
      scheduleMeasure();
      if (correctionTop !== null && node.scrollTop === correctionTop) { correctionTop = null; return; }
      correctionTop = null;
      const ticket = ++scrollTicket;
      const top = node.scrollTop;
      // The virtualizer may publish its native-input generation before or
      // after this listener. Read that generation after all synchronous owners.
      queueMicrotask(() => {
        if (disposed || ticket !== scrollTicket || node.scrollTop !== top) return;
        const now = performance.now();
        // Only frames the virtualizer counts as native reader movement extend
        // a motion; its own registered corrections do not. The prefix restore
        // above keeps the same content in view under a new estimate, so a
        // seek it re-arms requests the history the reader is looking at.
        const generation = handle.current?.getUserScrollGeneration();
        const nativeFrame = generation !== undefined && generation !== lastFrameGeneration;
        lastFrameGeneration = generation;
        if (!pointerDown && now > manualUntil && !(nativeFrame && now <= motionUntil)) return;
        motionUntil = now + NATIVE_MOTION_FRAME_GAP_MS;
        active();
        arm();
      });
    };
    const written = (event: Event) => {
      const detail = (event as CustomEvent<MessageStackScrollWriteDetail>).detail;
      if (detail?.scrollSource === "user" && (detail.scrollKind === "incremental" || detail.scrollKind === "page_jump")) return;
      // Explicit seeks, restoration and follow writes are not native drag
      // continuation. They must not reuse an earlier wheel/key lease.
      manualUntil = Number.NEGATIVE_INFINITY;
      motionUntil = Number.NEGATIVE_INFINITY;
      pointerDown = false;
      scrollTicket += 1;
      cancelTimer();
      // A seek's own jump keeps the frozen view; any other navigation (a
      // jump to the latest message, a checkpoint) replaces it.
      if (!navigation.isPending()) { scrollbarHeld = false; releaseFrozen(); }
    };
    rearmAfterCorrectionRef.current = () => {
      if (!disposed && (timer !== null || pointerDown || performance.now() <= Math.max(manualUntil, motionUntil))) {
        // The correction's own scroll event follows; it is not new motion.
        correctionTop = node.scrollTop;
        arm();
      }
    };
    node.addEventListener("scroll", scroll, { passive: true });
    node.addEventListener("mousedown", down);
    node.addEventListener("wheel", manual, { passive: true });
    node.addEventListener("touchstart", manual, { passive: true });
    node.addEventListener("touchmove", manual, { passive: true });
    node.addEventListener(MESSAGE_STACK_USER_SCROLL_INTENT_EVENT, intent);
    node.addEventListener(MESSAGE_STACK_NAVIGATION_START_EVENT, navigationStart);
    node.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, written);
    window.addEventListener("mouseup", up);
    window.addEventListener("mousemove", move);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(scheduleMeasure);
    if (contentRef.current) observer?.observe(contentRef.current);
    const unsubscribe = handle.current?.subscribeToCommittedLayout?.(scheduleMeasure);
    scheduleMeasure();
    return () => {
      disposed = true;
      releaseFrozen();
      rearmAfterCorrectionRef.current = null;
      if (deferredMeasureRef.current !== null) { window.clearTimeout(deferredMeasureRef.current); deferredMeasureRef.current = null; }
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
      node.removeEventListener(MESSAGE_STACK_NAVIGATION_START_EVENT, navigationStart);
      node.removeEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, written);
      window.removeEventListener("mouseup", up);
      window.removeEventListener("mousemove", move);
    };
  }, [options.isActive, options.sessionId, options.scrollStateKey, options.scrollContainerRef, handle, navigation, measureRef, extentRef, applySlideRef]);

  return {
    contentRef,
    prefixHeight: extent.prefix + slidePx,
    suffixHeight: extent.suffix - slidePx,
    // True while the loaded messages are held at the viewport edge: page
    // demand at their edges would load around the old window, not the seek's.
    // Only a hold far from the loaded window's own place: one that a seek is
    // loading the history for. A small offset at the window's edge still
    // loads the next page there.
    heldAtViewport: Math.abs(slidePx) > HELD_FAR_PX,
    virtualizerHandleRef: handle,
    navigate: (position: number) => {
      seekKeepsPosition.current = false;
      dropPlacementRef.current(true);
      return navigation.navigate(position);
    },
  };
}
