// Owns the AgentSessionPanel-side wiring for the conversation overview rail:
// tail-item synthesis, layout/viewport snapshot subscriptions, and overview
// navigation. Does not own projection math (see conversation-overview-map.ts)
// or rail rendering (see ConversationOverviewRail.tsx). Split out of
// AgentSessionPanel.tsx during the round-28 shrinkage.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";

import { CONVERSATION_OVERVIEW_MIN_MESSAGES } from "./ConversationOverviewRail";
import type {
  ConversationOverviewItem,
  ConversationOverviewTailItemInput,
} from "./conversation-overview-map";
import type {
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationMessageListHandle,
  VirtualizedConversationViewportSnapshot,
} from "./VirtualizedConversationMessageList";
import type { Session } from "../types";

const CONVERSATION_OVERVIEW_LIVE_TURN_HEIGHT_PX = 108;
const CONVERSATION_OVERVIEW_FALLBACK_MAX_HEIGHT_PX = 520;
const CONVERSATION_OVERVIEW_MIN_HEIGHT_PX = 160;
const CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX = 24;

type ConversationOverviewRailBuildTask = {
  id: number;
  run: () => void;
};

let nextConversationOverviewRailBuildTaskId = 1;
let conversationOverviewRailBuildFrameId: number | null = null;
const pendingConversationOverviewRailBuildTasks: ConversationOverviewRailBuildTask[] =
  [];

function scheduleConversationOverviewRailBuild(run: () => void) {
  const task: ConversationOverviewRailBuildTask = {
    id: nextConversationOverviewRailBuildTaskId,
    run,
  };
  nextConversationOverviewRailBuildTaskId += 1;
  pendingConversationOverviewRailBuildTasks.push(task);
  scheduleConversationOverviewRailBuildDrain();
  return () => {
    const taskIndex = pendingConversationOverviewRailBuildTasks.findIndex(
      (candidate) => candidate.id === task.id,
    );
    if (taskIndex !== -1) {
      pendingConversationOverviewRailBuildTasks.splice(taskIndex, 1);
    }
    if (
      pendingConversationOverviewRailBuildTasks.length === 0 &&
      conversationOverviewRailBuildFrameId !== null
    ) {
      window.cancelAnimationFrame(conversationOverviewRailBuildFrameId);
      conversationOverviewRailBuildFrameId = null;
    }
  };
}

function scheduleConversationOverviewRailBuildDrain() {
  if (conversationOverviewRailBuildFrameId !== null) {
    return;
  }
  let ranSynchronously = false;
  const frameId = window.requestAnimationFrame(() => {
    if (conversationOverviewRailBuildFrameId === null) {
      ranSynchronously = true;
    } else {
      conversationOverviewRailBuildFrameId = null;
    }
    const task = pendingConversationOverviewRailBuildTasks.shift();
    if (task) {
      task.run();
    }
    if (pendingConversationOverviewRailBuildTasks.length > 0) {
      scheduleConversationOverviewRailBuildDrain();
    }
  });
  conversationOverviewRailBuildFrameId = ranSynchronously ? null : frameId;
}

