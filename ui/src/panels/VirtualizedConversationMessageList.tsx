// Virtualized-conversation list rendering for an agent session pane.
//
// What this file owns:
//   - `VirtualizedConversationMessageList` — the React component
//     that renders a long session transcript via absolute-positioned
//     message slots whose heights are measured via
//     `ResizeObserver`. Owns the full render-window orchestration:
//     which messages are in the window, what their estimated vs.
//     measured heights are, how the viewport tracks the bottom of
//     the conversation while streaming, the anchor-stabilized
//     scroll-up auto-load of older messages, and the user-scroll
//     cooldown that keeps measurement-driven `scrollTop` adjustments
//     from fighting direct-input scroll gestures.
//   - `MeasuredMessageCard` — the per-slot wrapper that reports its
//     measured height back to the virtualizer via `onHeightChange`
//     after every `ResizeObserver` tick.
//   - Handler type aliases for the render-message callback signature
//     (`RenderMessageCard`), the unbound session-wide handlers
//     (`UserInputSubmitHandler`, `McpElicitationSubmitHandler`,
//     `CodexAppRequestSubmitHandler`) that the parent pane threads
//     through, and the bound-to-a-session variants
//     (`BoundUserInputSubmitHandler`, `BoundMcpElicitationSubmitHandler`,
//     `BoundCodexAppRequestSubmitHandler`) that `MeasuredMessageCard`
//     calls into.
//   - Virtualization tuning constants specific to the render-window
//     (`VIRTUALIZED_MESSAGE_OVERSCAN_PX`, `RENDER_WINDOW_INITIAL_SIZE`,
//     `RENDER_WINDOW_LOAD_MORE`,
//     `RENDER_WINDOW_LOAD_MORE_THRESHOLD_PX`,
//     `USER_SCROLL_ADJUSTMENT_COOLDOWN_MS`) and the shared empty-set
//     sentinel `EMPTY_MATCHED_ITEM_KEYS` for session-find
//     highlighting.
//
// What this file does NOT own:
//   - The pure virtualization math
//     (`buildVirtualizedMessageLayout`,
//     `findVirtualizedMessageRange`,
//     `getAdjustedVirtualizedScrollTopForHeightChange`,
//     `estimateConversationMessageHeight`,
//     `DEFAULT_ESTIMATED_MESSAGE_HEIGHT`,
//     `DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT`,
//     `isScrollContainerNearBottom`, `getScrollContainerBottomGap`)
//     — those stay in `./conversation-virtualization`.
//   - The outer pane shell, footer, composer, non-virtualized
//     conversation path for short sessions, and the
//     inactive-but-cached session wrapper — those remain in
//     `./AgentSessionPanel.tsx` and import the component from here.
//   - `MessageSlot` — rendered inside each `MeasuredMessageCard` but
//     defined in `./session-message-leaves`.
//
// Split out of `ui/src/panels/AgentSessionPanel.tsx`. Same exports,
// same signatures, same constants; consumers in `AgentSessionPanel`
// now import both the component and the handler types from here.

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { MessageSlot } from "./session-message-leaves";
import {
  DEFAULT_ESTIMATED_MESSAGE_HEIGHT,
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  buildVirtualizedMessageLayout,
  estimateConversationMessageHeight,
  findVirtualizedMessageRange,
  getAdjustedVirtualizedScrollTopForHeightChange,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import type {
  ApprovalDecision,
  JsonValue,
  Message,
  McpElicitationAction,
} from "../types";

export type UserInputSubmitHandler = (
  sessionId: string,
  messageId: string,
  answers: Record<string, string[]>,
) => void;

export type BoundUserInputSubmitHandler = (
  messageId: string,
  answers: Record<string, string[]>,
) => void;

export type McpElicitationSubmitHandler = (
  sessionId: string,
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

export type BoundMcpElicitationSubmitHandler = (
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

export type CodexAppRequestSubmitHandler = (
  sessionId: string,
  messageId: string,
  result: JsonValue,
) => void;

export type BoundCodexAppRequestSubmitHandler = (messageId: string, result: JsonValue) => void;

export type RenderMessageCard = (
  message: Message,
  preferImmediateHeavyRender: boolean,
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  onUserInputSubmit: BoundUserInputSubmitHandler,
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler,
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler,
) => JSX.Element | null;

const VIRTUALIZED_MESSAGE_OVERSCAN_PX = 960;
/** Maximum number of messages to render initially. Older messages load on scroll-up or via the inline button. */
const RENDER_WINDOW_INITIAL_SIZE = 200;
/** Number of additional messages to prepend when the window expands (auto or via button). */
const RENDER_WINDOW_LOAD_MORE = 200;
/** Scroll-top threshold (px) at which older messages auto-load. */
const RENDER_WINDOW_LOAD_MORE_THRESHOLD_PX = 400;
/**
 * Cool-down window (ms) after a direct-scroll input (wheel, touch,
 * keyboard) during which measurement-driven `scrollTop` adjustments
 * in `handleHeightChange` are skipped. 200 ms comfortably covers a
 * single wheel notch plus browser scroll coalescing; longer than
 * that and idle anchor preservation starts losing responsiveness.
 */
const USER_SCROLL_ADJUSTMENT_COOLDOWN_MS = 200;
export const EMPTY_MATCHED_ITEM_KEYS = new Set<string>();

export function VirtualizedConversationMessageList({
  isActive,
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  conversationSearchQuery = "",
  conversationSearchMatchedItemKeys = EMPTY_MATCHED_ITEM_KEYS,
  conversationSearchActiveItemKey = null,
  onConversationSearchItemMount = () => {},
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
}: {
  isActive: boolean;
  renderMessageCard: RenderMessageCard;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  conversationSearchQuery?: string;
  conversationSearchMatchedItemKeys?: ReadonlySet<string>;
  conversationSearchActiveItemKey?: string | null;
  onConversationSearchItemMount?: (itemKey: string, node: HTMLElement | null) => void;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
}) {
  const hasConversationSearch = conversationSearchQuery.trim().length > 0;
  const activeConversationSearchMessageId =
    conversationSearchActiveItemKey?.startsWith("message:")
      ? conversationSearchActiveItemKey.slice("message:".length)
      : null;
  const messageHeightsRef = useRef<Record<string, number>>({});
  // Expected behavior: when the viewport is already at the latest message,
  // virtualized height measurements should keep the viewport pinned there.
  const shouldKeepBottomAfterLayoutRef = useRef(false);
  // Pending anchor-stabilization write after `setRenderWindowSize`
  // prepends older messages. Captured in the scroll-near-top auto-load
  // handler (and the explicit "Load N earlier messages" button), then
  // applied in a `useLayoutEffect` after React commits the grown
  // layout but before the browser paints — so the viewport never
  // shows the new document at the old `scrollTop`. Tracking a
  // specific message's viewport offset (rather than a raw
  // `scrollHeight` delta) keeps the anchor valid regardless of whether
  // the newly-prepended cards are priced at their estimated or
  // measured heights; subsequent `ResizeObserver`-driven measurement
  // adjustments via `getAdjustedVirtualizedScrollTopForHeightChange`
  // then preserve the same offset as the prepended block settles.
  const pendingLoadMoreAnchorRef = useRef<{
    anchorMessageId: string;
    anchorOffsetInViewport: number;
    // Render-window size this anchor was captured for. The consuming
    // `useLayoutEffect` only applies the anchor when the current
    // `renderWindowSize` matches this value, so an auto-grow on a
    // streaming-delta render (lines 898-905) that bumps
    // `renderWindowSize` by a single message cannot silently pick up
    // a stale anchor captured during an earlier scroll gesture.
    expectedRenderWindowSize: number;
  } | null>(null);
  // Timestamp of the user's last direct-scroll input (wheel, touch-drag,
  // or keyboard PageUp/PageDown/arrow/Home/End). Measurement-driven
  // `scrollTop` adjustments via
  // `getAdjustedVirtualizedScrollTopForHeightChange` fight the user
  // during an active scroll: each unmeasured card that enters the
  // viewport reports a height much larger than its estimate, the
  // anchor math rewrites `scrollTop` to preserve the anchor on
  // content that shifted down, and the net effect is the user's
  // wheel-up motion gets cancelled (or even reversed) by a
  // simultaneous growth adjustment. We track the last direct-scroll
  // input here so `handleHeightChange` can skip the scrollTop write
  // while the user is actively scrolling — layout still updates on
  // measurement, the wheel input still wins the visible scroll.
  const lastUserScrollInputTimeRef = useRef(0);
  const [viewport, setViewport] = useState({
    height: DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
    scrollTop: 0,
  });
  const [layoutVersion, setLayoutVersion] = useState(0);
  // Post-activation measurement phase: when the list transitions from
  // inactive to active (or mounts directly in the active state with
  // messages already present), the first render uses estimated heights
  // for any messages that were added while the list was inactive. Those
  // estimates drive `layout.tops` and `layout.totalHeight`.
  //
  // This flag used to ALSO hide the wrapper via a CSS
  // `visibility: hidden` class so the mis-estimated first paint
  // never reached the screen. That produced a "shows messages then
  // empty" flicker in practice: any path that momentarily toggled
  // `isActive` or re-triggered the measuring phase during streaming
  // would snap the whole transcript invisible for one commit before
  // the completion check (or the 150 ms timeout) revealed it again,
  // and the invisible-commit also stole the viewport from anyone
  // mid-scroll. The hide is gone now; the flag only gates the
  // scroll-to-bottom handoff after measurements land (and the
  // `shouldKeepBottomAfterLayoutRef` arm below). Cards may paint at
  // estimated positions for one frame on tab switch; the original
  // overlap-on-scroll trade-off is intentionally smaller because
  // only that first post-activation frame can be off, not steady
  // state. A future cleanup could inline the scroll-write logic
  // directly into the inactive→active transition detector and drop
  // the state variable entirely.
  const [isMeasuringPostActivation, setIsMeasuringPostActivation] = useState(
    () => isActive && messages.length > 0,
  );
  // Detect inactive → active transitions during render so the
  // measurement handoff runs in the SAME render phase as
  // `isActive=true`. Setting state during render is React's
  // officially-blessed pattern for derived-from-props state; React
  // replays the render synchronously so intermediate state never
  // commits.
  const [prevIsActive, setPrevIsActive] = useState(isActive);
  if (prevIsActive !== isActive) {
    setPrevIsActive(isActive);
    if (!prevIsActive && isActive && messages.length > 0) {
      setIsMeasuringPostActivation(true);
    }
  }

  // Render window: normally only keep the last N messages in the layout.
  // Session find temporarily keeps all messages in the layout so the active
  // hit can be positioned, while still rendering only the visible rows.
  const [renderWindowSize, setRenderWindowSize] = useState(
    Math.min(RENDER_WINDOW_INITIAL_SIZE, messages.length),
  );
  const prevMessageCountRef = useRef(messages.length);
  // When new messages arrive (agent responding), keep the window anchored
  // to the bottom by expanding it to cover the new messages.
  if (messages.length > prevMessageCountRef.current) {
    const growth = messages.length - prevMessageCountRef.current;
    if (renderWindowSize + growth <= messages.length) {
      // Expand window by the number of new messages so they're visible.
      // This runs during render (before effects) so the layout is correct
      // on the first paint.
      setRenderWindowSize((prev) => Math.min(prev + growth, messages.length));
    }
  }
  prevMessageCountRef.current = messages.length;

  const windowStartIndex = hasConversationSearch
    ? 0
    : Math.max(0, messages.length - renderWindowSize);
  const windowedMessages = useMemo(
    () => messages.slice(windowStartIndex),
    [messages, windowStartIndex],
  );
  const hasOlderMessages = windowStartIndex > 0;

  const messageIndexById = useMemo(
    () => new Map(windowedMessages.map((message, index) => [message.id, index])),
    [windowedMessages],
  );
  const messageHeights = useMemo(
    () =>
      windowedMessages.map(
        (message) => messageHeightsRef.current[message.id] ?? estimateConversationMessageHeight(message),
      ),
    [layoutVersion, windowedMessages],
  );
  const layout = useMemo(() => buildVirtualizedMessageLayout(messageHeights), [messageHeights]);
  const layoutTopsRef = useRef(layout.tops);
  layoutTopsRef.current = layout.tops;
  const activeViewport = isActive ? scrollContainerRef.current : null;
  const viewportHeight =
    activeViewport?.clientHeight && activeViewport.clientHeight > 0
      ? activeViewport.clientHeight
      : viewport.height;
  const viewportScrollTop = activeViewport ? activeViewport.scrollTop : viewport.scrollTop;
  const activeConversationSearchMessageIndex =
    activeConversationSearchMessageId !== null
      ? messageIndexById.get(activeConversationSearchMessageId)
      : undefined;
  const activeConversationSearchScrollTop =
    hasConversationSearch && activeConversationSearchMessageIndex !== undefined
      ? Math.max(
          (layout.tops[activeConversationSearchMessageIndex] ?? 0) -
            Math.max(
              (viewportHeight -
                (messageHeights[activeConversationSearchMessageIndex] ??
                  DEFAULT_ESTIMATED_MESSAGE_HEIGHT)) /
                2,
              0,
            ),
          0,
        )
      : null;
  const viewportVisibleRange = useMemo(
    () =>
      findVirtualizedMessageRange(
        layout.tops,
        messageHeights,
        viewportScrollTop,
        viewportHeight,
        VIRTUALIZED_MESSAGE_OVERSCAN_PX,
      ),
    [layout.tops, messageHeights, viewportHeight, viewportScrollTop],
  );
  const activeConversationSearchVisibleRange = useMemo(
    () =>
      activeConversationSearchScrollTop === null
        ? null
        : findVirtualizedMessageRange(
            layout.tops,
            messageHeights,
            activeConversationSearchScrollTop,
            viewportHeight,
            VIRTUALIZED_MESSAGE_OVERSCAN_PX,
          ),
    [
      activeConversationSearchScrollTop,
      layout.tops,
      messageHeights,
      viewportHeight,
    ],
  );
  const visibleRanges = useMemo(() => {
    const ranges = [viewportVisibleRange];
    if (activeConversationSearchVisibleRange) {
      ranges.push(activeConversationSearchVisibleRange);
    }

    const sortedRanges = ranges
      .filter((range) => range.endIndex > range.startIndex)
      .map((range) => ({ ...range }))
      .sort((first, second) => first.startIndex - second.startIndex);
    const mergedRanges: { startIndex: number; endIndex: number }[] = [];

    for (const range of sortedRanges) {
      const lastRange = mergedRanges[mergedRanges.length - 1];
      if (!lastRange || range.startIndex > lastRange.endIndex) {
        mergedRanges.push(range);
        continue;
      }

      lastRange.endIndex = Math.max(lastRange.endIndex, range.endIndex);
    }

    return mergedRanges.length > 0 ? mergedRanges : [{ startIndex: 0, endIndex: 0 }];
  }, [activeConversationSearchVisibleRange, viewportVisibleRange]);

  useEffect(() => {
    messageHeightsRef.current = Object.fromEntries(
      windowedMessages
        .filter((message) => messageHeightsRef.current[message.id] !== undefined)
        .map((message) => [message.id, messageHeightsRef.current[message.id] as number]),
    );
  }, [windowedMessages]);

  useLayoutEffect(() => {
    if (
      !isActive ||
      !hasConversationSearch ||
      activeConversationSearchMessageIndex === undefined ||
      activeConversationSearchScrollTop === null
    ) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const nextViewportHeight =
      node.clientHeight > 0 ? node.clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT;
    const nextScrollTop = activeConversationSearchScrollTop;

    shouldKeepBottomAfterLayoutRef.current = false;
    if (Math.abs(node.scrollTop - nextScrollTop) >= 1) {
      node.scrollTop = nextScrollTop;
    }
    setViewport((current) =>
      current.height === nextViewportHeight && current.scrollTop === nextScrollTop
        ? current
        : {
            height: nextViewportHeight,
            scrollTop: nextScrollTop,
          },
    );
  }, [
    activeConversationSearchMessageIndex,
    activeConversationSearchScrollTop,
    hasConversationSearch,
    isActive,
    scrollContainerRef,
  ]);

  // Arm the bottom-pin flag while measuring so the existing re-pin
  // `useLayoutEffect` (declared below) writes the correct scrollTop
  // after each measurement commit. Declared BEFORE the re-pin effect so
  // that it runs first in the commit phase and the flag is set by the
  // time the re-pin reads it.
  useLayoutEffect(() => {
    if (isMeasuringPostActivation) {
      shouldKeepBottomAfterLayoutRef.current = true;
    }
  }, [isMeasuringPostActivation]);

  // Completion check: intentionally runs on every commit while measuring is
  // active. Measurements mutate `messageHeightsRef` synchronously and then
  // trigger ordinary React commits, so an explicit dependency list is more
  // likely to miss a ref-only measurement than to make this safer.
  // When all currently-visible slots have real measurements, write a
  // final scrollTop and reveal by clearing the flag. Reading from
  // `messageHeightsRef.current` is safe because `handleHeightChange`
  // mutates it synchronously before bumping `layoutVersion`, so by the
  // time this effect runs after a measurement-driven commit, the ref
  // already reflects the latest data.
  useLayoutEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }
    if (!isActive) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const visibleMessages = visibleRanges.flatMap((range) =>
      windowedMessages.slice(range.startIndex, range.endIndex),
    );
    if (visibleMessages.length === 0) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const allMeasured = visibleMessages.every(
      (message) => messageHeightsRef.current[message.id] !== undefined,
    );
    if (!allMeasured) {
      return;
    }

    const node = scrollContainerRef.current;
    if (node) {
      const target = Math.max(node.scrollHeight - node.clientHeight, 0);
      if (Math.abs(node.scrollTop - target) >= 1) {
        node.scrollTop = target;
      }
    }
    setIsMeasuringPostActivation(false);
  });

  // Timeout fallback: if the visible slots take longer than 150 ms to
  // report their first measurement (e.g., a heavy syntax-highlighted
  // code block or a deferred markdown renderer), reveal the wrapper
  // anyway so the user sees something rather than a blank region.
  useEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      const node = scrollContainerRef.current;
      if (node) {
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        if (Math.abs(node.scrollTop - target) >= 1) {
          node.scrollTop = target;
        }
      }
      shouldKeepBottomAfterLayoutRef.current = true;
      setIsMeasuringPostActivation(false);
    }, 150);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [isMeasuringPostActivation, scrollContainerRef]);

  useLayoutEffect(() => {
    if (!isActive || !shouldKeepBottomAfterLayoutRef.current) {
      return;
    }

    // Honour the user-scroll cooldown here too. `handleHeightChange`
    // already skips its own `scrollTop` write during the cooldown, but
    // it still calls `setLayoutVersion((c) => c + 1)`, which triggers
    // a re-render that commits a new `layout.totalHeight` — and THIS
    // effect fires on that change. Without the gate the re-pin
    // would run anyway and snap `scrollTop` back to the bottom, so
    // the user's wheel-up input is cancelled one commit later. The
    // net effect is the exact "near-bottom streaming re-pins the
    // viewport and fights user scroll-up" symptom the cooldown is
    // supposed to close. Gating the effect on the same cooldown
    // keeps the scroll-to-bottom handoff working for pure streaming
    // (no direct-scroll input → `lastUserScrollInputTimeRef` stays
    // at its init 0 → `performance.now() - 0` always exceeds the
    // cooldown) while suppressing the re-pin during an active wheel
    // gesture.
    const timeSinceUserScroll =
      performance.now() - lastUserScrollInputTimeRef.current;
    if (timeSinceUserScroll < USER_SCROLL_ADJUSTMENT_COOLDOWN_MS) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    // Re-pin to the new bottom on every commit while the flag is set.
    // The flag is cleared only in the scroll handler (see `syncViewport`
    // below) when the user explicitly scrolls away from near-bottom.
    //
    // The round 8 pre-fix version cleared this flag here after observing
    // `isScrollContainerAtBottom(node)`, but that ran in the SAME commit
    // the flag was set in (via `handleHeightChange` → `setLayoutVersion`).
    // At that point the DOM reflected the OLD `scrollHeight` — the
    // wrapper's `style={{ height: layout.totalHeight }}` had not yet
    // committed the new total height — so `isScrollContainerAtBottom`
    // reported true against the stale geometry, cleared the flag, and
    // the *next* commit (the one that actually committed the new
    // height) found the flag cleared and bailed out of the re-pin. The
    // net effect was leaving the user ~10-100 px above the new bottom
    // on the exact measurement-driven growth scenario the round 8 fix
    // was added to handle. Clearing is delegated to scroll events so
    // the pin survives across as many commits as the measurement cycle
    // needs, matching the `TerminalPanel.shouldStickToBottomRef`
    // sticky-until-user-scrolls-away pattern.
    //
    // The `>= 1` guard prevents no-op writes from triggering a scroll
    // event + reflow + ResizeObserver tick cascade. When the layout
    // commit only moved `scrollHeight` by the subpixel rounding delta,
    // the computed target matches the current scrollTop and we do
    // nothing instead of writing a value the browser would round to the
    // same integer anyway.
    const target = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (Math.abs(node.scrollTop - target) >= 1) {
      node.scrollTop = target;
    }
  }, [isActive, layout.totalHeight, scrollContainerRef]);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const syncViewport = () => {
      const nextState = {
        height: node.clientHeight > 0 ? node.clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
        scrollTop: node.scrollTop,
      };

      setViewport((current) =>
        current.height === nextState.height && current.scrollTop === nextState.scrollTop
          ? current
          : nextState,
      );

      // Clear the pin flag once the user has scrolled away from
      // near-bottom. Mirrors `TerminalPanel.shouldStickToBottomRef`'s
      // `onScroll` update: sticky stays on until the user explicitly
      // moves away, then re-arms only when a later `handleHeightChange`
      // observes the viewport back near the bottom. This is the only
      // site that clears the flag — the re-pinning `useLayoutEffect`
      // above intentionally leaves it set so a single measurement
      // growth can survive the same-commit `setLayoutVersion` →
      // follow-up-commit re-render without tearing down the pin.
      if (
        shouldKeepBottomAfterLayoutRef.current &&
        !isScrollContainerNearBottom(node)
      ) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
    };

    // Record direct-input scroll gestures so measurement-driven
    // `scrollTop` adjustments in `handleHeightChange` can defer while
    // the user is actively scrolling. See the comment on
    // `lastUserScrollInputTimeRef` above for the failure mode this
    // protects against. Keyboard scrolling (arrow keys,
    // PageUp/PageDown, Home/End, Space) is routed through the same
    // path because browsers fire the same cascade of scroll events,
    // but the `keydown` listener covers the input intent regardless
    // of whether the key actually moves the viewport.
    const markUserScroll = () => {
      lastUserScrollInputTimeRef.current = performance.now();
    };

    syncViewport();
    node.addEventListener("scroll", syncViewport, { passive: true });
    node.addEventListener("wheel", markUserScroll, { passive: true });
    node.addEventListener("touchmove", markUserScroll, { passive: true });
    node.addEventListener("keydown", markUserScroll);
    const resizeObserver = new ResizeObserver(syncViewport);
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", syncViewport);
      node.removeEventListener("wheel", markUserScroll);
      node.removeEventListener("touchmove", markUserScroll);
      node.removeEventListener("keydown", markUserScroll);
      resizeObserver.disconnect();
    };
  }, [isActive, scrollContainerRef, sessionId]);

  // Stabilize handler references so MeasuredMessageCard memo can skip
  // re-renders for messages whose data and position haven't changed,
  // and so event handlers inside the `useEffect`s below can read the
  // latest per-render data (windowed messages, message index map,
  // current render-window size, current viewport visible range)
  // without participating in those effects' dependency arrays.
  const messagesRef = useRef(windowedMessages);
  messagesRef.current = windowedMessages;
  const messageIndexByIdRef = useRef(messageIndexById);
  messageIndexByIdRef.current = messageIndexById;
  // Current render-window size, updated every render so the scroll-up
  // auto-load helper can compute the exact next value synchronously.
  // Storing the computed `nextSize` in `pendingLoadMoreAnchorRef` lets
  // the consuming `useLayoutEffect` distinguish "our grow landed"
  // from "some other code path bumped renderWindowSize" (e.g. the
  // new-message auto-grow during streaming at lines 898-905).
  const renderWindowSizeRef = useRef(renderWindowSize);
  renderWindowSizeRef.current = renderWindowSize;
  // Viewport visible range, mirrored to a ref so the auto-load-older-
  // messages scroll listener can read the current range without
  // participating in the effect's dep array. Putting
  // `viewportVisibleRange.startIndex` in the deps caused the effect
  // to tear down and re-subscribe the native scroll listener on
  // every scroll event (that's what updates `viewport.scrollTop` ->
  // recomputes `viewportVisibleRange`), dropping any wheel event
  // that landed in the window between `removeEventListener` and the
  // next `addEventListener`. The ref pattern matches `messagesRef` /
  // `messageIndexByIdRef` / `renderWindowSizeRef`.
  const viewportVisibleRangeRef = useRef(viewportVisibleRange);
  viewportVisibleRangeRef.current = viewportVisibleRange;

  // Shared "prepend older messages + anchor the viewport" helper used
  // by both the scroll-near-top auto-load effect (below) and the
  // explicit "Load N earlier messages" button (in the render body).
  // Both entry points need the same lifecycle — compute the next
  // render-window size against the current value, bail on no-op,
  // capture the first visible message as the anchor, write
  // `pendingLoadMoreAnchorRef`, call `setRenderWindowSize(nextSize)`.
  // The consuming `useLayoutEffect` then restores the anchor's
  // viewport offset before paint.
  //
  // Previously the button path called `setRenderWindowSize` directly
  // without the anchor, so clicking it painted the grown document at
  // the old `scrollTop` for one frame while the scroll-up path was
  // smooth; routing both sites through this helper makes the button
  // behave identically.
  //
  // `useCallback` with `[scrollContainerRef]` keeps the identity
  // stable across renders so the auto-load effect doesn't re-subscribe
  // its listener whenever render state changes. Caller passes the
  // current full message count; reading `messages.length` through
  // the outer closure is fine because both call sites re-evaluate it
  // at invocation time (the effect depends on `messages.length`, so
  // it re-subscribes when the count changes; the button reads it
  // from render scope).
  const loadMoreEarlierMessages = useCallback((fullMessageCount: number) => {
    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }
    const currentSize = renderWindowSizeRef.current;
    const nextSize = Math.min(currentSize + RENDER_WINDOW_LOAD_MORE, fullMessageCount);
    if (nextSize === currentSize) {
      return;
    }
    const firstVisibleIndex = viewportVisibleRangeRef.current.startIndex;
    const anchorMessage = messagesRef.current[firstVisibleIndex];
    if (!anchorMessage) {
      return;
    }
    const anchorTop = layoutTopsRef.current[firstVisibleIndex] ?? 0;
    pendingLoadMoreAnchorRef.current = {
      anchorMessageId: anchorMessage.id,
      anchorOffsetInViewport: anchorTop - node.scrollTop,
      expectedRenderWindowSize: nextSize,
    };
    setRenderWindowSize(nextSize);
  }, [scrollContainerRef]);

  // Auto-load older messages when the scroll container approaches
  // the top of the rendered window. Before calling
  // `setRenderWindowSize` we record an anchor — the id of the first
  // visible message plus its current offset from `scrollTop`. The
  // separate `useLayoutEffect` below consumes that anchor right
  // after React commits the grown layout (before the browser paints)
  // and writes the `scrollTop` value that keeps the anchor message
  // at the same viewport offset. Two reasons this shape replaced the
  // earlier `requestAnimationFrame`-based approach that read the
  // `scrollHeight` delta:
  //
  //   1. `requestAnimationFrame` fires after React commits but can
  //      fire after the browser has already painted the grown
  //      document at the OLD `scrollTop`, producing a visible
  //      one-frame jump where the user suddenly sees the top of the
  //      prepended block. `useLayoutEffect` runs synchronously
  //      between commit and paint, so the paint only ever lands at
  //      the corrected `scrollTop`.
  //
  //   2. A raw `newScrollHeight - prevScrollHeight` delta is fed by
  //      `layout.totalHeight`, which for freshly-prepended messages
  //      uses `estimateConversationMessageHeight`. When the prepended
  //      cards' `ResizeObserver`s fire asynchronously and the
  //      estimates are replaced with real measurements,
  //      `layout.totalHeight` shifts again and the anchor would bob.
  //      Anchoring on a specific message's viewport offset is stable
  //      against measurement arrivals — the existing
  //      `getAdjustedVirtualizedScrollTopForHeightChange` path keeps
  //      that same offset as each measurement lands.
  useEffect(() => {
    if (!isActive || !hasOlderMessages) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    function handleScroll() {
      if (!node || node.scrollTop > RENDER_WINDOW_LOAD_MORE_THRESHOLD_PX) {
        return;
      }
      loadMoreEarlierMessages(messages.length);
    }

    node.addEventListener("scroll", handleScroll, { passive: true });
    return () => {
      node.removeEventListener("scroll", handleScroll);
    };
  }, [
    hasOlderMessages,
    isActive,
    loadMoreEarlierMessages,
    messages.length,
    scrollContainerRef,
  ]);

  // Apply the pending scroll anchor right after `setRenderWindowSize`
  // commits the grown layout — synchronously before paint. See the
  // scroll-up auto-load `useEffect` above for rationale.
  //
  // The anchor is only applied when the current `renderWindowSize`
  // matches the `expectedRenderWindowSize` captured at scroll-handler
  // time. If those disagree, this effect fired for a different reason
  // (e.g. the new-message auto-grow at lines 898-905 bumped
  // `renderWindowSize` by +1 during streaming) and the anchor is
  // stale from an earlier scroll gesture. Clearing the ref in that
  // case prevents a surprise scroll jump when a streaming delta
  // arrives after the user scrolled up but didn't actually trigger
  // the load-more path.
  useLayoutEffect(() => {
    const anchor = pendingLoadMoreAnchorRef.current;
    if (!anchor) {
      return;
    }
    pendingLoadMoreAnchorRef.current = null;
    if (anchor.expectedRenderWindowSize !== renderWindowSize) {
      return;
    }
    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }
    const newIndex = messageIndexByIdRef.current.get(anchor.anchorMessageId);
    if (newIndex === undefined) {
      return;
    }
    const newAnchorTop = layoutTopsRef.current[newIndex] ?? 0;
    const nextScrollTop = Math.max(newAnchorTop - anchor.anchorOffsetInViewport, 0);
    if (Math.abs(node.scrollTop - nextScrollTop) >= 1) {
      node.scrollTop = nextScrollTop;
    }
  }, [renderWindowSize, scrollContainerRef]);

  const handleHeightChange = useCallback((messageId: string, nextHeight: number) => {
    if (!Number.isFinite(nextHeight) || nextHeight <= 0) {
      return;
    }

    // Round to integer pixels so subpixel drift from `getBoundingClientRect`
    // cannot repeatedly cross the 1-pixel commit threshold below. Storing
    // fractional heights caused a cascading shake: each float-level
    // measurement committed, bumped `layoutVersion`, re-pinned `scrollTop`,
    // reflowed, and then reported a slightly different fractional height on
    // the next ResizeObserver tick. Integer storage eliminates the drift at
    // the source so the re-pin loop cannot be re-entered by noise alone.
    const roundedHeight = Math.round(nextHeight);
    const hadPreviousMeasurement = messageHeightsRef.current[messageId] !== undefined;
    const previousHeight =
      messageHeightsRef.current[messageId] ??
      estimateConversationMessageHeight(messagesRef.current[messageIndexByIdRef.current.get(messageId) ?? 0]);
    if (hadPreviousMeasurement && Math.abs(previousHeight - roundedHeight) < 1) {
      return;
    }

    messageHeightsRef.current[messageId] = roundedHeight;

    const messageIndex = messageIndexByIdRef.current.get(messageId);
    const node = scrollContainerRef.current;
    // If the user/app is already at the latest message, measurements that
    // increase the virtualized height should move the viewport with the bottom.
    const shouldKeepBottom =
      isActive && node
        ? shouldKeepBottomAfterLayoutRef.current ||
          isScrollContainerNearBottom(node)
        : false;
    if (shouldKeepBottom) {
      shouldKeepBottomAfterLayoutRef.current = true;
    }
    if (node && messageIndex !== undefined) {
      // The user-scroll cooldown gates BOTH branches below. During a
      // direct-scroll gesture (wheel / touch / key input in the last
      // `USER_SCROLL_ADJUSTMENT_COOLDOWN_MS`) we never want a streaming
      // measurement to rewrite `scrollTop` out from under the user —
      // not even to re-pin the viewport to the bottom. The original
      // symptom this protects against is "near-bottom streaming
      // re-pins the viewport and fights user scroll-up": while
      // streaming, measurements fire dozens of times per second, and
      // before this gate a user wheeling up within the 72 px
      // near-bottom band would see the pin snap `scrollTop` back to
      // the bottom on every measurement, making it nearly impossible
      // to escape the band by slow wheeling. With the gate, their
      // wheel motion accumulates until `isScrollContainerNearBottom`
      // reports false, `syncViewport` clears the pin flag, and the
      // shouldKeepBottom condition dissolves naturally. When the
      // cooldown expires (user pauses), the pin resumes — so the
      // intended "keep me at bottom while I'm just watching new
      // content arrive" behaviour still works.
      const timeSinceUserScroll =
        performance.now() - lastUserScrollInputTimeRef.current;
      const inUserScrollCooldown =
        timeSinceUserScroll < USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
      if (shouldKeepBottom) {
        if (inUserScrollCooldown) {
          // User is actively scrolling; do not re-pin to bottom even
          // though the heuristic still considers them near-bottom.
          // `shouldKeepBottomAfterLayoutRef` stays set above so the
          // pin naturally resumes once the cooldown elapses (unless
          // `syncViewport` clears it first because the user has
          // scrolled past the 72 px band in the interim).
        } else {
          // Skip no-op writes so a measurement whose rounded height matches
          // the previous pin target cannot trigger an extra scroll event +
          // reflow + ResizeObserver re-fire cycle.
          const target = Math.max(node.scrollHeight - node.clientHeight, 0);
          if (Math.abs(node.scrollTop - target) >= 1) {
            node.scrollTop = target;
          }
        }
      } else if (inUserScrollCooldown) {
        // Defer the anchor-preserving `scrollTop` write for the same
        // reason the pin write is deferred above: during a wheel-up
        // gesture `getAdjustedVirtualizedScrollTopForHeightChange` is
        // driven by previously-unmeasured cards reporting real
        // heights an order of magnitude larger than the estimate, so
        // the compensation would cancel or reverse the user's scroll
        // intent. Layout still rebuilds (the `setLayoutVersion` bump
        // below runs unconditionally), so freshly-measured cards
        // render at their real heights on the next paint; we just
        // don't move the viewport. Once the cooldown expires the
        // anchor-preservation resumes for idle-user cases (streaming
        // updates, syntax highlighting completion, image load).
      } else {
        const nextScrollTop = getAdjustedVirtualizedScrollTopForHeightChange({
          currentScrollTop: node.scrollTop,
          messageTop: layoutTopsRef.current[messageIndex] ?? 0,
          nextHeight: roundedHeight,
          previousHeight,
        });
        if (Math.abs(nextScrollTop - node.scrollTop) >= 1) {
          node.scrollTop = nextScrollTop;
        }
      }
    }

    setLayoutVersion((current) => current + 1);
  }, [isActive, scrollContainerRef]);

  const boundApprovalDecision = useCallback(
    (messageId: string, decision: ApprovalDecision) => onApprovalDecision(sessionId, messageId, decision),
    [sessionId, onApprovalDecision],
  );
  const boundUserInputSubmit = useCallback(
    (messageId: string, answers: Record<string, string[]>) => onUserInputSubmit(sessionId, messageId, answers),
    [sessionId, onUserInputSubmit],
  );
  const boundMcpElicitationSubmit = useCallback(
    (messageId: string, action: McpElicitationAction, content?: JsonValue) =>
      onMcpElicitationSubmit(sessionId, messageId, action, content),
    [sessionId, onMcpElicitationSubmit],
  );
  const boundCodexAppRequestSubmit = useCallback(
    (messageId: string, result: JsonValue) => onCodexAppRequestSubmit(sessionId, messageId, result),
    [sessionId, onCodexAppRequestSubmit],
  );

  if (!isActive) {
    return <div className="virtualized-message-list" style={{ height: layout.totalHeight }} />;
  }

  return (
    <div
      className={`virtualized-message-list${isMeasuringPostActivation ? " is-measuring-post-activation" : ""}`}
      style={{ height: layout.totalHeight }}
    >
      {hasOlderMessages && viewportVisibleRange.startIndex === 0 && (
        <div
          className="load-earlier-messages"
          style={{ position: "absolute", top: 0, left: 0, right: 0 }}
        >
          <button
            type="button"
            className="ghost-button load-earlier-messages-button"
            onClick={() => loadMoreEarlierMessages(messages.length)}
          >
            Load {Math.min(RENDER_WINDOW_LOAD_MORE, windowStartIndex)} earlier
            messages ({windowStartIndex} hidden)
          </button>
        </div>
      )}
      {visibleRanges.flatMap((range) =>
        windowedMessages
          .slice(range.startIndex, range.endIndex)
          .map((message, visibleIndex) => {
            const messageIndex = range.startIndex + visibleIndex;
            return (
              <MeasuredMessageCard
                key={message.id}
                isActive={isActive}
                renderMessageCard={renderMessageCard}
                message={message}
                itemKey={isActive ? `message:${message.id}` : undefined}
                isSearchMatch={conversationSearchMatchedItemKeys.has(`message:${message.id}`)}
                isSearchActive={conversationSearchActiveItemKey === `message:${message.id}`}
                preferImmediateHeavyRender={isActive && messageIndex >= windowedMessages.length - 2}
                top={layout.tops[messageIndex] ?? 0}
                onSearchItemMount={onConversationSearchItemMount}
                onApprovalDecision={boundApprovalDecision}
                onUserInputSubmit={boundUserInputSubmit}
                onMcpElicitationSubmit={boundMcpElicitationSubmit}
                onCodexAppRequestSubmit={boundCodexAppRequestSubmit}
                onHeightChange={handleHeightChange}
              />
            );
          }),
      )}
    </div>
  );
}

