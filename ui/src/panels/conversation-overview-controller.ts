// AgentSessionPanel wiring for the server-computed conversation overview.
//
// The rail is intentionally independent of transcript residency and
// virtualizer layout. This controller owns only:
//   - one bounded overview fetch keyed by server freshness metadata,
//   - conversion of the resident scroll fraction into global positions, and
//   - direct around-position history navigation for off-window targets.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";

import {
  fetchSessionOverview,
  type SessionOverviewResponse,
} from "../api";
import {
  notifyMessageStackScrollWrite,
  writeMessageStackScrollTopImmediately,
} from "../message-stack-scroll-sync";
import { requestSessionHistoryAroundPage } from "../session-history-demand";
import { CONVERSATION_OVERVIEW_MIN_MESSAGES } from "./ConversationOverviewRail";
import type { ConversationOverviewViewport } from "./conversation-overview-map";
import type { VirtualizedConversationMessageListHandle } from "./VirtualizedConversationMessageList";

const EMPTY_OVERVIEW_VIEWPORT: ConversationOverviewViewport = {
  startPosition: 0,
  endPosition: 1,
};
const CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX = 24;
// Streaming turns can advance message count and mutation metadata several
// times in one paint burst. Keep the first rail load immediate, then collapse
// those follow-up invalidations so one active pane never fans out duplicate
// overview requests while the user is typing or switching tabs.
const CONVERSATION_OVERVIEW_REFRESH_DEBOUNCE_MS = 120;
const CONVERSATION_OVERVIEW_REFRESH_MAX_WAIT_MS = 500;

type ConversationOverviewRailFrame = {
  heightPx: number;
  portalTarget: HTMLElement;
  rightPx: number;
  topPx: number;
};

type PendingOverviewLayoutScrollRestore = {
  scrollStateKey: string;
  sessionId: string;
};