export function useConversationOverviewController({
  agent,
  isActive,
  messageCount,
  scrollContainerRef,
  sessionId,
  showWaitingIndicator,
  waitingIndicatorPrompt,
}: {
  agent: Session["agent"];
  isActive: boolean;
  messageCount: number;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
}) {
  const tailItems = useMemo(
    () =>
      buildConversationOverviewTailItems({
        agent,
        sessionId,
        showWaitingIndicator,
        waitingIndicatorPrompt,
      }),
    [agent, sessionId, showWaitingIndicator, waitingIndicatorPrompt],
  );
  const shouldRender = messageCount >= CONVERSATION_OVERVIEW_MIN_MESSAGES;
  const virtualizerHandleRef =
    useRef<VirtualizedConversationMessageListHandle | null>(null);
  const [isRailReady, setIsRailReady] = useState(false);
  const [layoutSnapshot, setLayoutSnapshot] =
    useState<VirtualizedConversationLayoutSnapshot | null>(null);
  const [viewportSnapshot, setViewportSnapshot] =
    useState<VirtualizedConversationViewportSnapshot | null>(null);
  const overviewSessionIdRef = useRef(sessionId);
  const navigationFrameIdsRef = useRef<Set<number>>(new Set());
  overviewSessionIdRef.current = sessionId;

  const refreshLayoutSnapshot = useCallback(() => {
    const nextSnapshot = virtualizerHandleRef.current?.getLayoutSnapshot() ?? null;
    setLayoutSnapshot((previousSnapshot) =>
      areConversationOverviewLayoutSnapshotsEqual(previousSnapshot, nextSnapshot)
        ? previousSnapshot
        : nextSnapshot,
    );
    const nextViewportSnapshot =
      nextSnapshot === null
        ? null
        : extractConversationOverviewViewportSnapshot(nextSnapshot);
    setViewportSnapshot((previousSnapshot) =>
      areConversationOverviewViewportSnapshotsEqual(
        previousSnapshot,
        nextViewportSnapshot,
      )
        ? previousSnapshot
        : nextViewportSnapshot,
    );
  }, []);

  const refreshViewportSnapshot = useCallback(() => {
    const nextSnapshot = virtualizerHandleRef.current?.getViewportSnapshot() ?? null;
    setViewportSnapshot((previousSnapshot) =>
      areConversationOverviewViewportSnapshotsEqual(previousSnapshot, nextSnapshot)
        ? previousSnapshot
        : nextSnapshot,
    );
  }, []);

  const cancelNavigationFrames = useCallback(() => {
    navigationFrameIdsRef.current.forEach((frameId) => {
      window.cancelAnimationFrame(frameId);
    });
    navigationFrameIdsRef.current.clear();
  }, []);

  const scheduleNavigationFrame = useCallback((callback: () => void) => {
    const expectedSessionId = overviewSessionIdRef.current;
    let frameId: number | null = null;
    let ranSynchronously = false;
    const runFrame = () => {
      if (frameId === null) {
        ranSynchronously = true;
      } else {
        navigationFrameIdsRef.current.delete(frameId);
      }
      if (overviewSessionIdRef.current !== expectedSessionId) {
        return;
      }
      callback();
    };
    frameId = window.requestAnimationFrame(runFrame);
    if (!ranSynchronously) {
      navigationFrameIdsRef.current.add(frameId);
    }
  }, []);

  const navigate = useCallback(
    (item: ConversationOverviewItem) => {
      const handle = virtualizerHandleRef.current;
      if (!handle) {
        return;
      }
      if (item.kind === "live_turn") {
        const jumpedToTail =
          messageCount > 0
            ? handle.jumpToMessageIndex(messageCount - 1, { align: "end" })
            : false;
        const scrollTailIntoView = () => {
          const node = scrollContainerRef.current;
          if (!node) {
            return;
          }
          node.scrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
          refreshViewportSnapshot();
        };
        if (jumpedToTail) {
          scheduleNavigationFrame(scrollTailIntoView);
        } else {
          scrollTailIntoView();
        }
        return;
      }
      const jumped =
        handle.jumpToMessageId(item.messageId, { align: "center" }) ||
        handle.jumpToMessageIndex(item.messageIndex, { align: "center" });
      if (jumped) {
        scheduleNavigationFrame(refreshViewportSnapshot);
      }
    },
    [
      messageCount,
      refreshViewportSnapshot,
      scheduleNavigationFrame,
      scrollContainerRef,
    ],
  );

  useEffect(() => cancelNavigationFrames, [
    cancelNavigationFrames,
    sessionId,
    shouldRender,
  ]);

  useEffect(() => {
    if (!isActive || !shouldRender) {
      setIsRailReady(false);
      setLayoutSnapshot((previousSnapshot) =>
        previousSnapshot === null ? previousSnapshot : null,
      );
      setViewportSnapshot((previousSnapshot) =>
        previousSnapshot === null ? previousSnapshot : null,
      );
      return;
    }

    setIsRailReady(false);
    const expectedSessionId = sessionId;
    let firstFrameId: number | null = null;
    let secondFrameId: number | null = null;
    let cancelQueuedActivation: (() => void) | null = null;
    let cancelled = false;
    const activate = () => {
      if (cancelled || overviewSessionIdRef.current !== expectedSessionId) {
        return;
      }
      refreshLayoutSnapshot();
      setIsRailReady(true);
    };
    firstFrameId = window.requestAnimationFrame(() => {
      firstFrameId = null;
      secondFrameId = window.requestAnimationFrame(() => {
        secondFrameId = null;
        cancelQueuedActivation = scheduleConversationOverviewRailBuild(activate);
      });
    });

    return () => {
      cancelled = true;
      cancelQueuedActivation?.();
      if (firstFrameId !== null) {
        window.cancelAnimationFrame(firstFrameId);
      }
      if (secondFrameId !== null) {
        window.cancelAnimationFrame(secondFrameId);
      }
    };
  }, [isActive, refreshLayoutSnapshot, sessionId, shouldRender]);

  useEffect(() => {
    if (!isActive || !shouldRender || !isRailReady) {
      return undefined;
    }
    const scrollNode = scrollContainerRef.current;
    let layoutFrameId: number | null = null;
    let viewportFrameId: number | null = null;
    const scheduleLayoutRefresh = () => {
      if (layoutFrameId !== null) {
        return;
      }
      let ranSynchronously = false;
      const frameId = window.requestAnimationFrame(() => {
        if (layoutFrameId === null) {
          ranSynchronously = true;
        }
        layoutFrameId = null;
        refreshLayoutSnapshot();
        refreshViewportSnapshot();
      });
      layoutFrameId = ranSynchronously ? null : frameId;
    };
    const scheduleViewportRefresh = () => {
      if (viewportFrameId !== null) {
        return;
      }
      let ranSynchronously = false;
      const frameId = window.requestAnimationFrame(() => {
        if (viewportFrameId === null) {
          ranSynchronously = true;
        }
        viewportFrameId = null;
        refreshViewportSnapshot();
      });
      viewportFrameId = ranSynchronously ? null : frameId;
    };

    scheduleLayoutRefresh();
    scrollNode?.addEventListener("scroll", scheduleViewportRefresh, {
      passive: true,
    });
    window.addEventListener("resize", scheduleLayoutRefresh);
    return () => {
      if (layoutFrameId !== null) {
        window.cancelAnimationFrame(layoutFrameId);
      }
      if (viewportFrameId !== null) {
        window.cancelAnimationFrame(viewportFrameId);
      }
      scrollNode?.removeEventListener("scroll", scheduleViewportRefresh);
      window.removeEventListener("resize", scheduleLayoutRefresh);
    };
  }, [
    isActive,
    isRailReady,
    refreshLayoutSnapshot,
    refreshViewportSnapshot,
    scrollContainerRef,
    shouldRender,
  ]);

  const maxHeightPx =
    viewportSnapshot?.viewportHeightPx !== undefined
      ? Math.max(
          CONVERSATION_OVERVIEW_MIN_HEIGHT_PX,
          viewportSnapshot.viewportHeightPx -
            CONVERSATION_OVERVIEW_VIEWPORT_PADDING_PX,
        )
      : CONVERSATION_OVERVIEW_FALLBACK_MAX_HEIGHT_PX;

  return {
    isRailReady,
    layoutSnapshot,
    maxHeightPx,
    navigate,
    shouldRenderRail: shouldRender && isRailReady && layoutSnapshot !== null,
    shouldRender,
    tailItems,
    viewportSnapshot,
    virtualizerHandleRef,
  };
}

