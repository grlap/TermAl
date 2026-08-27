// Owns: message-stack scrolling, tail-follow state, and search-match scroll
// convergence for SessionPaneView.
// Does not own: pane tab rendering, active-tab selection, source-file loading,
// or transcript card rendering.
// Split from: ui/src/SessionPaneView.tsx.

import {
  startTransition,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FocusEvent as ReactFocusEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type MutableRefObject,
  type RefObject,
  type TouchEvent as ReactTouchEvent,
  type UIEvent as ReactUIEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import {
  buildTurnContentTransition,
  classifyTurnContentTransition,
  didLatestTurnContentChangeBeyondPromptResidency,
  type TurnContentTransition,
} from "./SessionPaneView.content-transition";
import { resolveSessionPaneResizeMeasurement } from "./SessionPaneView.resize-measurement";
import {
  buildMessageListSignature,
  canNestedScrollableConsumeWheel,
  clamp,
  normalizeWheelDelta,
} from "./app-utils";
import { cancelConversationMessageEntryReveals } from "./panels/conversation-message-reveal";
import type { VirtualizedConversationMessageListHandle } from "./panels/VirtualizedConversationMessageList";
import { useCommittedRef } from "./panels/use-committed-ref";
import { useStableEvent } from "./panels/use-stable-event";
import {
  MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  notifyMessageStackScrollWrite,
  writeMessageStackScrollTopImmediately,
  type MessageStackBottomRepinRequestDetail,
  type MessageStackScrollWriteKind,
} from "./message-stack-scroll-sync";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import type { PaneScrollPosition } from "./pane-scroll-position-migration";
import {
  requestSessionHistoryStartPage,
  requestSessionHistoryTailPage,
} from "./session-history-demand";
import {
  captureDetachedPaneScrollPosition,
  createDetachedScrollRestoreController,
  preserveDetachedPaneScrollAnchor,
  type DetachedScrollRestoreController,
} from "./session-pane-detached-restore";
import {
  SESSION_PHYSICAL_BOTTOM_TOLERANCE_PX,
  SESSION_STICKY_BOTTOM_BAND_PX,
  resolveSettledScrollMinimumAttempts,
} from "./scroll-position";
import {
  SESSION_BOTTOM_FOLLOW_MAX_FRAME_MS,
  SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS,
  SESSION_BOTTOM_FOLLOW_SNAP_DISTANCE_PX,
  isFirstAgentOutputForObservedPrompt,
  resolveLatestTurnOutputState,
  resolveLatestTurnTailSignature,
  resolvePostLiveMessageFollowTransition,
  resolveSessionBottomFollowPersistedScrollTop,
  resolveSessionBottomFollowScrollTop,
  resolveSessionBottomFollowWriteScrollTop,
  type LatestTurnOutputState,
} from "./session-live-tail-follow";
import type { SessionSearchMatch } from "./session-find";
import type { Message, Session } from "./types";
import type { PaneViewMode } from "./workspace";

export {
  isFirstAgentOutputForObservedPrompt,
  resolveLatestTurnOutputState,
  resolveLatestTurnTailSignature,
  resolvePostLiveMessageFollowTransition,
  resolveSessionBottomFollowPersistedScrollTop,
  resolveSessionBottomFollowScrollTop,
  resolveSessionBottomFollowWriteScrollTop,
} from "./session-live-tail-follow";

const SESSION_PAGE_SCROLL_VIEWPORT_FACTOR = 0.85;
const SESSION_PAGE_SCROLL_MINIMUM_PX = 160;
const SESSION_BOTTOM_FOLLOW_STABLE_MS =
  SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS * 2;

export function resolveSessionPageScrollDistance(clientHeight: number) {
  return Math.max(
    Math.round(Math.max(0, clientHeight) * SESSION_PAGE_SCROLL_VIEWPORT_FACTOR),
    SESSION_PAGE_SCROLL_MINIMUM_PX,
  );
}

export function canMoveMessageStackByDelta(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  deltaY: number,
) {
  if (!Number.isFinite(deltaY) || Math.abs(deltaY) < 0.5) {
    return false;
  }

  const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);
  if (maxScrollTop <= 0) {
    return false;
  }

  const currentScrollTop = clamp(scrollTop, 0, maxScrollTop);
  const nextScrollTop = clamp(currentScrollTop + deltaY, 0, maxScrollTop);
  return Math.abs(nextScrollTop - currentScrollTop) >= 0.5;
}

export function isMessageStackAtPhysicalBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
) {
  return (
    Math.max(scrollHeight - clientHeight - scrollTop, 0) <=
    SESSION_PHYSICAL_BOTTOM_TOLERANCE_PX
  );
}

export function claimMessageStackBottomRepinAuthority(
  detail: { authorityPresent?: boolean } | undefined,
  currentScrollStateKey: string,
  authorityScrollStateKey: string,
) {
  if (currentScrollStateKey !== authorityScrollStateKey) {
    return false;
  }
  if (detail) {
    detail.authorityPresent = true;
  }
  return true;
}

type NewResponseIndicatorKind = "activity" | "response";

export function resolveNewResponseIndicatorVisibility({
  hasUnloadedNewerHistory,
  indicatorKind,
  liveTailPinned,
}: {
  hasUnloadedNewerHistory: boolean;
  indicatorKind: NewResponseIndicatorKind | null;
  liveTailPinned: boolean;
}) {
  return hasUnloadedNewerHistory || (!liveTailPinned && indicatorKind !== null);
}

type UseSessionPaneScrollStateParams = {
  activeSession: Session | null;
  activeSessionSearchMatch: SessionSearchMatch | null;
  defaultScrollToBottom: boolean;
  deferContentScrollEffects: boolean;
  hasSessionFindQuery: boolean;
  isActive: boolean;
  isSending: boolean;
  isSessionTabActive: boolean;
  onScrollToBottomRequestHandled: (token: number) => void;
  paneContentSignatures: Record<string, string>;
  paneMessageContentSignatures: Record<string, string>;
  paneRootRef: RefObject<HTMLElement | null>;
  paneScrollPositions: Record<string, PaneScrollPosition>;
  paneShouldStickToBottomRef: MutableRefObject<
    Record<string, boolean | undefined>
  >;
  paneViewMode: PaneViewMode;
  pendingScrollToBottomRequest: {
    reattach?: boolean;
    sessionId: string;
    token: number;
  } | null;
  scrollStateKey: string;
  showWaitingIndicator: boolean;
  visibleContentSignature: string;
  visibleLastMessageAuthor: Message["author"] | undefined;
  visibleMessageContentSignature: string;
};