const MeasuredMessageCard = memo(function MeasuredMessageCard({
  isActive,
  renderMessageCard,
  message,
  itemKey,
  isSearchMatch,
  isSearchActive,
  preferImmediateHeavyRender,
  onSearchItemMount,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onHeightChange,
  top,
}: {
  isActive: boolean;
  renderMessageCard: RenderMessageCard;
  message: Message;
  itemKey?: string;
  isSearchMatch: boolean;
  isSearchActive: boolean;
  preferImmediateHeavyRender: boolean;
  onSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: BoundUserInputSubmitHandler;
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler;
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler;
  onHeightChange: (messageId: string, nextHeight: number) => void;
  top: number;
}) {
  const slotRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = slotRef.current;
    if (!node) {
      return;
    }

    let frameId = 0;
    const measure = () => {
      frameId = 0;
      onHeightChange(message.id, node.getBoundingClientRect().height);
    };

    measure();
    const resizeObserver = new ResizeObserver(() => {
      if (frameId !== 0) {
        return;
      }

      frameId = window.requestAnimationFrame(measure);
    });
    resizeObserver.observe(node);

    return () => {
      resizeObserver.disconnect();
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [isActive, message, onHeightChange]);

  return (
    <div ref={slotRef} className="virtualized-message-slot" style={{ top }}>
      <MessageSlot
        itemKey={itemKey}
        isSearchMatch={isSearchMatch}
        isSearchActive={isSearchActive}
        onSearchItemMount={onSearchItemMount}
      >
        {renderMessageCard(
          message,
          preferImmediateHeavyRender,
          onApprovalDecision,
          onUserInputSubmit,
          onMcpElicitationSubmit,
          onCodexAppRequestSubmit,
        )}
      </MessageSlot>
    </div>
  );
});