/** @internal Exported for focused regression tests; not a cross-panel API. */
export function buildConversationOverviewTailItems({
  agent,
  sessionId,
  showWaitingIndicator,
  waitingIndicatorPrompt,
}: {
  agent: Session["agent"];
  sessionId: string;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
}): readonly ConversationOverviewTailItemInput[] {
  if (!showWaitingIndicator) {
    return [];
  }
  const isCommand = Boolean(waitingIndicatorPrompt?.trim().startsWith("/"));
  return [
    {
      id: `live-turn:${sessionId}`,
      kind: "live_turn",
      status: "running",
      estimatedHeightPx: CONVERSATION_OVERVIEW_LIVE_TURN_HEIGHT_PX,
      textSample: `${agent} is working — ${
        isCommand ? "Executing a command" : "Waiting for output"
      }`,
      author: "assistant",
    },
  ];
}

function areConversationOverviewLayoutSnapshotsEqual(
  left: VirtualizedConversationLayoutSnapshot | null,
  right: VirtualizedConversationLayoutSnapshot | null,
): boolean {
  if (left === right) {
    return true;
  }
  if (!left || !right) {
    return false;
  }
  if (
    left.sessionId !== right.sessionId ||
    left.messageCount !== right.messageCount ||
    left.estimatedTotalHeightPx !== right.estimatedTotalHeightPx ||
    left.viewportWidthPx !== right.viewportWidthPx ||
    left.isActive !== right.isActive ||
    left.messages.length !== right.messages.length
  ) {
    return false;
  }

  return left.messages.every((leftMessage, index) => {
    const rightMessage = right.messages[index];
    return (
      rightMessage !== undefined &&
      leftMessage.messageId === rightMessage.messageId &&
      leftMessage.messageIndex === rightMessage.messageIndex &&
      leftMessage.pageIndex === rightMessage.pageIndex &&
      leftMessage.type === rightMessage.type &&
      leftMessage.author === rightMessage.author &&
      leftMessage.estimatedTopPx === rightMessage.estimatedTopPx &&
      leftMessage.estimatedHeightPx === rightMessage.estimatedHeightPx &&
      leftMessage.measuredPageHeightPx === rightMessage.measuredPageHeightPx
    );
  });
}