export function useConversationOverviewController({
  isActive,
  messageCount,
  messageStartIndex,
  renderedMessageCount,
  scrollContainerRef,
  scrollStateKey,
  sessionId,
  sessionMutationStamp,
  tailFollowIntent,
}: {
  isActive: boolean;
  messageCount: number;
  messageStartIndex: number;
  renderedMessageCount: number;
  scrollContainerRef: RefObject<HTMLElement | null>;
  scrollStateKey: string;
  sessionId: string;
  sessionMutationStamp: number;
  tailFollowIntent: boolean;
}) {
  const isOverviewEligible =
    messageCount >= CONVERSATION_OVERVIEW_MIN_MESSAGES;
  const virtualizerHandleRef =
    useRef<VirtualizedConversationMessageListHandle | null>(null);
  const navigationFrameIdsRef = useRef<Set<number>>(new Set());
  const overviewRequestSessionIdRef = useRef<string | null>(null);
  const overviewRefreshBurstStartedAtRef = useRef<number | null>(null);
  const overviewRefreshTimerRef = useRef<number | null>(null);
  const overviewRequestIdRef = useRef(0);
  const readyOverviewSessionIdRef = useRef<string | null>(null);
  const pendingLayoutScrollRestoreRef =
    useRef<PendingOverviewLayoutScrollRestore | null>(null);
  const scrollStateKeyRef = useRef(scrollStateKey);
  const sessionIdRef = useRef(sessionId);
  const tailFollowIntentRef = useRef(tailFollowIntent);
  const messageStartIndexRef = useRef(messageStartIndex);
  const renderedMessageCountRef = useRef(renderedMessageCount);
  const [overview, setOverview] = useState<SessionOverviewResponse | null>(null);
  const [viewport, setViewport] = useState<ConversationOverviewViewport>(
    EMPTY_OVERVIEW_VIEWPORT,
  );
  const [railFrame, setRailFrame] =
    useState<ConversationOverviewRailFrame | null>(null);
  const hasReadyOverview = overview?.sessionId === sessionId;
  // Eligibility starts/refreshes the server request. Readiness alone enables
  // the wrapper and CSS that replace the native scrollbar with the overview
  // rail, so pending, failed, or stale-session data never removes the only
  // working scroll affordance.
  const shouldRequestOverview = isActive && isOverviewEligible;
  const shouldRender = shouldRequestOverview && hasReadyOverview;
  readyOverviewSessionIdRef.current = overview?.sessionId ?? null;
  scrollStateKeyRef.current = scrollStateKey;
  sessionIdRef.current = sessionId;
  tailFollowIntentRef.current = tailFollowIntent;
  messageStartIndexRef.current = messageStartIndex;
  renderedMessageCountRef.current = renderedMessageCount;

  const cancelOverviewRefreshTimer = useCallback(() => {
    if (overviewRefreshTimerRef.current !== null) {
      window.clearTimeout(overviewRefreshTimerRef.current);
      overviewRefreshTimerRef.current = null;
    }
  }, []);

  const requestOverview = useCallback((expectedSessionId: string) => {
    const requestId = overviewRequestIdRef.current + 1;
    overviewRequestIdRef.current = requestId;
    overviewRefreshBurstStartedAtRef.current = null;
    overviewRefreshTimerRef.current = null;
    void fetchSessionOverview(expectedSessionId).then(
      (response) => {
        if (
          overviewRequestIdRef.current !== requestId ||
          sessionIdRef.current !== expectedSessionId ||
          response.sessionId !== expectedSessionId
        ) {
          return;
        }
        if (readyOverviewSessionIdRef.current !== expectedSessionId) {
          const scrollNode = scrollContainerRef.current;
          if (scrollNode) {
            pendingLayoutScrollRestoreRef.current = {
              scrollStateKey: scrollStateKeyRef.current,
              sessionId: expectedSessionId,
            };
          }
        }
        setOverview(response);
      },
      () => {
        if (
          overviewRequestIdRef.current === requestId &&
          sessionIdRef.current === expectedSessionId
        ) {
          setOverview(null);
        }
      },
    );
  }, [scrollContainerRef]);

  const refreshViewport = useCallback(() => {
    const scrollNode = scrollContainerRef.current;
    const nextViewport = conversationOverviewViewportFromResidentWindow({
      messageCount,
      messageStartIndex,
      renderedMessageCount,
      scrollNode,
    });
    setViewport((current) =>
      current.startPosition === nextViewport.startPosition &&
      current.endPosition === nextViewport.endPosition
        ? current
        : nextViewport,
    );
  }, [
    messageCount,
    messageStartIndex,
    renderedMessageCount,
    scrollContainerRef,
  ]);

  const refreshRailFrame = useCallback(() => {
    const scrollNode = scrollContainerRef.current;
    const portalTarget = scrollNode?.closest(".workspace-pane");
    if (
      scrollNode &&
      scrollNode.clientHeight > 0 &&
      portalTarget instanceof HTMLElement
    ) {
      const bounds = scrollNode.getBoundingClientRect();
      const portalBounds = portalTarget.getBoundingClientRect();
      const insetPx = CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX / 2;
      const nextRailFrame = {
        heightPx: Math.max(
          0,
          scrollNode.clientHeight - CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX,
        ),
        portalTarget,
        rightPx: Math.max(0, portalBounds.right - bounds.right + insetPx),
        topPx: Math.max(0, bounds.top - portalBounds.top + insetPx),
      };
      setRailFrame((current) =>
        current?.heightPx === nextRailFrame.heightPx &&
        current.portalTarget === nextRailFrame.portalTarget &&
        current.rightPx === nextRailFrame.rightPx &&
        current.topPx === nextRailFrame.topPx
          ? current
          : nextRailFrame,
      );
      return;
    }
    setRailFrame(null);
  }, [scrollContainerRef]);

  useEffect(() => {
    if (!shouldRequestOverview) {
      cancelOverviewRefreshTimer();
      overviewRequestIdRef.current += 1;
      overviewRefreshBurstStartedAtRef.current = null;
      overviewRequestSessionIdRef.current = null;
      setOverview(null);
      return;
    }
    const expectedSessionId = sessionId;
    const isInitialRequestForSession =
      overviewRequestSessionIdRef.current !== expectedSessionId;
    overviewRequestSessionIdRef.current = expectedSessionId;
    setOverview((current) =>
      current?.sessionId === expectedSessionId ? current : null,
    );
    cancelOverviewRefreshTimer();
    if (isInitialRequestForSession) {
      requestOverview(expectedSessionId);
      return;
    }

    const now = performance.now();
    const burstStartedAt =
      overviewRefreshBurstStartedAtRef.current ?? now;
    overviewRefreshBurstStartedAtRef.current = burstStartedAt;
    const remainingMaxWait = Math.max(
      0,
      CONVERSATION_OVERVIEW_REFRESH_MAX_WAIT_MS - (now - burstStartedAt),
    );
    if (remainingMaxWait <= 0) {
      requestOverview(expectedSessionId);
      return;
    }
    overviewRefreshTimerRef.current = window.setTimeout(
      () => requestOverview(expectedSessionId),
      Math.min(CONVERSATION_OVERVIEW_REFRESH_DEBOUNCE_MS, remainingMaxWait),
    );
  }, [
    cancelOverviewRefreshTimer,
    messageCount,
    requestOverview,
    sessionId,
    sessionMutationStamp,
    shouldRequestOverview,
  ]);

  useEffect(
    () => () => {
      cancelOverviewRefreshTimer();
      overviewRequestIdRef.current += 1;
    },
    [cancelOverviewRefreshTimer],
  );

  useLayoutEffect(() => {
    if (!shouldRequestOverview) {
      setViewport(EMPTY_OVERVIEW_VIEWPORT);
      setRailFrame(null);
      return undefined;
    }
    const scrollNode = scrollContainerRef.current;
    refreshViewport();
    refreshRailFrame();
    scrollNode?.addEventListener("scroll", refreshViewport, { passive: true });
    window.addEventListener("resize", refreshRailFrame, { passive: true });
    const resizeObserver =
      scrollNode && typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => {
            refreshViewport();
            refreshRailFrame();
          })
        : null;
    if (scrollNode) {
      resizeObserver?.observe(scrollNode);
    }
    return () => {
      scrollNode?.removeEventListener("scroll", refreshViewport);
      window.removeEventListener("resize", refreshRailFrame);
      resizeObserver?.disconnect();
    };
  }, [
    refreshRailFrame,
    refreshViewport,
    scrollContainerRef,
    sessionId,
    shouldRequestOverview,
  ]);

  useLayoutEffect(() => {
    const pendingRestore = pendingLayoutScrollRestoreRef.current;
    if (!hasReadyOverview || !pendingRestore) {
      return;
    }
    pendingLayoutScrollRestoreRef.current = null;
    if (
      pendingRestore.sessionId !== sessionId ||
      pendingRestore.scrollStateKey !== scrollStateKey
    ) {
      return;
    }
    // The overview response can resolve after a tab switch or pane migration.
    // Its pre-render DOM geometry is not scroll authority: the outgoing view
    // may still be at the bottom while the incoming session has a detached
    // saved position. Re-read the current session-scoped intent at write time.
    // Detached restoration is owned by SessionPaneView and must not be replayed
    // (or adopted) here after the wrapper transition.
    if (!tailFollowIntentRef.current) {
      return;
    }
    const scrollNode = scrollContainerRef.current;
    if (!scrollNode) {
      return;
    }
    const nextScrollTop = Math.max(
      0,
      scrollNode.scrollHeight - scrollNode.clientHeight,
    );
    writeMessageStackScrollTopImmediately(scrollNode, nextScrollTop);
    notifyMessageStackScrollWrite(scrollNode, {
      scrollKind: "bottom_pin",
    });
  }, [hasReadyOverview, scrollContainerRef, scrollStateKey, sessionId]);

  const cancelNavigationFrames = useCallback(() => {
    for (const frameId of navigationFrameIdsRef.current) {
      window.cancelAnimationFrame(frameId);
    }
    navigationFrameIdsRef.current.clear();
  }, []);

  useEffect(() => cancelNavigationFrames, [
    cancelNavigationFrames,
    sessionId,
  ]);

  const scheduleNavigationFrame = useCallback((callback: () => void) => {
    const expectedSessionId = sessionIdRef.current;
    let frameId = 0;
    let firedSynchronously = false;
    frameId = window.requestAnimationFrame(() => {
      firedSynchronously = true;
      navigationFrameIdsRef.current.delete(frameId);
      if (sessionIdRef.current === expectedSessionId) {
        callback();
      }
    });
    if (!firedSynchronously) {
      navigationFrameIdsRef.current.add(frameId);
    }
  }, []);

  const jumpToResidentPosition = useCallback((position: number) => {
    const localIndex = position - messageStartIndexRef.current;
    if (
      localIndex < 0 ||
      localIndex >= renderedMessageCountRef.current
    ) {
      return false;
    }
    return (
      virtualizerHandleRef.current?.jumpToMessageIndex(localIndex, {
        align: "center",
        flush: true,
      }) ?? false
    );
  }, []);

  const navigate = useCallback(
    (position: number) => {
      const boundedPosition = Math.min(
        Math.max(0, Math.floor(position)),
        Math.max(0, messageCount - 1),
      );
      if (jumpToResidentPosition(boundedPosition)) {
        scheduleNavigationFrame(refreshViewport);
        return;
      }
      const expectedSessionId = sessionIdRef.current;
      void requestSessionHistoryAroundPage(
        expectedSessionId,
        boundedPosition,
      ).then((applied) => {
        if (!applied || sessionIdRef.current !== expectedSessionId) {
          return;
        }
        const retryJump = () => {
          if (jumpToResidentPosition(boundedPosition)) {
            refreshViewport();
            return;
          }
          scheduleNavigationFrame(() => {
            jumpToResidentPosition(boundedPosition);
            refreshViewport();
          });
        };
        scheduleNavigationFrame(retryJump);
      });
    },
    [
      jumpToResidentPosition,
      messageCount,
      refreshViewport,
      scheduleNavigationFrame,
    ],
  );

  return {
    isRailReady: hasReadyOverview,
    navigate,
    overview,
    railHeightPx: railFrame?.heightPx ?? null,
    railPortalTarget: railFrame?.portalTarget ?? null,
    railRightPx: railFrame?.rightPx ?? null,
    railTopPx: railFrame?.topPx ?? null,
    shouldRenderRail: shouldRender,
    shouldRender,
    viewport,
    virtualizerHandleRef,
  };
}