export function useSessionPaneScrollState({
  activeSession,
  activeSessionSearchMatch,
  defaultScrollToBottom,
  deferContentScrollEffects,
  hasSessionFindQuery,
  isActive,
  isSending,
  isSessionTabActive,
  onScrollToBottomRequestHandled,
  paneContentSignatures,
  paneMessageContentSignatures,
  paneRootRef,
  paneScrollPositions,
  paneShouldStickToBottomRef,
  paneViewMode,
  pendingScrollToBottomRequest,
  scrollStateKey,
  showWaitingIndicator,
  visibleContentSignature,
  visibleLastMessageAuthor,
  visibleMessageContentSignature,
}: UseSessionPaneScrollStateParams) {
  const messageStackRef = useRef<HTMLElement | null>(null);
  const virtualizerHandleRef =
    useRef<VirtualizedConversationMessageListHandle | null>(null);
  const settledScrollToBottomCancelRef = useRef<(() => void) | null>(null);
  const settledScrollToBottomKindRef =
    useRef<MessageStackScrollWriteKind | null>(null);
  const previousShowWaitingIndicatorByKeyRef = useRef<
    Record<string, boolean | undefined>
  >({});
  const latestTurnOutputByKeyRef = useRef<
    Record<string, LatestTurnOutputState | undefined>
  >({});
  const latestTurnTailSignatureByKeyRef = useRef<
    Record<string, string | undefined>
  >({});
  const latestMessageTailSignatureByKeyRef = useRef<
    Record<string, string | undefined>
  >({});
  const latestMessageContentSignatureByKeyRef = useRef<
    Record<string, string | undefined>
  >({});
  const latestMessagesByKeyRef = useRef<
    Record<string, readonly Message[] | undefined>
  >({});
  const latestPendingPromptIdsByKeyRef = useRef<
    Record<string, string[] | undefined>
  >({});
  const latestTurnContentTransitionByKeyRef = useRef<
    Record<string, TurnContentTransition | undefined>
  >({});
  // These per-session baselines intentionally survive scrollStateKey changes.
  // App-owned pane signatures and scroll positions survive the same A -> B -> A
  // visit, so clearing the hook-local half on B's activation would make A's
  // next resident-history trim/reveal look like live activity. The complete
  // maps are released with this hook instance when its pane is destroyed. The
  // message baseline retains immutable array references; it does not clone
  // transcript messages or build per-delta marker maps.
  const visibleContentSignatureByKeyRef = useRef<
    Record<string, string | undefined>
  >({});
  const liveFlowActiveByKeyRef = useRef<Record<string, boolean | undefined>>(
    {},
  );
  const awaitingPostLivePromptMessageIdByKeyRef = useRef<
    Record<string, string | null | undefined>
  >({});
  const renderSequenceRef = useRef(0);
  const lastPrePaintRepinRef = useRef<{
    key: string;
    render: number;
    scrollTop: number;
    targetTop: number;
  } | null>(null);
  const paneProgrammaticBottomFollowRef = useRef<{
    key: string | null;
    until: number;
  }>({ key: null, until: Number.NEGATIVE_INFINITY });
  const liveFlowActiveRef = useCommittedRef(
    isSending || showWaitingIndicator,
  );
  const paneTailFollowDetachedByKeyRef = useRef<
    Record<string, true | undefined>
  >({});
  const detachedScrollRestoreControllerRef =
    useRef<DetachedScrollRestoreController | null>(null);
  if (detachedScrollRestoreControllerRef.current === null) {
    detachedScrollRestoreControllerRef.current =
      createDetachedScrollRestoreController();
  }
  const detachedScrollRestoreController =
    detachedScrollRestoreControllerRef.current;
  const currentScrollStateKeyRef = useCommittedRef(scrollStateKey);
  const pendingStartHistoryDemandRef = useRef<{ key: string } | null>(null);
  const pendingTailHistoryDemandRef = useRef<{ key: string } | null>(null);
  const paneLastTouchClientYRef = useRef<number | null>(null);
  const sessionSearchItemRefsRef = useRef<Record<string, HTMLElement | null>>(
    {},
  );
  const [newResponseIndicatorByKey, setNewResponseIndicatorByKey] = useState<
    Record<string, NewResponseIndicatorKind | undefined>
  >({});
  const [liveTailPinnedByKey, setLiveTailPinnedByKey] = useState<
    Record<string, boolean | undefined>
  >({});
  useLayoutEffect(() => {
    renderSequenceRef.current += 1;
  });

  useEffect(() => {
    // Leaving a pane/session must not let its unresolved boundary demand block
    // a fresh request when the reader returns. Each request is also compared by
    // object identity in its completion callback, so a late old completion
    // cannot clear a newer demand for the same key.
    if (pendingStartHistoryDemandRef.current?.key !== scrollStateKey) {
      pendingStartHistoryDemandRef.current = null;
    }
    if (pendingTailHistoryDemandRef.current?.key !== scrollStateKey) {
      pendingTailHistoryDemandRef.current = null;
    }
  }, [scrollStateKey]);

  const savedScrollPosition = paneScrollPositions[scrollStateKey];
  const hasUnloadedNewerHistory = activeSession?.hasNewerHistory === true;
  const newResponseIndicatorKind =
    newResponseIndicatorByKey[scrollStateKey] ?? null;
  const newResponseIndicatorLabel = hasUnloadedNewerHistory
    ? "Jump to latest"
    : newResponseIndicatorKind === "activity"
      ? "New activity"
      : "New response";

  function getTailFollowIntent() {
    if (hasUnloadedNewerHistory) {
      return false;
    }
    return (
      paneShouldStickToBottomRef.current[scrollStateKey] ??
      paneScrollPositions[scrollStateKey]?.shouldStick ??
      true
    );
  }

  function setTailFollowIntent(
    nextValue: boolean,
    options: { preserveDetachedRestore?: boolean } = {},
  ) {
    paneShouldStickToBottomRef.current[scrollStateKey] = nextValue;
    if (nextValue) {
      cancelDetachedMessageStackRestore(scrollStateKey);
      delete paneTailFollowDetachedByKeyRef.current[scrollStateKey];
    } else {
      if (!options.preserveDetachedRestore) {
        cancelDetachedMessageStackRestore(scrollStateKey);
      }
      paneTailFollowDetachedByKeyRef.current[scrollStateKey] = true;
    }
    setLiveTailPinnedByKey((current) => {
      if (current[scrollStateKey] === nextValue) {
        return current;
      }
      return {
        ...current,
        [scrollStateKey]: nextValue,
      };
    });
  }

  // LIVE TURN is always an ordinary in-flow transcript card. Attachment is
  // scroll-controller intent only: it decides whether the whole transcript
  // follows the bottom, never whether the card receives separate positioning.
  const tailFollowIntent =
    liveTailPinnedByKey[scrollStateKey] ??
    savedScrollPosition?.shouldStick ??
    getTailFollowIntent();
  const liveTailPinned = !hasUnloadedNewerHistory && tailFollowIntent;
  // A delayed indicator-state transition must not override explicit
  // attachment. Geometry can briefly include estimated or stale virtual
  // spacer height while residency changes, but a tail-pinned pane has no
  // unseen response to advertise.
  const showNewResponseIndicator = resolveNewResponseIndicatorVisibility({
    hasUnloadedNewerHistory,
    indicatorKind: newResponseIndicatorKind,
    liveTailPinned,
  });

  function hasDetachedTailFollowAuthority() {
    return Boolean(paneTailFollowDetachedByKeyRef.current[scrollStateKey]);
  }

  function markTailFollowDetachedByUser() {
    // Detached authority wins over temporary near-bottom geometry until the
    // user reaches the real bottom or explicitly returns to the live tail.
    paneTailFollowDetachedByKeyRef.current[scrollStateKey] = true;
    cancelSettledScrollToBottom();
    setTailFollowIntent(false);
    const node = messageStackRef.current;
    if (node) {
      // Native keyboard scrolling is animated by Blink. Its first scroll event
      // can move only a fraction of the physical-bottom tolerance, so preserve
      // the exact pre-animation position now. The scroll handler can then
      // recognize that first upward frame as reader movement instead of
      // reattaching tail-follow and bouncing the viewport back down.
      paneScrollPositions[scrollStateKey] =
        captureDetachedPaneScrollPosition(node);
    }
  }

  function keepPaneScrollPositionPinned(node: HTMLElement) {
    paneScrollPositions[scrollStateKey] = {
      top: node.scrollTop,
      shouldStick: true,
    };
  }

  function captureDetachedMessageStackPosition() {
    if (getTailFollowIntent()) {
      return;
    }
    const node = messageStackRef.current;
    if (!node) {
      return;
    }
    // Tab selection is the last synchronous point where the shared stack still
    // contains the outgoing session's fully reconciled mounted range. Capture
    // here rather than in effect cleanup: by cleanup time React may already
    // have reused the node for the incoming tab, losing the outgoing anchor.
    paneScrollPositions[scrollStateKey] =
      captureDetachedPaneScrollPosition(node);
  }

  function beginPaneProgrammaticBottomFollow() {
    paneProgrammaticBottomFollowRef.current = {
      key: scrollStateKey,
      until: performance.now() + MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
    };
  }

  function cancelPaneProgrammaticBottomFollow() {
    paneProgrammaticBottomFollowRef.current = {
      key: null,
      until: Number.NEGATIVE_INFINITY,
    };
  }

  function isPaneProgrammaticBottomFollowActive() {
    const bottomFollow = paneProgrammaticBottomFollowRef.current;
    return (
      bottomFollow.key === scrollStateKey &&
      bottomFollow.until >= performance.now()
    );
  }

  function isSettledProgrammaticBottomFollowActive() {
    return (
      settledScrollToBottomKindRef.current === "bottom_follow" &&
      settledScrollToBottomCancelRef.current !== null
    );
  }

  function cancelDetachedMessageStackRestore(key = scrollStateKey) {
    detachedScrollRestoreController.cancel(key);
  }

  useEffect(() => {
    if (paneProgrammaticBottomFollowRef.current.key !== scrollStateKey) {
      paneProgrammaticBottomFollowRef.current = {
        key: null,
        until: Number.NEGATIVE_INFINITY,
      };
    }
  }, [scrollStateKey]);

  useLayoutEffect(
    () => () => {
      // Live follow survives ordinary message/status commits, but never owns a
      // pane after its scroll scope changes or becomes inactive. Keeping this
      // cleanup separate from content effects prevents status-only commits
      // from cancelling convergence without scheduling a replacement.
      cancelSettledScrollToBottom();
      cancelPaneProgrammaticBottomFollow();
    },
    [isActive, isSessionTabActive, paneViewMode, scrollStateKey],
  );

  useLayoutEffect(
    () => () => {
      // Losing focus to another visible pane does not invalidate this pane's
      // geometry convergence. A tab/mode/key change does: the shared DOM then
      // belongs to a different scroll scope.
      cancelDetachedMessageStackRestore(scrollStateKey);
    },
    [isSessionTabActive, paneViewMode, scrollStateKey],
  );

  function setNewResponseIndicator(
    key: string,
    visible: boolean,
    kind: NewResponseIndicatorKind = "response",
  ) {
    startTransition(() => {
      setNewResponseIndicatorByKey((current) => {
        const currentKind = current[key];
        if (
          (!visible && currentKind === undefined) ||
          (visible &&
            (currentKind === kind ||
              (currentKind === "response" && kind === "activity")))
        ) {
          return current;
        }

        const nextState = { ...current };
        if (visible) {
          nextState[key] = kind;
        } else {
          delete nextState[key];
        }
        return nextState;
      });
    });
  }

  function handleConversationSearchItemMount(
    itemKey: string,
    node: HTMLElement | null,
  ) {
    if (node) {
      sessionSearchItemRefsRef.current[itemKey] = node;
      return;
    }

    delete sessionSearchItemRefsRef.current[itemKey];
  }

  useEffect(() => {
    if (!hasSessionFindQuery) {
      sessionSearchItemRefsRef.current = {};
    }
  }, [activeSession?.id, hasSessionFindQuery]);

  function scrollToLatestMessage(
    behavior: ScrollBehavior,
    force = false,
    scrollKind?: MessageStackScrollWriteKind,
    options: {
      frameDurationMs?: number;
      snapBottomFollowBeforePaint?: boolean;
    } = {},
  ) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const nextScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    const writeScrollTop =
      scrollKind === "bottom_follow"
        ? resolveSessionBottomFollowWriteScrollTop({
            currentScrollTop: node.scrollTop,
            frameDurationMs: options.frameDurationMs,
            snapBeforePaint: options.snapBottomFollowBeforePaint ?? false,
            targetScrollTop: nextScrollTop,
          })
        : nextScrollTop;
    let wroteScrollTop = false;
    if (Math.abs(node.scrollTop - writeScrollTop) > (force ? 0.5 : 1)) {
      node.scrollTo({
        top: writeScrollTop,
        behavior,
      });
      wroteScrollTop = true;
      if (scrollKind === "bottom_follow") {
        beginPaneProgrammaticBottomFollow();
      } else if (scrollKind) {
        cancelPaneProgrammaticBottomFollow();
      }
      notifyMessageStackScrollWrite(node, {
        scrollKind,
      });
    }
    if (
      !wroteScrollTop &&
      scrollKind === "bottom_follow" &&
      options.snapBottomFollowBeforePaint
    ) {
      // A height collapse can make the browser clamp `scrollTop` before this
      // layout effect reads it. There is then no numeric write to emit, but the
      // virtualizer still needs a synchronous bottom-follow notification so it
      // mounts the final page instead of painting a stale spacer-only range.
      beginPaneProgrammaticBottomFollow();
      notifyMessageStackScrollWrite(node, {
        scrollKind,
      });
    }
    setTailFollowIntent(true);
    paneScrollPositions[scrollStateKey] = {
      // Auto writes can be read back synchronously after native clamping.
      // Smooth writes cannot, so preserve the owned destination until their
      // scroll events publish settled geometry instead of saving the stale
      // pre-animation position.
      top: resolveSessionBottomFollowPersistedScrollTop({
        behavior,
        observedScrollTop: node.scrollTop,
        writeScrollTop,
        wroteScrollTop,
      }),
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }

  function repinAttachedLiveContentBeforePaint() {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }
    const currentRender = renderSequenceRef.current;
    const targetTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    const lastRepin = lastPrePaintRepinRef.current;
    if (
      lastRepin?.key === scrollStateKey &&
      lastRepin.render === currentRender &&
      Math.abs(lastRepin.targetTop - targetTop) <= 0.5 &&
      Math.abs(lastRepin.scrollTop - node.scrollTop) <= 0.5
    ) {
      return;
    }
    // Attached content growth is not navigation. Keep the entire transcript
    // in one coordinate system and correct its real bottom in the same commit
    // that mounted or measured the growth. A velocity-bounded rAF follow here
    // would expose one or more painted frames with LIVE TURN displaced.
    cancelSettledScrollToBottom();
    scrollToLatestMessage("auto", true, "bottom_follow", {
      snapBottomFollowBeforePaint: true,
    });
    lastPrePaintRepinRef.current = {
      key: scrollStateKey,
      render: currentRender,
      scrollTop: node.scrollTop,
      targetTop: Math.max(node.scrollHeight - node.clientHeight, 0),
    };
  }

  function scrollVirtualizedMessageStackToBottom(
    node: HTMLElement,
    options: {
      scrollKind?: Extract<
        MessageStackScrollWriteKind,
        "bottom_boundary" | "bottom_pin"
      >;
      scrollSource?: "programmatic" | "user";
    } = {},
  ) {
    if (!node.querySelector(".virtualized-message-list")) {
      return false;
    }

    const nextScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (Math.abs(node.scrollTop - nextScrollTop) > 0.5) {
      writeMessageStackScrollTopImmediately(node, nextScrollTop);
    }
    notifyMessageStackScrollWrite(node, {
      scrollKind: options.scrollKind ?? "bottom_pin",
      scrollSource: options.scrollSource,
    });
    setTailFollowIntent(true);
    paneScrollPositions[scrollStateKey] = {
      top: Number.MAX_SAFE_INTEGER,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
    return true;
  }

  useLayoutEffect(() => {
    const node = messageStackRef.current;
    if (!node || !isSessionTabActive || paneViewMode !== "session") {
      return;
    }

    // Every visible selected session tab owns its transcript's layout authority,
    // even when another pane has keyboard focus. Composer/page measurement may
    // need an immediate, same-task correction before paint in that non-focused
    // pane. Hidden tabs still have no listener, and this handler rejects the
    // request once explicit tail-follow intent has been released by navigation.
    const handleBottomRepinRequest = (event: Event) => {
      const detail =
        event instanceof CustomEvent
          ? (event.detail as
              | Partial<MessageStackBottomRepinRequestDetail>
              | undefined)
          : undefined;
      if (
        !claimMessageStackBottomRepinAuthority(
          detail,
          currentScrollStateKeyRef.current,
          scrollStateKey,
        )
      ) {
        return;
      }
      if (!getTailFollowIntent()) {
        return;
      }
      // Layout requests during live flow belong to the same pre-paint append
      // authority as message commits. This includes Markdown height collapse,
      // composer growth, and command cards gaining measured height.
      if (
        detail?.beforePaint ||
        liveFlowActiveRef.current ||
        isSettledProgrammaticBottomFollowActive()
      ) {
        repinAttachedLiveContentBeforePaint();
        return;
      }
      scrollToLatestMessage("auto", true);
    };

    node.addEventListener(
      MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
      handleBottomRepinRequest,
    );
    return () => {
      node.removeEventListener(
        MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
        handleBottomRepinRequest,
      );
    };
  }, [
    activeSession?.id,
    hasUnloadedNewerHistory,
    // Activation resets the programmatic-bottom-follow latch read by this
    // handler. Re-register after that lifecycle edge so the listener and its
    // captured scroll callbacks share the current activation epoch.
    isActive,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    const node = messageStackRef.current;
    const ResizeObserverCtor = globalThis.ResizeObserver;
    if (
      !node ||
      typeof ResizeObserverCtor !== "function" ||
      hasUnloadedNewerHistory ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session"
    ) {
      return;
    }

    let conversationPage: HTMLElement | null = null;
    let previousContentHeight = 0;
    let previousViewportHeight = node.clientHeight;
    let resizeObserver: ResizeObserver;
    let pageVisibilityObserver: MutationObserver | null = null;

    const bindActiveConversationPage = () => {
      const nextConversationPage = node.querySelector(
        ".session-conversation-page:not([hidden]), .empty-state:not([hidden])",
      );
      const nextPage =
        nextConversationPage instanceof HTMLElement
          ? nextConversationPage
          : null;
      if (nextPage === conversationPage) {
        return false;
      }
      if (conversationPage) {
        resizeObserver.unobserve(conversationPage);
      }
      pageVisibilityObserver?.disconnect();
      conversationPage = nextPage;
      previousContentHeight =
        conversationPage?.getBoundingClientRect().height ?? 0;
      if (conversationPage) {
        resizeObserver.observe(conversationPage);
        pageVisibilityObserver?.observe(conversationPage, {
          attributeFilter: ["hidden"],
          attributes: true,
        });
      }
      return true;
    };

    const repinAfterRelevantResize = () => {
      const activePageChanged = bindActiveConversationPage();
      const nextContentHeight =
        conversationPage?.getBoundingClientRect().height ?? 0;
      const nextViewportHeight = node.clientHeight;
      const shouldRepinEveryMeasuredPixel =
        liveFlowActiveRef.current ||
        isSettledProgrammaticBottomFollowActive();
      // Mermaid and other asynchronously measured cards can alternate by a
      // pixel or two while idle. Ignore that harmless jitter, but retain the
      // sub-pixel sensitivity needed while output is actively streaming.
      const resizeMeasurement = resolveSessionPaneResizeMeasurement({
        activePageChanged,
        nextContentHeight,
        nextViewportHeight,
        previousContentHeight,
        previousViewportHeight,
        shouldRepinEveryMeasuredPixel,
      });
      // Keep the last handled baseline when idle jitter is suppressed. This
      // lets same-direction one/two-pixel refinements accumulate past the
      // threshold without reacting to a harmless back-and-forth wobble.
      previousContentHeight = resizeMeasurement.nextContentHeightBaseline;
      previousViewportHeight = resizeMeasurement.nextViewportHeightBaseline;
      if (!resizeMeasurement.shouldRepin) {
        return;
      }
      if (
        currentScrollStateKeyRef.current !== scrollStateKey ||
        !getTailFollowIntent()
      ) {
        return;
      }
      // ResizeObserver runs before paint. During attached live flow every
      // measured growth or collapse must converge here; deferring growth to a
      // smooth animation frame makes LIVE TURN visibly jump first.
      if (shouldRepinEveryMeasuredPixel) {
        repinAttachedLiveContentBeforePaint();
        return;
      }
      // ResizeObserver callbacks run before paint. Correct synchronously through
      // the pane's single authority so a pinned user never sees the old top
      // followed by a second-frame snap to the new bottom.
      scrollToLatestMessage("auto", true);
    };

    resizeObserver = new ResizeObserverCtor(() => repinAfterRelevantResize());

    const MutationObserverCtor = globalThis.MutationObserver;
    const rootChildObserver =
      typeof MutationObserverCtor === "function"
        ? new MutationObserverCtor(() => repinAfterRelevantResize())
        : null;
    pageVisibilityObserver =
      typeof MutationObserverCtor === "function"
        ? new MutationObserverCtor(() => repinAfterRelevantResize())
        : null;
    bindActiveConversationPage();

    // The composer is a sibling of the message stack. Observe the stable pane
    // frame rather than the stack itself: pane/window resizing changes this
    // frame, while textarea growth is handled by the explicit repin request and
    // does not generate a second correction for each transition frame.
    const paneFrame = paneRootRef.current;
    if (paneFrame && paneFrame !== node) {
      resizeObserver.observe(paneFrame);
    }

    // Page replacement is structural: the active conversation page is a direct
    // message-stack child. Observe only that boundary plus the current page's
    // `hidden` attribute. A subtree observer runs for every streamed text-node
    // mutation and turns token delivery into repeated synchronous layout reads.
    rootChildObserver?.observe(node, {
      childList: true,
    });
    const handleWindowResize = () => repinAfterRelevantResize();
    window.addEventListener("resize", handleWindowResize);

    return () => {
      resizeObserver.disconnect();
      rootChildObserver?.disconnect();
      pageVisibilityObserver?.disconnect();
      window.removeEventListener("resize", handleWindowResize);
    };
  }, [
    activeSession?.id,
    hasUnloadedNewerHistory,
    isActive,
    isSending,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
    showWaitingIndicator,
  ]);

  useLayoutEffect(() => {
    const messages = activeSession?.messages ?? [];
    const currentLiveFlowActive = liveFlowActiveRef.current;
    const previousLiveFlowActive =
      liveFlowActiveByKeyRef.current[scrollStateKey] ?? false;
    liveFlowActiveByKeyRef.current[scrollStateKey] = currentLiveFlowActive;
    const previousVisibleContentSignature =
      visibleContentSignatureByKeyRef.current[scrollStateKey];
    visibleContentSignatureByKeyRef.current[scrollStateKey] =
      visibleContentSignature;
    const currentTurnOutput = resolveLatestTurnOutputState(messages);
    const previousTurnOutput = latestTurnOutputByKeyRef.current[scrollStateKey];
    latestTurnOutputByKeyRef.current[scrollStateKey] = currentTurnOutput;
    const currentTurnTailSignature = resolveLatestTurnTailSignature(messages);
    const previousTurnTailSignature =
      latestTurnTailSignatureByKeyRef.current[scrollStateKey];
    latestTurnTailSignatureByKeyRef.current[scrollStateKey] =
      currentTurnTailSignature;
    const tailMessage = messages[messages.length - 1];
    const currentMessageTailSignature = buildMessageListSignature(
      tailMessage ? [tailMessage] : [],
    );
    const previousMessageTailSignature =
      latestMessageTailSignatureByKeyRef.current[scrollStateKey];
    latestMessageTailSignatureByKeyRef.current[scrollStateKey] =
      currentMessageTailSignature;
    const previousMessageContentSignature =
      latestMessageContentSignatureByKeyRef.current[scrollStateKey];
    latestMessageContentSignatureByKeyRef.current[scrollStateKey] =
      visibleMessageContentSignature;
    const previousMessages = latestMessagesByKeyRef.current[scrollStateKey];
    latestMessagesByKeyRef.current[scrollStateKey] = messages;
    const currentPendingPromptIds = (activeSession?.pendingPrompts ?? [])
      .filter((prompt) => !prompt.localOnly)
      .map((prompt) => prompt.id);
    const previousPendingPromptIds =
      latestPendingPromptIdsByKeyRef.current[scrollStateKey];
    latestPendingPromptIdsByKeyRef.current[scrollStateKey] =
      currentPendingPromptIds;
    const latestTurnChanged =
      previousTurnTailSignature !== undefined &&
      previousTurnTailSignature !== currentTurnTailSignature;
    const promptResidencyChanged =
      previousTurnOutput !== undefined &&
      previousTurnOutput.promptMessageId !== currentTurnOutput.promptMessageId &&
      (previousTurnOutput.promptMessageId === null ||
        currentTurnOutput.promptMessageId === null);
    latestTurnContentTransitionByKeyRef.current[scrollStateKey] =
      buildTurnContentTransition({
        lastConsumedMessageContentSignature:
          paneMessageContentSignatures[scrollStateKey],
        latestTurnChangedBeyondPromptResidency:
          didLatestTurnContentChangeBeyondPromptResidency({
            currentMessages: messages,
            currentPromptMessageId: currentTurnOutput.promptMessageId,
            latestTurnChanged,
            previousMessages,
            previousPromptMessageId: previousTurnOutput?.promptMessageId,
            promptResidencyChanged,
          }),
        pendingPromptsAdvanced:
          previousPendingPromptIds !== undefined &&
          currentPendingPromptIds.some(
            (promptId) => !previousPendingPromptIds.includes(promptId),
          ),
        tailMessageChanged:
          previousMessageTailSignature !== undefined &&
          previousMessageTailSignature !== currentMessageTailSignature,
        previousMessageContentSignature,
        previousTransition:
          latestTurnContentTransitionByKeyRef.current[scrollStateKey],
        toMessageContentSignature: visibleMessageContentSignature,
      });

    const postLiveMessageTransition = resolvePostLiveMessageFollowTransition({
      awaitingPromptMessageId:
        awaitingPostLivePromptMessageIdByKeyRef.current[scrollStateKey],
      currentLiveFlowActive,
      currentPromptMessageId: currentTurnOutput.promptMessageId,
      latestTurnContentChanged:
        previousTurnTailSignature !== undefined &&
        previousTurnTailSignature !== currentTurnTailSignature,
      previousLiveFlowActive,
    });
    if (
      postLiveMessageTransition.awaitingPostLivePromptMessageId !== undefined
    ) {
      awaitingPostLivePromptMessageIdByKeyRef.current[scrollStateKey] =
        postLiveMessageTransition.awaitingPostLivePromptMessageId;
    } else {
      delete awaitingPostLivePromptMessageIdByKeyRef.current[scrollStateKey];
    }

    const receivedFirstOutputForPrompt = isFirstAgentOutputForObservedPrompt(
      previousTurnOutput,
      currentTurnOutput,
    );
    const changedLiveContent =
      previousVisibleContentSignature !== undefined &&
      previousVisibleContentSignature !== visibleContentSignature &&
      (currentLiveFlowActive ||
        postLiveMessageTransition.shouldFollowPostLiveMessage ||
        receivedFirstOutputForPrompt);
    if (
      (!receivedFirstOutputForPrompt && !changedLiveContent) ||
      hasUnloadedNewerHistory ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session" ||
      !getTailFollowIntent()
    ) {
      return;
    }

    // Attached appends are synchronized in this layout effect. The committed
    // tree and its real bottom therefore reach paint together; smooth follow
    // is reserved for explicit navigation and reattachment, not streaming.
    // Turn status and the final assistant message can arrive in separate SSE
    // commits. The per-key latch survives a status-only idle commit, but stays
    // bound to that turn's prompt identity. A later prompt clears it without
    // following, while the matching final message still receives pre-paint
    // synchronization even when earlier command/progress output means this is
    // not first output.
    // A browser clamp can make a live reparse require no numeric write. The
    // snap helper still notifies the virtualizer synchronously in that case.
    repinAttachedLiveContentBeforePaint();
  }, [
    activeSession?.messages,
    activeSession?.pendingPrompts,
    hasUnloadedNewerHistory,
    isActive,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
    isSending,
    showWaitingIndicator,
    visibleContentSignature,
    visibleMessageContentSignature,
  ]);

  function scrollMessageStackByDelta(
    deltaY: number,
    options: {
      scrollKind?: MessageStackScrollWriteKind;
    } = {},
  ) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    // Page controls and normalized wheel/touch paths are explicit navigation.
    // They must take authority before any write, including when a retained
    // detached-restore frame has already been dequeued by the browser.
    cancelDetachedMessageStackRestore(scrollStateKey);

    if (
      !canMoveMessageStackByDelta(
        node.scrollTop,
        node.scrollHeight,
        node.clientHeight,
        deltaY,
      )
    ) {
      return;
    }

    cancelConversationMessageEntryReveals(node);

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    const nextScrollTop = clamp(node.scrollTop + deltaY, 0, maxScrollTop);
    const landsAtPhysicalBottom = isMessageStackAtPhysicalBottom(
      nextScrollTop,
      node.scrollHeight,
      node.clientHeight,
    );
    if (!landsAtPhysicalBottom) {
      // This is the shared first-write path for native wheel/trackpad input
      // and explicit page controls. React's delegated wheel handler observes
      // the native listener only after it has called preventDefault(), so
      // transfer scroll authority here before the first write. LIVE TURN is
      // already in the transcript's coordinate system and needs no separate
      // presentation handoff. A delta reaching bottom stays attached.
      // Manual navigation is direction-independent and remains detached until
      // the physical bottom; the shared sticky-bottom band only absorbs
      // layout jitter while already attached.
      markTailFollowDetachedByUser();
    }
    cancelPaneProgrammaticBottomFollow();
    // A pane-owned PageUp/PageDown or wheel delta must also abort any native
    // smooth bottom-follow animation already running in Blink. The old stale
    // prepend restore used to cancel that animation as an accidental side
    // effect; generation-based stale-restore rejection correctly performs no
    // DOM write, so ownership transfer must be explicit at the input writer.
    writeMessageStackScrollTopImmediately(node, nextScrollTop);
    notifyMessageStackScrollWrite(node, {
      scrollKind: options.scrollKind,
      scrollSource: "user",
    });
    paneScrollPositions[scrollStateKey] = landsAtPhysicalBottom
      ? { top: node.scrollTop, shouldStick: true }
      : captureDetachedPaneScrollPosition(node);
    if (landsAtPhysicalBottom) {
      setTailFollowIntent(true);
      setNewResponseIndicator(scrollStateKey, false);
    }
  }

  function followLatestMessageForPromptSend() {
    // A historical window may retain stale bottom-follow state from an older
    // residency snapshot. Never interpret that as permission to follow the
    // bottom of the loaded slice: keep the viewport stable and let the
    // explicit Jump to latest action request the real tail pages.
    if (hasUnloadedNewerHistory) {
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return undefined;
    }
    if (!getTailFollowIntent()) {
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return undefined;
    }
    repinAttachedLiveContentBeforePaint();
    return undefined;
  }

  function scrollMessageStackByPage(direction: -1 | 1) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    scrollMessageStackByDelta(
      resolveSessionPageScrollDistance(node.clientHeight) * direction,
      {
        scrollKind: "page_jump",
      },
    );
  }

  function scrollSessionMessageStackByPageJump(direction: -1 | 1) {
    scrollMessageStackByPage(direction);
  }

  function scrollMessageStackToBoundary(boundary: "top" | "bottom") {
    const currentNode = messageStackRef.current;
    if (currentNode) {
      cancelConversationMessageEntryReveals(currentNode);
    }
    if (boundary === "bottom") {
      const applyBottomBoundary = () => {
        // Explicit navigation owns the viewport before it emits any scroll
        // write. A synchronous virtualizer reconciliation must not observe an
        // older detached-restore controller and replay its saved target.
        cancelDetachedMessageStackRestore(scrollStateKey);
        cancelSettledScrollToBottom();
        cancelPaneProgrammaticBottomFollow();
        const node = messageStackRef.current;
        if (node) {
          setTailFollowIntent(true);
          if (
            !scrollVirtualizedMessageStackToBottom(node, {
              scrollKind: "bottom_boundary",
              scrollSource: "user",
            })
          ) {
            scrollToLatestMessage("auto", true, "seek");
          }
        }
      };
      if (hasUnloadedNewerHistory && activeSession) {
        if (pendingTailHistoryDemandRef.current?.key === scrollStateKey) {
          return;
        }
        setTailFollowIntent(false);
        setNewResponseIndicator(scrollStateKey, true);
        const requestedScrollStateKey = scrollStateKey;
        const demand = { key: requestedScrollStateKey };
        pendingTailHistoryDemandRef.current = demand;
        void requestSessionHistoryTailPage(activeSession.id).then((applied) => {
          if (pendingTailHistoryDemandRef.current === demand) {
            pendingTailHistoryDemandRef.current = null;
          }
          if (
            !applied ||
            currentScrollStateKeyRef.current !== requestedScrollStateKey
          ) {
            return;
          }
          requestAnimationFrame(() => {
            if (currentScrollStateKeyRef.current === requestedScrollStateKey) {
              applyBottomBoundary();
            }
          });
        });
        return;
      }
      applyBottomBoundary();
      return;
    }

    const applyTopBoundary = () => {
      const node = messageStackRef.current;
      if (!node) {
        return;
      }
      cancelSettledScrollToBottom();
      cancelPaneProgrammaticBottomFollow();
      node.scrollTo({
        top: 0,
        behavior: "auto",
      });
      notifyMessageStackScrollWrite(node, {
        scrollKind: "seek",
        scrollSource: "user",
      });
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    };
    const needsTrueStartPage =
      activeSession &&
      (activeSession.hasOlderHistory ??
        (activeSession.messagesLoaded === false &&
          activeSession.hasNewerHistory !== true));
    if (!needsTrueStartPage) {
      applyTopBoundary();
      return;
    }
    if (pendingStartHistoryDemandRef.current?.key === scrollStateKey) {
      return;
    }
    const requestedScrollStateKey = scrollStateKey;
    const demand = { key: requestedScrollStateKey };
    pendingStartHistoryDemandRef.current = demand;
    void requestSessionHistoryStartPage(activeSession.id).then(() => {
      if (pendingStartHistoryDemandRef.current === demand) {
        pendingStartHistoryDemandRef.current = null;
      }
      if (currentScrollStateKeyRef.current !== requestedScrollStateKey) {
        return;
      }
      // A failed/superseded page request still honors the user's navigation
      // against the resident window. Silently swallowing the keypress makes a
      // transient history race indistinguishable from broken input.
      requestAnimationFrame(() => {
        if (currentScrollStateKeyRef.current === requestedScrollStateKey) {
          applyTopBoundary();
        }
      });
    });
  }

  const handleMessageStackWheel = useStableEvent(
    function handleMessageStackWheel(event: WheelEvent) {
      if (event.defaultPrevented || event.ctrlKey) {
        return;
      }

      const node = messageStackRef.current;
      if (!node) {
        return;
      }

      const deltaY = normalizeWheelDelta(event, node);
      if (Math.abs(deltaY) < 0.5) {
        return;
      }

      if (canNestedScrollableConsumeWheel(event.target, node, deltaY)) {
        return;
      }

      // A wheel gesture at the physical boundary (or in a transcript shorter
      // than its viewport) did not navigate away from the live tail. Detaching
      // here would turn harmless trackpad overscroll into a silent follow-off.
      if (
        !canMoveMessageStackByDelta(
          node.scrollTop,
          node.scrollHeight,
          node.clientHeight,
          deltaY,
        )
      ) {
        return;
      }

      event.preventDefault();
      scrollMessageStackByDelta(deltaY, {
        scrollKind: "incremental",
      });
    },
  );

  useEffect(() => {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }
    const listener = (event: WheelEvent) => handleMessageStackWheel(event);
    node.addEventListener("wheel", listener, { passive: false });
    return () => {
      node.removeEventListener("wheel", listener);
    };
  }, [handleMessageStackWheel]);

  const handleNestedTargetPageKey = useStableEvent(
    function handleNestedTargetPageKey(event: KeyboardEvent) {
      if (
        event.defaultPrevented ||
        (event.key !== "PageUp" && event.key !== "PageDown") ||
        !isNestedEditablePageKeyTarget(event.target)
      ) {
        return;
      }
      if (
        !(event.target instanceof Node) ||
        !paneRootRef.current?.contains(event.target)
      ) {
        return;
      }

      const command = resolvePaneScrollCommand(
        {
          altKey: event.altKey,
          ctrlKey: event.ctrlKey,
          key: event.key,
          metaKey: event.metaKey,
          shiftKey: event.shiftKey,
        },
        event.target,
      );
      if (!command) {
        return;
      }

      event.preventDefault();
      if (command.kind === "boundary") {
        scrollMessageStackToBoundary(
          command.direction === "up" ? "top" : "bottom",
        );
        return;
      }

      scrollSessionMessageStackByPageJump(command.direction === "up" ? -1 : 1);
    },
  );

  const handleDocumentMessageStackUpwardKey = useStableEvent(
    function handleDocumentMessageStackUpwardKey(event: KeyboardEvent) {
      const node = messageStackRef.current;
      if (
        !isActive ||
        !isSessionTabActive ||
        paneViewMode !== "session" ||
        !node ||
        !isUpwardMessageStackKeyIntent(event) ||
        !isMessageStackKeyboardEventInActivePane(event, node, paneRootRef.current) ||
        !canMoveMessageStackByDelta(
          node.scrollTop,
          node.scrollHeight,
          node.clientHeight,
          -1,
        )
      ) {
        return;
      }

      // Chromium can keep keyboard-scroll ownership on the transcript after a
      // click on non-focusable message content even though document.body is the
      // key event target. React's message-stack onKeyDown never sees that path,
      // so claim detached authority before the browser emits its first animated
      // upward scroll frame. Otherwise that frame is recorded as attached and
      // an A -> B -> A tab switch restores the live bottom instead of the
      // reader's anchored offset.
      cancelConversationMessageEntryReveals(node);
      cancelDetachedMessageStackRestore(scrollStateKey);
      cancelPaneProgrammaticBottomFollow();
      if (!hasDetachedTailFollowAuthority() || getTailFollowIntent()) {
        markTailFollowDetachedByUser();
      }
    },
  );

  useEffect(() => {
    if (!isActive || paneViewMode !== "session") {
      return;
    }

    const listener = (event: KeyboardEvent) => handleNestedTargetPageKey(event);
    window.addEventListener("keydown", listener, true);
    return () => {
      window.removeEventListener("keydown", listener, true);
    };
  }, [handleNestedTargetPageKey, isActive, paneViewMode]);

  useEffect(() => {
    if (!isActive || !isSessionTabActive || paneViewMode !== "session") {
      return;
    }

    document.addEventListener(
      "keydown",
      handleDocumentMessageStackUpwardKey,
      true,
    );
    return () => {
      document.removeEventListener(
        "keydown",
        handleDocumentMessageStackUpwardKey,
        true,
      );
    };
  }, [
    handleDocumentMessageStackUpwardKey,
    isActive,
    isSessionTabActive,
    paneViewMode,
  ]);

  function scheduleSettledScrollToBottom(
    behavior: ScrollBehavior,
    options: {
      maxAttempts?: number;
      minAttempts?: number;
      onComplete?: () => void;
      preferVirtualizedBoundary?: boolean;
      scrollKind?: MessageStackScrollWriteKind;
    } = {},
  ) {
    cancelSettledScrollToBottom();

    const scheduledScrollStateKey = scrollStateKey;
    let frameId = 0;
    let cancelled = false;
    let completed = false;
    const maxAttempts = options.maxAttempts ?? 12;
    const minimumAttempts = resolveSettledScrollMinimumAttempts(
      maxAttempts,
      options.minAttempts,
    );
    const maximumDurationMs =
      maxAttempts * SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS;
    const minimumDurationMs =
      minimumAttempts * SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS;
    let previousScrollHeight = -1;
    let elapsedMs = 0;
    let stableDurationMs = 0;
    let previousFrameTimestamp: number | null = null;

    function complete() {
      if (cancelled || completed) {
        return;
      }

      completed = true;
      if (settledScrollToBottomCancelRef.current === cancel) {
        settledScrollToBottomCancelRef.current = null;
        settledScrollToBottomKindRef.current = null;
      }
      options.onComplete?.();
    }

    const tick = (frameTimestamp: number) => {
      frameId = 0;
      // cancelAnimationFrame cannot retract a callback the browser has already
      // dequeued. Re-check ownership inside the callback so a user detach or
      // scroll-scope change cannot be followed by one stale bottom write.
      if (cancelled || completed) {
        return;
      }
      if (currentScrollStateKeyRef.current !== scheduledScrollStateKey) {
        // The pane reuses one scroll container across session tabs. A frame
        // queued by the outgoing tab must never write into the incoming tab.
        cancel();
        return;
      }
      if (
        options.scrollKind === "bottom_follow" &&
        (!getTailFollowIntent() || hasDetachedTailFollowAuthority())
      ) {
        cancelPaneProgrammaticBottomFollow();
        complete();
        return;
      }
      const measuredFrameDurationMs =
        previousFrameTimestamp === null
          ? SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS
          : frameTimestamp - previousFrameTimestamp;
      const frameDurationMs =
        Number.isFinite(measuredFrameDurationMs) && measuredFrameDurationMs > 0
          ? Math.min(
              measuredFrameDurationMs,
              SESSION_BOTTOM_FOLLOW_MAX_FRAME_MS,
            )
          : SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS;
      previousFrameTimestamp = frameTimestamp;
      elapsedMs += frameDurationMs;
      const node = messageStackRef.current;
      if (!node) {
        if (elapsedMs < maximumDurationMs) {
          frameId = window.requestAnimationFrame(tick);
        } else {
          complete();
        }
        return;
      }

      if (
        options.preferVirtualizedBoundary &&
        scrollVirtualizedMessageStackToBottom(node)
      ) {
        complete();
        return;
      }

      scrollToLatestMessage(
        behavior,
        elapsedMs <= minimumDurationMs,
        options.scrollKind,
        { frameDurationMs },
      );

      const bottomGap = Math.max(
        node.scrollHeight - node.clientHeight - node.scrollTop,
        0,
      );
      const settledBottomGap =
        options.scrollKind === "bottom_follow"
          ? SESSION_BOTTOM_FOLLOW_SNAP_DISTANCE_PX
          : SESSION_PHYSICAL_BOTTOM_TOLERANCE_PX;
      const heightStable =
        previousScrollHeight >= 0 &&
        Math.abs(node.scrollHeight - previousScrollHeight) <= 16;
      if (bottomGap <= settledBottomGap && heightStable) {
        stableDurationMs += frameDurationMs;
      } else {
        stableDurationMs = 0;
      }

      previousScrollHeight = node.scrollHeight;
      if (
        elapsedMs < maximumDurationMs &&
        (elapsedMs < minimumDurationMs ||
          stableDurationMs < SESSION_BOTTOM_FOLLOW_STABLE_MS)
      ) {
        frameId = window.requestAnimationFrame(tick);
      } else {
        if (
          options.scrollKind === "bottom_follow" &&
          bottomGap > settledBottomGap &&
          getTailFollowIntent() &&
          !hasDetachedTailFollowAuthority()
        ) {
          // The velocity budget is intentionally finite, but attachment is a
          // stronger contract: once that budget expires, finish at the current
          // physical bottom so the latest response cannot remain hidden with
          // no indicator or future wake-up.
          scrollToLatestMessage("auto", true, "bottom_follow", {
            snapBottomFollowBeforePaint: true,
          });
        }
        complete();
      }
    };

    const cancel = () => {
      cancelled = true;
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
      if (settledScrollToBottomCancelRef.current === cancel) {
        settledScrollToBottomCancelRef.current = null;
        settledScrollToBottomKindRef.current = null;
      }
    };

    settledScrollToBottomCancelRef.current = cancel;
    settledScrollToBottomKindRef.current = options.scrollKind ?? null;
    frameId = window.requestAnimationFrame(tick);
    return cancel;
  }

  function cancelSettledScrollToBottom() {
    const cancel = settledScrollToBottomCancelRef.current;
    settledScrollToBottomCancelRef.current = null;
    settledScrollToBottomKindRef.current = null;
    cancel?.();
  }

  function handleMessageStackTouchStart(event: ReactTouchEvent<HTMLElement>) {
    paneLastTouchClientYRef.current = event.touches[0]?.clientY ?? null;
  }

  function isManualMessageStackScrollInput(
    event:
      | ReactWheelEvent<HTMLElement>
      | ReactTouchEvent<HTMLElement>
      | ReactKeyboardEvent<HTMLElement>
      | ReactMouseEvent<HTMLElement>,
  ) {
    const node = messageStackRef.current;
    if (event.type === "wheel" && "deltaY" in event) {
      return Boolean(
        !event.defaultPrevented &&
          node &&
          canMoveMessageStackByDelta(
            node.scrollTop,
            node.scrollHeight,
            node.clientHeight,
            event.deltaY,
          ),
      );
    }

    if (event.type === "touchmove" && "touches" in event) {
      const currentTouchClientY = event.touches[0]?.clientY ?? null;
      const previousTouchClientY = paneLastTouchClientYRef.current;
      paneLastTouchClientYRef.current = currentTouchClientY;
      const deltaY =
        currentTouchClientY !== null && previousTouchClientY !== null
          ? previousTouchClientY - currentTouchClientY
          : 0;
      return Boolean(
        node &&
          canMoveMessageStackByDelta(
            node.scrollTop,
            node.scrollHeight,
            node.clientHeight,
            deltaY,
          ),
      );
    }

    if (event.type === "keydown" && "key" in event) {
      if (
        event.defaultPrevented ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        isInteractiveMessageStackKeyTarget(event.target, event.currentTarget)
      ) {
        return false;
      }
      return (
        event.key === "PageUp" ||
        event.key === "PageDown" ||
        event.key === "ArrowUp" ||
        event.key === "ArrowDown" ||
        event.key === "Home" ||
        event.key === "End" ||
        event.key === " "
      );
    }

    return event.type === "mousedown" && event.target === event.currentTarget;
  }

  function handleMessageStackUserScrollIntent(
    event:
      | ReactWheelEvent<HTMLElement>
      | ReactTouchEvent<HTMLElement>
      | ReactKeyboardEvent<HTMLElement>
      | ReactMouseEvent<HTMLElement>,
  ) {
    // Nested scrollables (code/table panes), transcript controls, and gestures
    // that cannot change scrollTop do not transfer authority away from live
    // follow. Only a real transcript-navigation intent detaches the in-flow
    // LIVE TURN from the viewport bottom.
    const node = messageStackRef.current;
    if (
      event.type === "wheel" &&
      "deltaY" in event &&
      node &&
      canNestedScrollableConsumeWheel(event.target, node, event.deltaY)
    ) {
      return;
    }
    if (!isManualMessageStackScrollInput(event)) {
      return;
    }
    if (node) {
      cancelConversationMessageEntryReveals(node);
    }
    cancelDetachedMessageStackRestore(scrollStateKey);
    cancelPaneProgrammaticBottomFollow();
    if (!hasDetachedTailFollowAuthority() || getTailFollowIntent()) {
      markTailFollowDetachedByUser();
    }
  }

  function handleMessageStackFocusCapture(
    event: ReactFocusEvent<HTMLElement>,
  ) {
    if (event.target === event.currentTarget) {
      return;
    }
    // Keyboard focus inside the transcript can make the browser call
    // scrollIntoView without a wheel/key scroll event. That navigation owns
    // the viewport and must not be reverted by a pending detached restore.
    cancelDetachedMessageStackRestore(scrollStateKey);
  }

  function handleMessageStackScroll(event: ReactUIEvent<HTMLElement>) {
    const node = event.currentTarget;
    if (
      detachedScrollRestoreController.consumeNativeScroll({
        key: scrollStateKey,
        node,
        publishSavedTarget: (targetTop) => {
          setTailFollowIntent(false, { preserveDetachedRestore: true });
          paneScrollPositions[scrollStateKey] = preserveDetachedPaneScrollAnchor(
            paneScrollPositions[scrollStateKey],
            targetTop,
          );
          if (hasUnloadedNewerHistory) {
            setNewResponseIndicator(scrollStateKey, true);
          }
        },
      })
    ) {
      return;
    }
    const previousScrollPosition = paneScrollPositions[scrollStateKey];
    const previousTop = previousScrollPosition?.top;
    const movedUpFromRecordedPosition =
      typeof previousTop === "number" &&
      previousTop < Number.MAX_SAFE_INTEGER / 2 &&
      node.scrollTop < previousTop - 1;
    const hasDetachedTailFollow = hasDetachedTailFollowAuthority();
    const movedUpAfterUserEscape =
      hasDetachedTailFollow &&
      typeof previousTop === "number" &&
      previousTop < Number.MAX_SAFE_INTEGER / 2 &&
      node.scrollTop < previousTop;
    const shouldStick =
      node.scrollHeight - node.scrollTop - node.clientHeight <
      SESSION_STICKY_BOTTOM_BAND_PX;
    const isAtPhysicalBottom = isMessageStackAtPhysicalBottom(
      node.scrollTop,
      node.scrollHeight,
      node.clientHeight,
    );
    if (hasUnloadedNewerHistory) {
      cancelPaneProgrammaticBottomFollow();
      cancelSettledScrollToBottom();
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] =
        captureDetachedPaneScrollPosition(node);
      setNewResponseIndicator(scrollStateKey, true);
      return;
    }
    if (isPaneProgrammaticBottomFollowActive()) {
      const targetTop = Math.max(node.scrollHeight - node.clientHeight, 0);
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: targetTop,
        shouldStick: true,
      };
      setNewResponseIndicator(scrollStateKey, false);
      if (
        targetTop - node.scrollTop <= SESSION_PHYSICAL_BOTTOM_TOLERANCE_PX
      ) {
        cancelPaneProgrammaticBottomFollow();
      }
      return;
    }
    if (movedUpAfterUserEscape) {
      paneScrollPositions[scrollStateKey] =
        captureDetachedPaneScrollPosition(node);
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else if (
      hasDetachedTailFollow &&
      !isAtPhysicalBottom
    ) {
      // A manual gesture owns the viewport until it reaches the real bottom.
      // The wider sticky-bottom band is useful for absorbing layout jitter
      // while attached, but must never re-enable bottom-follow intent after a
      // small deliberate scroll away from the tail.
      paneScrollPositions[scrollStateKey] =
        captureDetachedPaneScrollPosition(node);
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else if (shouldStick) {
      // Reaching the physical bottom is the natural reattachment action.
      paneScrollPositions[scrollStateKey] = {
        top: node.scrollTop,
        shouldStick: true,
      };
      setTailFollowIntent(true);
      setNewResponseIndicator(scrollStateKey, false);
    } else if (
      hasDetachedTailFollow ||
      movedUpFromRecordedPosition ||
      !getTailFollowIntent()
    ) {
      paneScrollPositions[scrollStateKey] =
        captureDetachedPaneScrollPosition(node);
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else {
      keepPaneScrollPositionPinned(node);
      setNewResponseIndicator(scrollStateKey, false);
    }
  }

  function scheduleDetachedMessageStackRestore(targetTop: number) {
    const restoreKey = scrollStateKey;
    return detachedScrollRestoreController.schedule({
      host: {
        getCurrentKey: () => currentScrollStateKeyRef.current,
        getNode: () => messageStackRef.current,
        isTailFollowAttached: getTailFollowIntent,
        notifyPositionRestore: (node) => {
          notifyMessageStackScrollWrite(node, {
            scrollKind: "position_restore",
          });
        },
        publishReachablePosition: (top) => {
          setTailFollowIntent(false, { preserveDetachedRestore: true });
          paneScrollPositions[restoreKey] = preserveDetachedPaneScrollAnchor(
            paneScrollPositions[restoreKey],
            top,
          );
        },
        publishSavedTarget: (top) => {
          setTailFollowIntent(false, { preserveDetachedRestore: true });
          paneScrollPositions[restoreKey] = preserveDetachedPaneScrollAnchor(
            paneScrollPositions[restoreKey],
            top,
          );
        },
        publishUnloadedNewerHistory: () => {
          if (hasUnloadedNewerHistory) {
            setNewResponseIndicator(restoreKey, true);
          }
        },
      },
      key: restoreKey,
      targetTop,
    });
  }

  function restorePinnedMessageStackBeforePaint(node: HTMLElement) {
    // A tab switch reuses the pane's scroll container. Waiting until the first
    // animation frame would paint the newly active transcript at the previous
    // tab's offset and then visibly jump it to the bottom. Establish attached
    // geometry during the layout phase; the settled follow below remains
    // responsible for measurements that arrive after this commit.
    writeMessageStackScrollTopImmediately(
      node,
      Math.max(node.scrollHeight - node.clientHeight, 0),
    );
    notifyMessageStackScrollWrite(node, {
      scrollKind: "bottom_pin",
    });
    setTailFollowIntent(true);
    paneScrollPositions[scrollStateKey] = {
      top: Number.MAX_SAFE_INTEGER,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }

  useLayoutEffect(() => {
    let restoreCleanup: (() => void) | undefined;
    const restoreKey = scrollStateKey;
    if (!isSessionTabActive || paneViewMode !== "session") {
      return undefined;
    }
    const node = messageStackRef.current;
    if (!node) {
      return undefined;
    }

    if (paneScrollPositions[scrollStateKey]) {
      const saved = paneScrollPositions[scrollStateKey];
      if (saved.shouldStick || getTailFollowIntent()) {
        restorePinnedMessageStackBeforePaint(node);
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      } else {
        // A scroll controller from the previous tab must not survive into a
        // saved detached position. Virtualized geometry can also be
        // temporarily shorter than the saved target while its pages remount;
        // preserve that target and retry instead of converting it to a bottom
        // pin, which would permanently lose the user's reading position.
        cancelPaneProgrammaticBottomFollow();
        cancelSettledScrollToBottom();
        setTailFollowIntent(false, { preserveDetachedRestore: true });
        const restoredAnchor =
          saved.anchor !== undefined &&
          virtualizerHandleRef.current?.restoreViewportAnchor(saved.anchor) ===
            true;
        if (restoredAnchor) {
          paneScrollPositions[scrollStateKey] = {
            anchor: saved.anchor,
            shouldStick: false,
            top: node.scrollTop,
          };
        } else {
          restoreCleanup = scheduleDetachedMessageStackRestore(saved.top);
        }
      }
    } else if (defaultScrollToBottom) {
      restorePinnedMessageStackBeforePaint(node);
      restoreCleanup = scheduleSettledScrollToBottom("auto", {
        maxAttempts: 60,
        preferVirtualizedBoundary: true,
      });
    } else {
      writeMessageStackScrollTopImmediately(node, 0);
      notifyMessageStackScrollWrite(node);
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    }

    return () => {
      restoreCleanup?.();
      cancelDetachedMessageStackRestore(restoreKey);
    };
  }, [
    activeSession?.id,
    defaultScrollToBottom,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    const previousByKey = previousShowWaitingIndicatorByKeyRef.current;
    const wasShowing = previousByKey[scrollStateKey] ?? false;

    if (!showWaitingIndicator) {
      previousByKey[scrollStateKey] = false;
      return;
    }

    if (
      deferContentScrollEffects ||
      !activeSession ||
      activeSession.hasNewerHistory === true ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session"
    ) {
      return;
    }

    if (wasShowing) {
      return;
    }

    previousByKey[scrollStateKey] = true;

    if (!getTailFollowIntent()) {
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return;
    }

    repinAttachedLiveContentBeforePaint();
    return undefined;
  }, [
    activeSession?.id,
    activeSession?.hasNewerHistory,
    deferContentScrollEffects,
    isActive,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
    showWaitingIndicator,
  ]);

  useLayoutEffect(() => {
    if (!hasSessionFindQuery || !activeSessionSearchMatch) {
      return;
    }

    const node =
      sessionSearchItemRefsRef.current[activeSessionSearchMatch.itemKey];
    if (!node) {
      return;
    }

    setTailFollowIntent(false);
    node.scrollIntoView({
      block: "center",
      behavior: "auto",
    });

    const container = messageStackRef.current;
    if (!container) {
      return;
    }
    notifyMessageStackScrollWrite(container);

    paneScrollPositions[scrollStateKey] = {
      top: container.scrollTop,
      shouldStick: false,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }, [
    activeSessionSearchMatch,
    hasSessionFindQuery,
    paneScrollPositions,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    if (!activeSession || !isSessionTabActive) {
      return;
    }
    if (deferContentScrollEffects) {
      return;
    }

    const previousSignature = paneContentSignatures[scrollStateKey];
    const previousMessageContentSignature =
      paneMessageContentSignatures[scrollStateKey];
    if (previousSignature === visibleContentSignature) {
      return;
    }
    paneContentSignatures[scrollStateKey] = visibleContentSignature;
    paneMessageContentSignatures[scrollStateKey] =
      visibleMessageContentSignature;
    const latestTurnContentTransition =
      latestTurnContentTransitionByKeyRef.current[scrollStateKey];
    delete latestTurnContentTransitionByKeyRef.current[scrollStateKey];
    if (previousSignature === undefined) {
      // The layout-phase restore above owns first activation. Repeating it
      // here after paint creates a second scroll authority and can overwrite a
      // detached position restored during the same commit.
      return;
    }

    const contentTransitionKind =
      paneViewMode === "session"
        ? classifyTurnContentTransition({
            currentMessageContentSignature: visibleMessageContentSignature,
            previousMessageContentSignature,
            showWaitingIndicator,
            transition: latestTurnContentTransition,
          })
        : "live";
    if (
      hasSessionFindQuery &&
      contentTransitionKind !== "residentHistoryOnly"
    ) {
      setTailFollowIntent(false);
      if (paneViewMode === "session") {
        setNewResponseIndicator(
          scrollStateKey,
          true,
          contentTransitionKind === "pendingPromptsAdvanced" ||
            visibleLastMessageAuthor !== "assistant"
            ? "activity"
            : "response",
        );
      }
      return;
    }
    if (contentTransitionKind === "pendingPromptsAdvanced") {
      if (getTailFollowIntent()) {
        setNewResponseIndicator(scrollStateKey, false);
        repinAttachedLiveContentBeforePaint();
        return;
      }
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return;
    }
    if (contentTransitionKind === "residentHistoryOnly") {
      // Loading or trimming an older resident prefix is a consequence of the
      // user's scroll, not unseen live-tail activity. The broad conversation
      // signature must still advance, but it must not arm the popup or repin
      // the viewport that initiated the history reveal.
      return;
    }

    const onlyPendingPromptsChanged =
      paneViewMode === "session" &&
      showWaitingIndicator &&
      previousMessageContentSignature === visibleMessageContentSignature;
    if (onlyPendingPromptsChanged) {
      if (getTailFollowIntent()) {
        setNewResponseIndicator(scrollStateKey, false);
        repinAttachedLiveContentBeforePaint();
        return;
      }
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return;
    }

    const shouldScroll = getTailFollowIntent();
    if (!shouldScroll) {
      if (paneViewMode === "session") {
        setNewResponseIndicator(
          scrollStateKey,
          true,
          visibleLastMessageAuthor === "assistant" ? "response" : "activity",
        );
      }
      return;
    }

    setNewResponseIndicator(scrollStateKey, false);
    repinAttachedLiveContentBeforePaint();
    return;
  }, [
    activeSession?.id,
    deferContentScrollEffects,
    hasSessionFindQuery,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
    showWaitingIndicator,
    visibleContentSignature,
    visibleLastMessageAuthor,
    visibleMessageContentSignature,
  ]);

  useLayoutEffect(() => {
    if (
      !pendingScrollToBottomRequest ||
      !isActive ||
      paneViewMode !== "session" ||
      activeSession?.id !== pendingScrollToBottomRequest.sessionId
    ) {
      return;
    }

    const requestToken = pendingScrollToBottomRequest.token;
    const shouldReattach = pendingScrollToBottomRequest.reattach === true;
    if (shouldReattach && !getTailFollowIntent()) {
      // Resuming an agent turn is new activity, not explicit navigation. Keep
      // a reader's detached viewport fixed and let the indicator offer the
      // deliberate jump back to the live tail.
      setNewResponseIndicator(scrollStateKey, true, "activity");
      onScrollToBottomRequestHandled(requestToken);
      return undefined;
    }
    if (shouldReattach) {
      // The reader is already attached. Preserve that ownership before the
      // first frame so waiting/working state and early output cannot race ahead
      // of the continuous live-tail follow.
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
      setNewResponseIndicator(scrollStateKey, false);
    }
    const node = messageStackRef.current;
    if (node?.querySelector(".virtualized-message-list")) {
      scrollMessageStackToBoundary("bottom");
      onScrollToBottomRequestHandled(requestToken);
      return undefined;
    }

    if (shouldReattach) {
      beginPaneProgrammaticBottomFollow();
    }
    return scheduleSettledScrollToBottom("auto", {
      ...(shouldReattach
        ? {
            // Approval can append a large command/result card while the
            // velocity-bounded follow is converging. Match the live-flow
            // budget so the controller cannot stop visibly short of bottom.
            maxAttempts: 60,
            minAttempts: 4,
            scrollKind: "bottom_follow" as const,
          }
        : {}),
      onComplete: () => {
        onScrollToBottomRequestHandled(requestToken);
      },
    });
  }, [
    activeSession?.id,
    isActive,
    onScrollToBottomRequestHandled,
    paneViewMode,
    pendingScrollToBottomRequest,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    if (!isSending || paneViewMode !== "session") {
      return;
    }

    // Sending starts new activity, but it is not a navigation command. A
    // reader who moved away from the tail keeps the same viewport and gets an
    // indicator instead of being pulled away from the text they are reading.
    followLatestMessageForPromptSend();
    return undefined;
  }, [isSending, paneViewMode, scrollStateKey]);

  return {
    captureDetachedMessageStackPosition,
    handleConversationSearchItemMount,
    handleMessageStackFocusCapture,
    handleMessageStackScroll,
    handleMessageStackTouchStart,
    handleMessageStackUserScrollIntent,
    liveTailPinned,
    messageStackRef,
    newResponseIndicatorLabel,
    scrollMessageStackByPage,
    scrollMessageStackToBoundary,
    scrollSessionMessageStackByPageJump,
    showNewResponseIndicator,
    virtualizerHandleRef,
  };
}

function isNestedEditablePageKeyTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  if (
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLInputElement ||
    target instanceof HTMLSelectElement
  ) {
    return true;
  }

  return (
    target.isContentEditable ||
    target.contentEditable === "true" ||
    target.getAttribute("contenteditable") === "" ||
    target.getAttribute("contenteditable") === "true"
  );
}

function isInteractiveMessageStackKeyTarget(
  target: EventTarget | null,
  currentTarget: EventTarget | null,
) {
  if (
    !(target instanceof Element) ||
    !(currentTarget instanceof Element) ||
    target === currentTarget
  ) {
    return false;
  }

  const interactiveTarget = target.closest(
    'button, a[href], input, textarea, select, summary, [contenteditable]:not([contenteditable="false"]), [role="button"], [role="link"], [role="menuitem"], [role="option"], [role="tab"], [role="checkbox"], [role="radio"], [role="switch"]',
  );
  return Boolean(
    interactiveTarget && currentTarget.contains(interactiveTarget),
  );
}

function isUpwardMessageStackKeyIntent(event: KeyboardEvent) {
  if (
    event.defaultPrevented ||
    event.altKey ||
    event.ctrlKey ||
    event.metaKey
  ) {
    return false;
  }
  return (
    event.key === "ArrowUp" ||
    event.key === "PageUp" ||
    event.key === "Home" ||
    (event.key === " " && event.shiftKey)
  );
}

function isMessageStackKeyboardEventInActivePane(
  event: KeyboardEvent,
  messageStack: HTMLElement,
  paneRoot: HTMLElement | null,
) {
  if (
    isInteractiveMessageStackKeyTarget(
      event.target,
      paneRoot ?? messageStack,
    )
  ) {
    return false;
  }

  const path =
    typeof event.composedPath === "function" ? event.composedPath() : [];
  if (
    path.includes(messageStack) ||
    (paneRoot !== null && path.includes(paneRoot))
  ) {
    return true;
  }
  if (
    event.target === document ||
    event.target === document.body ||
    event.target === document.documentElement
  ) {
    return true;
  }
  return (
    event.target instanceof Node &&
    (messageStack.contains(event.target) ||
      (paneRoot?.contains(event.target) ?? false))
  );
}