function extractConversationOverviewViewportSnapshot(
  snapshot: VirtualizedConversationLayoutSnapshot,
): VirtualizedConversationViewportSnapshot {
  return {
    sessionId: snapshot.sessionId,
    messageCount: snapshot.messageCount,
    estimatedTotalHeightPx: snapshot.estimatedTotalHeightPx,
    viewportTopPx: snapshot.viewportTopPx,
    viewportHeightPx: snapshot.viewportHeightPx,
    viewportWidthPx: snapshot.viewportWidthPx,
    isActive: snapshot.isActive,
    visiblePageRange: snapshot.visiblePageRange,
    mountedPageRange: snapshot.mountedPageRange,
  };
}

function areConversationOverviewViewportSnapshotsEqual(
  left: VirtualizedConversationViewportSnapshot | null,
  right: VirtualizedConversationViewportSnapshot | null,
): boolean {
  if (left === right) {
    return true;
  }
  if (!left || !right) {
    return false;
  }
  return (
    left.sessionId === right.sessionId &&
    left.messageCount === right.messageCount &&
    left.estimatedTotalHeightPx === right.estimatedTotalHeightPx &&
    left.viewportTopPx === right.viewportTopPx &&
    left.viewportHeightPx === right.viewportHeightPx &&
    left.viewportWidthPx === right.viewportWidthPx &&
    left.isActive === right.isActive &&
    left.visiblePageRange.startIndex === right.visiblePageRange.startIndex &&
    left.visiblePageRange.endIndex === right.visiblePageRange.endIndex &&
    left.mountedPageRange.startIndex === right.mountedPageRange.startIndex &&
    left.mountedPageRange.endIndex === right.mountedPageRange.endIndex
  );
}