export function conversationOverviewViewportFromResidentWindow({
  messageCount,
  messageStartIndex,
  renderedMessageCount,
  scrollNode,
}: {
  messageCount: number;
  messageStartIndex: number;
  renderedMessageCount: number;
  scrollNode: Pick<
    HTMLElement,
    "clientHeight" | "scrollHeight" | "scrollTop"
  > | null;
}): ConversationOverviewViewport {
  if (messageCount <= 0 || renderedMessageCount <= 0) {
    return EMPTY_OVERVIEW_VIEWPORT;
  }
  const boundedStart = Math.min(
    Math.max(0, messageStartIndex),
    Math.max(0, messageCount - 1),
  );
  const boundedResidentCount = Math.min(
    renderedMessageCount,
    messageCount - boundedStart,
  );
  if (!scrollNode || scrollNode.scrollHeight <= 0) {
    return {
      startPosition: boundedStart,
      endPosition: Math.min(
        messageCount,
        boundedStart + boundedResidentCount,
      ),
    };
  }

  const visibleFraction = Math.min(
    1,
    Math.max(0, scrollNode.clientHeight / scrollNode.scrollHeight),
  );
  const visibleMessageCount = Math.max(
    1,
    Math.ceil(boundedResidentCount * visibleFraction),
  );
  const maxScrollTop = Math.max(
    0,
    scrollNode.scrollHeight - scrollNode.clientHeight,
  );
  const scrollFraction =
    maxScrollTop > 0
      ? Math.min(Math.max(scrollNode.scrollTop / maxScrollTop, 0), 1)
      : 0;
  const localStart = Math.round(
    scrollFraction *
      Math.max(0, boundedResidentCount - visibleMessageCount),
  );
  const startPosition = boundedStart + localStart;
  return {
    startPosition,
    endPosition: Math.min(
      messageCount,
      startPosition + visibleMessageCount,
    ),
  };
}
