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
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type MutableRefObject,
  type RefObject,
  type TouchEvent as ReactTouchEvent,
  type UIEvent as ReactUIEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import {
  canNestedScrollableConsumeWheel,
  clamp,
  normalizeWheelDelta,
  pruneSessionFlags,
} from "./app-utils";
import {
  MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  notifyMessageStackScrollWrite,
  type MessageStackBottomRepinRequestDetail,
  type MessageStackScrollWriteKind,
} from "./message-stack-scroll-sync";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import {
  requestSessionHistoryStartPage,
  requestSessionHistoryTailPage,
} from "./session-history-demand";
import {
  resolveSettledScrollMinimumAttempts,
  syncMessageStackScrollPosition,
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

type PaneScrollPosition = {
  top: number;
  shouldStick: boolean;
};

type UseSessionPaneScrollStateParams = {
  activeSession: Session | null;
  activeSessionSearchMatch: SessionSearchMatch | null;
  defaultScrollToBottom: boolean;
  deferContentScrollEffects: boolean;
  forceSessionScrollToBottomRef: MutableRefObject<
    Record<string, true | undefined>
  >;
  hasSessionFindQuery: boolean;
  isActive: boolean;
  isSending: boolean;
  isSessionTabActive: boolean;
  onScrollToBottomRequestHandled: (token: number) => void;
  paneContentSignatures: Record<string, string>;
  paneId: string;
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
  sessions: Session[];
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
  forceSessionScrollToBottomRef,
  hasSessionFindQuery,
  isActive,
  isSending,
  isSessionTabActive,
  onScrollToBottomRequestHandled,
  paneContentSignatures,
  paneId,
  paneMessageContentSignatures,
  paneRootRef,
  paneScrollPositions,
  paneShouldStickToBottomRef,
  paneViewMode,
  pendingScrollToBottomRequest,
  scrollStateKey,
  sessions,
  showWaitingIndicator,
  visibleContentSignature,
  visibleLastMessageAuthor,
  visibleMessageContentSignature,
}: UseSessionPaneScrollStateParams) {
  const messageStackRef = useRef<HTMLElement | null>(null);
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
  const visibleContentSignatureByKeyRef = useRef<
    Record<string, string | undefined>
  >({});
  const messageStackBottomByKeyRef = useRef<
    Record<string, number | undefined>
  >({});
  const liveFlowActiveByKeyRef = useRef<Record<string, boolean | undefined>>(
    {},
  );
  const awaitingPostLivePromptMessageIdByKeyRef = useRef<
    Record<string, string | null | undefined>
  >({});
  const paneProgrammaticBottomFollowRef = useRef<{
    key: string | null;
    until: number;
  }>({ key: null, until: Number.NEGATIVE_INFINITY });
  const liveFlowActiveRef = useRef(false);
  const paneTailFollowUserEscapeByKeyRef = useRef<
    Record<string, true | undefined>
  >({});
  const currentScrollStateKeyRef = useRef(scrollStateKey);
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
  const [visitedSessionIds, setVisitedSessionIds] = useState<
    Record<string, true | undefined>
  >({});
  currentScrollStateKeyRef.current = scrollStateKey;
  liveFlowActiveRef.current = isSending || showWaitingIndicator;

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
  const savedScrollShouldStick = savedScrollPosition?.shouldStick === true;
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
    return paneShouldStickToBottomRef.current[paneId] ?? true;
  }

  function setTailFollowIntent(nextValue: boolean) {
    paneShouldStickToBottomRef.current[paneId] = nextValue;
    if (nextValue) {
      delete paneTailFollowUserEscapeByKeyRef.current[scrollStateKey];
    } else {
      paneTailFollowUserEscapeByKeyRef.current[scrollStateKey] = true;
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

  // LIVE TURN attachment is a scroll-controller state, never a CSS positioning
  // mode. While attached, this hook keeps the ordinary in-flow tail at the
  // viewport bottom. Explicit user navigation detaches it so the whole
  // transcript, including LIVE TURN, moves as one surface.
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

  function hasTailFollowUserEscape() {
    return Boolean(paneTailFollowUserEscapeByKeyRef.current[scrollStateKey]);
  }

  function markTailFollowUserEscape() {
    // This flag wins over temporary near-bottom geometry until the user reaches
    // the real bottom again or explicitly navigates back to the live tail.
    paneTailFollowUserEscapeByKeyRef.current[scrollStateKey] = true;
    cancelSettledScrollToBottom();
    setTailFollowIntent(false);
  }

  function keepPaneScrollPositionPinned(node: HTMLElement) {
    paneScrollPositions[scrollStateKey] = {
      top: node.scrollTop,
      shouldStick: true,
    };
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
      delete messageStackBottomByKeyRef.current[scrollStateKey];
    },
    [isActive, isSessionTabActive, paneViewMode, scrollStateKey],
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
      node.scrollTop = nextScrollTop;
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
    if (
      !node ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session"
    ) {
      return;
    }

    // Composer measurement may need an immediate, same-task correction before
    // paint. It requests that correction instead of writing scrollTop itself;
    // this handler remains the authority and rejects the request once explicit
    // tail-follow intent has been released by user navigation.
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
      // Streaming Markdown can replace a tall source block with a much shorter
      // rendered element in this layout effect. Correct that authoritative
      // shrink synchronously, even while the regular follow loop is active, so
      // the new tree and its viewport position reach the same paint together.
      if (detail?.beforePaint) {
        scrollToLatestMessage("auto", true, "bottom_follow", {
          snapBottomFollowBeforePaint: true,
        });
        return;
      }
      // A settled live follow already re-reads the bottom geometry on each
      // animation frame. A second synchronous writer here would duplicate the
      // correction and make composer/card growth appear as a snap.
      if (isSettledProgrammaticBottomFollowActive()) {
        return;
      }
      if (liveFlowActiveRef.current) {
        scheduleLiveTailFollow();
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
      const priorContentHeight = previousContentHeight;
      const nextContentHeight =
        conversationPage?.getBoundingClientRect().height ?? 0;
      const priorViewportHeight = previousViewportHeight;
      const nextViewportHeight = node.clientHeight;
      const contentChanged =
        Math.abs(nextContentHeight - previousContentHeight) > 0.5;
      const viewportChanged =
        Math.abs(nextViewportHeight - previousViewportHeight) > 0.5;
      previousContentHeight = nextContentHeight;
      previousViewportHeight = nextViewportHeight;
      if (!activePageChanged && !contentChanged && !viewportChanged) {
        return;
      }
      if (
        currentScrollStateKeyRef.current !== scrollStateKey ||
        !getTailFollowIntent()
      ) {
        return;
      }
      const bottomMovedUpBeforePaint =
        activePageChanged ||
        nextContentHeight < priorContentHeight - 0.5 ||
        nextViewportHeight > priorViewportHeight + 0.5;
      // New command cards often gain their measured height after live follow
      // starts. The settled loop targets that new bottom on its next frame; a
      // second synchronous writer here would duplicate the correction and
      // produce the visible up/down jump.
      if (isSettledProgrammaticBottomFollowActive()) {
        if (bottomMovedUpBeforePaint) {
          scrollToLatestMessage("auto", true, "bottom_follow", {
            snapBottomFollowBeforePaint: true,
          });
        }
        return;
      }
      if (liveFlowActiveRef.current) {
        scheduleLiveTailFollow();
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
    const messageStack = messageStackRef.current;
    const previousMessageStackBottom =
      messageStackBottomByKeyRef.current[scrollStateKey];
    const currentMessageStackBottom = messageStack
      ? Math.max(messageStack.scrollHeight - messageStack.clientHeight, 0)
      : undefined;
    if (currentMessageStackBottom !== undefined) {
      messageStackBottomByKeyRef.current[scrollStateKey] =
        currentMessageStackBottom;
    }
    const bottomMovedUpBeforePaint =
      previousMessageStackBottom !== undefined &&
      currentMessageStackBottom !== undefined &&
      currentMessageStackBottom < previousMessageStackBottom - 0.5;
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

    // LIVE TURN remains an ordinary in-flow tail. While auto-follow owns the
    // pane, one persistent animation-frame controller retargets the moving
    // bottom. Content commits never consume their own synthetic frame budget,
    // so a burst of SSE/React commits before paint cannot accelerate motion.
    // Turn status and the final assistant message can arrive in separate SSE
    // commits. The per-key latch survives a status-only idle commit, but stays
    // bound to that turn's prompt identity. A later prompt clears it without
    // following, while the matching final message still receives pre-paint
    // synchronization even when earlier command/progress output means this is
    // not first output.
    if (changedLiveContent) {
      if (bottomMovedUpBeforePaint) {
        // A browser clamp can make a live reparse require no numeric write.
        // Still notify the virtualizer synchronously so the final page and the
        // corrected viewport reach the same paint.
        scrollToLatestMessage("auto", true, "bottom_follow", {
          snapBottomFollowBeforePaint: true,
        });
      }
      scheduleLiveTailFollow();
      return;
    }

    scheduleLiveTailFollow();
  }, [
    activeSession?.messages,
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

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    const nextScrollTop = clamp(node.scrollTop + deltaY, 0, maxScrollTop);
    const isUpwardScroll = deltaY < -0.5;
    if (isUpwardScroll) {
      paneTailFollowUserEscapeByKeyRef.current[scrollStateKey] = true;
    }
    cancelPaneProgrammaticBottomFollow();
    node.scrollTop = nextScrollTop;
    notifyMessageStackScrollWrite(node, {
      scrollKind: options.scrollKind,
      scrollSource: "user",
    });
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
    if (isUpwardScroll) {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else if (shouldStick) {
      setTailFollowIntent(true);
      setNewResponseIndicator(scrollStateKey, false);
    } else {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
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
    return scheduleLiveTailFollow();
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
    if (boundary === "bottom") {
      const applyBottomBoundary = () => {
        cancelSettledScrollToBottom();
        cancelPaneProgrammaticBottomFollow();
        const node = messageStackRef.current;
        if (node) {
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

  const handleMessageStackWheelRef = useRef<
    ((event: WheelEvent) => void) | null
  >(null);
  handleMessageStackWheelRef.current = function handleMessageStackWheel(
    event: WheelEvent,
  ) {
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

    cancelPaneProgrammaticBottomFollow();
    markTailFollowUserEscape();
    event.preventDefault();
    scrollMessageStackByDelta(deltaY, {
      scrollKind: "incremental",
    });
  };

  useEffect(() => {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }
    const listener = (event: WheelEvent) => {
      handleMessageStackWheelRef.current?.(event);
    };
    node.addEventListener("wheel", listener, { passive: false });
    return () => {
      node.removeEventListener("wheel", listener);
    };
  }, []);

  const handleNestedTargetPageKeyRef = useRef<
    ((event: KeyboardEvent) => void) | null
  >(null);
  handleNestedTargetPageKeyRef.current = function handleNestedTargetPageKey(
    event: KeyboardEvent,
  ) {
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
  };

  useEffect(() => {
    if (!isActive || paneViewMode !== "session") {
      return;
    }

    const listener = (event: KeyboardEvent) => {
      handleNestedTargetPageKeyRef.current?.(event);
    };
    window.addEventListener("keydown", listener, true);
    return () => {
      window.removeEventListener("keydown", listener, true);
    };
  }, [isActive, paneViewMode]);

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
      if (
        options.scrollKind === "bottom_follow" &&
        (!getTailFollowIntent() || hasTailFollowUserEscape())
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
          : 4;
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
          !hasTailFollowUserEscape()
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

  function scheduleLiveTailFollow() {
    if (!getTailFollowIntent()) {
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return;
    }
    // Mark the entire live-flow transition before its first animation frame.
    // Composer measurements and ResizeObserver delivery can run in the gap
    // between the React commit and that frame; they must join this settled
    // pin instead of issuing an intervening correction. Each frame targets the
    // current bottom. Time-based interpolation keeps ordinary growth smooth;
    // authoritative pre-paint collapses use the separate synchronous repin
    // path so the visible tree can never outrun its viewport correction.
    beginPaneProgrammaticBottomFollow();
    if (isSettledProgrammaticBottomFollowActive()) {
      return;
    }
    scheduleSettledScrollToBottom("auto", {
      // Large command/result cards can require more than the old 400 ms window
      // now that each frame is deliberately velocity-bounded.
      maxAttempts: 60,
      minAttempts: 4,
      scrollKind: "bottom_follow",
    });
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
    cancelPaneProgrammaticBottomFollow();
    if (!hasTailFollowUserEscape() || getTailFollowIntent()) {
      markTailFollowUserEscape();
    }
  }

  function handleMessageStackScroll(event: ReactUIEvent<HTMLElement>) {
    const node = event.currentTarget;
    const previousScrollPosition = paneScrollPositions[scrollStateKey];
    const previousTop = previousScrollPosition?.top;
    const movedUpFromRecordedPosition =
      typeof previousTop === "number" &&
      previousTop < Number.MAX_SAFE_INTEGER / 2 &&
      node.scrollTop < previousTop - 1;
    const movedUpAfterUserEscape =
      hasTailFollowUserEscape() && movedUpFromRecordedPosition;
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
    if (hasUnloadedNewerHistory) {
      cancelPaneProgrammaticBottomFollow();
      cancelSettledScrollToBottom();
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: node.scrollTop,
        shouldStick: false,
      };
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
      if (targetTop - node.scrollTop <= 4) {
        cancelPaneProgrammaticBottomFollow();
      }
      return;
    }
    if (movedUpAfterUserEscape) {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else if (shouldStick) {
      // Reaching the physical bottom is the natural reattachment action.
      setTailFollowIntent(true);
      setNewResponseIndicator(scrollStateKey, false);
    } else if (
      hasTailFollowUserEscape() ||
      movedUpFromRecordedPosition ||
      !getTailFollowIntent()
    ) {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else {
      keepPaneScrollPositionPinned(node);
      setNewResponseIndicator(scrollStateKey, false);
    }
  }

  function restoreMessageStackScrollTop(targetTop: number) {
    const node = messageStackRef.current;
    if (!node) {
      return false;
    }
    // A saved detached position can be recorded during the brief gap between
    // DOM growth and the next bottom-follow write. Never replay it after the
    // pane has re-entered explicit tail-follow.
    if (getTailFollowIntent()) {
      return false;
    }

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (targetTop > maxScrollTop + 1) {
      return false;
    }

    const nextTop = clamp(targetTop, 0, maxScrollTop);
    node.scrollTop = nextTop;
    notifyMessageStackScrollWrite(node);
    paneScrollPositions[scrollStateKey] = {
      top: targetTop,
      shouldStick: false,
    };
    return true;
  }

  useLayoutEffect(() => {
    let restoreCleanup: (() => void) | undefined;
    const node = messageStackRef.current;
    if (!node) {
      return undefined;
    }

    const shouldForceBottomAfterWorkspaceRebuild =
      defaultScrollToBottom &&
      activeSession &&
      forceSessionScrollToBottomRef.current[activeSession.id];
    if (shouldForceBottomAfterWorkspaceRebuild) {
      delete forceSessionScrollToBottomRef.current[activeSession.id];
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
      node.scrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
      scrollMessageStackToBoundary("bottom");
      if (!node.querySelector(".virtualized-message-list")) {
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
        });
      }
    } else if (paneScrollPositions[scrollStateKey]) {
      const saved = paneScrollPositions[scrollStateKey];
      setTailFollowIntent(saved.shouldStick);
      if (saved.shouldStick) {
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      } else if (!restoreMessageStackScrollTop(saved.top)) {
        setTailFollowIntent(true);
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      }
    } else if (defaultScrollToBottom) {
      restoreCleanup = scheduleSettledScrollToBottom("auto", {
        maxAttempts: 60,
        preferVirtualizedBoundary: true,
      });
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
    } else {
      node.scrollTop = 0;
      notifyMessageStackScrollWrite(node);
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    }

    return () => {
      restoreCleanup?.();
    };
  }, [activeSession?.id, defaultScrollToBottom, scrollStateKey]);

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

    return scheduleLiveTailFollow();
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
    if (
      deferContentScrollEffects ||
      !activeSession ||
      !isSessionTabActive ||
      paneViewMode !== "session" ||
      visitedSessionIds[activeSession.id]
    ) {
      return;
    }

    if (savedScrollShouldStick) {
      return;
    }

    return scheduleSettledScrollToBottom("auto", {
      preferVirtualizedBoundary: true,
    });
  }, [
    activeSession?.id,
    deferContentScrollEffects,
    isSessionTabActive,
    paneViewMode,
    savedScrollShouldStick,
    scrollStateKey,
    visitedSessionIds,
  ]);

  useEffect(() => {
    if (!activeSession?.id) {
      return;
    }

    setVisitedSessionIds((current) =>
      current[activeSession.id]
        ? current
        : {
            ...current,
            [activeSession.id]: true,
          },
    );
  }, [activeSession?.id]);

  useEffect(() => {
    const availableSessionIds = new Set(sessions.map((session) => session.id));
    setVisitedSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
  }, [sessions]);

  useEffect(() => {
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
    if (previousSignature === undefined) {
      const saved = paneScrollPositions[scrollStateKey];
      if (saved && !saved.shouldStick) {
        if (!restoreMessageStackScrollTop(saved.top)) {
          setTailFollowIntent(true);
          return scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
        }
        return;
      }
      if (getTailFollowIntent() || saved?.shouldStick) {
        return scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      }
      return;
    }

    const onlyPendingPromptsChanged =
      paneViewMode === "session" &&
      showWaitingIndicator &&
      previousMessageContentSignature === visibleMessageContentSignature;
    if (onlyPendingPromptsChanged) {
      if (getTailFollowIntent()) {
        setNewResponseIndicator(scrollStateKey, false);
        return scheduleLiveTailFollow();
      }
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return;
    }

    if (hasSessionFindQuery) {
      setTailFollowIntent(false);
      if (paneViewMode === "session") {
        setNewResponseIndicator(
          scrollStateKey,
          true,
          visibleLastMessageAuthor === "assistant" ? "response" : "activity",
        );
      }
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
    return scheduleLiveTailFollow();
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

  useEffect(() => {
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

  useEffect(() => {
    if (!isSending || paneViewMode !== "session") {
      return;
    }

    // Sending starts new activity, but it is not a navigation command. A
    // reader who moved away from the tail keeps the same viewport and gets an
    // indicator instead of being pulled away from the text they are reading.
    const frameId = window.requestAnimationFrame(() => {
      followLatestMessageForPromptSend();
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [isSending, paneViewMode, scrollStateKey]);

  return {
    handleConversationSearchItemMount,
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
