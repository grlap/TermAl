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
  useRef,
  useState,
  type RefObject,
} from "react";

import {
  fetchSessionOverview,
  type SessionOverviewResponse,
} from "../api";
import { requestSessionHistoryAroundPage } from "../session-history-demand";
import { CONVERSATION_OVERVIEW_MIN_MESSAGES } from "./ConversationOverviewRail";
import type { ConversationOverviewViewport } from "./conversation-overview-map";
import type { VirtualizedConversationMessageListHandle } from "./VirtualizedConversationMessageList";

const EMPTY_OVERVIEW_VIEWPORT: ConversationOverviewViewport = {
  startPosition: 0,
  endPosition: 1,
};
const CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX = 24;

export function useConversationOverviewController({
  isActive,
  messageCount,
  messageStartIndex,
  renderedMessageCount,
  scrollContainerRef,
  sessionId,
  sessionMutationStamp,
}: {
  isActive: boolean;
  messageCount: number;
  messageStartIndex: number;
  renderedMessageCount: number;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
  sessionMutationStamp: number;
}) {
  const shouldRender = messageCount >= CONVERSATION_OVERVIEW_MIN_MESSAGES;
  const virtualizerHandleRef =
    useRef<VirtualizedConversationMessageListHandle | null>(null);
  const navigationFrameIdsRef = useRef<Set<number>>(new Set());
  const sessionIdRef = useRef(sessionId);
  const messageStartIndexRef = useRef(messageStartIndex);
  const renderedMessageCountRef = useRef(renderedMessageCount);
  const [overview, setOverview] = useState<SessionOverviewResponse | null>(null);
  const [viewport, setViewport] = useState<ConversationOverviewViewport>(
    EMPTY_OVERVIEW_VIEWPORT,
  );
  const [railHeightPx, setRailHeightPx] = useState<number | null>(null);
  sessionIdRef.current = sessionId;
  messageStartIndexRef.current = messageStartIndex;
  renderedMessageCountRef.current = renderedMessageCount;

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
    if (scrollNode && scrollNode.clientHeight > 0) {
      const nextRailHeightPx = Math.max(
        0,
        scrollNode.clientHeight - CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX,
      );
      setRailHeightPx((current) =>
        current === nextRailHeightPx ? current : nextRailHeightPx,
      );
    }
  }, [
    messageCount,
    messageStartIndex,
    renderedMessageCount,
    scrollContainerRef,
  ]);

  useEffect(() => {
    if (!isActive || !shouldRender) {
      setOverview(null);
      return undefined;
    }
    let canceled = false;
    const expectedSessionId = sessionId;
    setOverview((current) =>
      current?.sessionId === expectedSessionId ? current : null,
    );
    void fetchSessionOverview(sessionId).then(
      (response) => {
        if (
          canceled ||
          sessionIdRef.current !== expectedSessionId ||
          response.sessionId !== expectedSessionId
        ) {
          return;
        }
        setOverview(response);
      },
      () => {
        if (!canceled && sessionIdRef.current === expectedSessionId) {
          setOverview(null);
        }
      },
    );
    return () => {
      canceled = true;
    };
  }, [
    isActive,
    messageCount,
    sessionId,
    sessionMutationStamp,
    shouldRender,
  ]);

  useEffect(() => {
    if (!isActive || !shouldRender) {
      setViewport(EMPTY_OVERVIEW_VIEWPORT);
      setRailHeightPx(null);
      return undefined;
    }
    const scrollNode = scrollContainerRef.current;
    refreshViewport();
    scrollNode?.addEventListener("scroll", refreshViewport, { passive: true });
    const resizeObserver =
      scrollNode && typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(refreshViewport)
        : null;
    if (scrollNode) {
      resizeObserver?.observe(scrollNode);
    }
    return () => {
      scrollNode?.removeEventListener("scroll", refreshViewport);
      resizeObserver?.disconnect();
    };
  }, [
    isActive,
    refreshViewport,
    scrollContainerRef,
    sessionId,
    shouldRender,
  ]);

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
    isRailReady: overview !== null,
    navigate,
    overview,
    railHeightPx,
    shouldRenderRail: isActive && shouldRender,
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
