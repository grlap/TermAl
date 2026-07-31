// Owns first-paint active transcript tail-windowing and demand hydration.
// Does not own conversation rendering, virtualization page bands, overview rail
// rendering, or composer state; those stay in AgentSessionPanel.tsx.
// Split from AgentSessionPanel.tsx to keep the panel focused on composition.

import {
  useCallback,
  useEffect,
  type RefObject,
} from "react";
import { resolvePaneScrollCommand } from "../pane-keyboard";
import {
  requestSessionHistoryNewerPage,
  requestSessionHistoryPage,
} from "../session-history-demand";
import type { Message } from "../types";

const INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX = 160;
const INITIAL_ACTIVE_TRANSCRIPT_WHEEL_DEMAND_THRESHOLD_PX = 8;
const INITIAL_ACTIVE_TRANSCRIPT_TOUCH_PULL_DEMAND_THRESHOLD_PX = 8;

/** @internal Shared by the owning panel and focused regression tests; not a cross-panel API. */
export function includeUndeferredMessageTail(
  deferredMessages: Message[],
  currentMessages: Message[],
) {
  if (deferredMessages === currentMessages) {
    return deferredMessages;
  }
  if (currentMessages.length === 0) {
    return currentMessages;
  }
  if (deferredMessages.length === 0) {
    return currentMessages;
  }

  const sharedLength = Math.min(
    deferredMessages.length,
    currentMessages.length,
  );
  for (let index = 0; index < sharedLength; index += 1) {
    if (currentMessages[index]?.id !== deferredMessages[index]?.id) {
      return currentMessages;
    }
    if (currentMessages[index] !== deferredMessages[index]) {
      return [
        ...deferredMessages.slice(0, index),
        ...currentMessages.slice(index),
      ];
    }
  }

  if (currentMessages.length > deferredMessages.length) {
    return [
      ...deferredMessages,
      ...currentMessages.slice(deferredMessages.length),
    ];
  }

  if (deferredMessages.length > currentMessages.length) {
    return currentMessages;
  }

  return deferredMessages;
}

function transcriptBoundaryDemandDirection(event: KeyboardEvent) {
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
  return command?.kind === "boundary" ? command.direction : null;
}

function isTranscriptTopBoundaryDemandKey(event: KeyboardEvent) {
  return transcriptBoundaryDemandDirection(event) === "up";
}

function isTranscriptDemandKey(event: KeyboardEvent) {
  return (
    event.key === "ArrowUp" || event.key === "Home" || event.key === "PageUp"
  );
}

function shouldIgnoreTranscriptDemandKeyTarget(event: KeyboardEvent) {
  const target = event.target;
  if (
    transcriptBoundaryDemandDirection(event) !== null ||
    event.key === "PageUp" ||
    event.key === "PageDown"
  ) {
    return false;
  }
  if (!(target instanceof Element)) {
    return false;
  }
  return Boolean(
    target.closest("input, textarea, select, option, [contenteditable]"),
  );
}

function isTranscriptDemandKeyEventInScope(
  event: KeyboardEvent,
  scrollNode: HTMLElement,
) {
  const path =
    typeof event.composedPath === "function" ? event.composedPath() : [];
  if (path.length > 0) {
    if (path.includes(scrollNode)) {
      return true;
    }
    const workspacePane = scrollNode.closest(".workspace-pane");
    return workspacePane !== null && path.includes(workspacePane);
  }
  if (!(event.target instanceof Node)) {
    return false;
  }
  if (scrollNode.contains(event.target)) {
    return true;
  }
  return scrollNode.closest(".workspace-pane")?.contains(event.target) ?? false;
}

// The server supplies the recent tail and older pages. This hook only detects
// user demand for the next page. The virtualizer is the single owner of scroll
// anchoring while a page is prepended and measured.
export function useInitialActiveTranscriptMessages({
  hasConversationMarkers,
  hasConversationSearch,
  isActive,
  messageCount,
  messages,
  messagesLoaded,
  hasOlderHistory: explicitHasOlderHistory,
  hasNewerHistory: explicitHasNewerHistory,
  scrollContainerRef,
  sessionId,
}: {
  hasConversationMarkers: boolean;
  hasConversationSearch: boolean;
  isActive: boolean;
  // The session's TRUE transcript length, which is NOT `messages.length` while a
  // large session is tail-hydrated: `messages` then holds only the 20-message
  // tail window. Keying page eligibility off `messages.length` made a 12k-message
  // session look complete, so the demand listeners below never attached and the
  // reader was stranded on the tail (tm-jfx, tm-2po). Optional/nullable so
  // callers without a summary count fall back to what they hold.
  messageCount?: number | null;
  messages: Message[];
  messagesLoaded?: boolean | null;
  hasOlderHistory?: boolean;
  hasNewerHistory?: boolean;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
}) {
  // Take the larger of the summary count and what we actually hold. The summary
  // count is authoritative while the browser holds only a bounded suffix.
  const transcriptMessageCount = Math.max(messageCount ?? 0, messages.length);
  const hasMessages = messages.length > 0;
  const hasOlderHistory =
    explicitHasOlderHistory ??
    (messagesLoaded === false ||
      (messagesLoaded !== true && transcriptMessageCount > messages.length));
  const hasNewerHistory = explicitHasNewerHistory ?? false;
  const requestOlderTranscriptPage = useCallback(() => {
    if (!hasOlderHistory) {
      return false;
    }
    requestSessionHistoryPage(sessionId);
    return true;
  }, [hasOlderHistory, sessionId]);

  useEffect(() => {
    if (!isActive || !hasOlderHistory) {
      return undefined;
    }
    if (hasConversationMarkers || hasConversationSearch || !hasMessages) {
      return undefined;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return undefined;
    }

    let lastTouchClientY: number | null = null;
    let hasDemandInteraction = false;
    let hasQueuedWheelDemandRender = false;
    let disposed = false;
    const requestOlderTranscriptPageAfterWheel = () => {
      if (hasQueuedWheelDemandRender) {
        return;
      }
      hasQueuedWheelDemandRender = true;
      queueMicrotask(() => {
        hasQueuedWheelDemandRender = false;
        if (!disposed) {
          requestOlderTranscriptPage();
        }
      });
    };
    const hydrateIfNearTop = () => {
      if (
        hasDemandInteraction &&
        node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestOlderTranscriptPage();
      }
    };
    const handleWheel = (event: WheelEvent) => {
      if (
        event.ctrlKey ||
        event.deltaY > -INITIAL_ACTIVE_TRANSCRIPT_WHEEL_DEMAND_THRESHOLD_PX
      ) {
        return;
      }
      hasDemandInteraction = true;
      if (node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX) {
        requestOlderTranscriptPageAfterWheel();
      }
    };
    const handleMouseDown = (event: MouseEvent) => {
      if (event.target === node) {
        hasDemandInteraction = true;
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (isTranscriptTopBoundaryDemandKey(event)) {
        // The pane-level boundary command owns the one bounded from=start
        // request. Ordinary page demand must not race it with a backward fetch.
        return;
      }
      if (!isTranscriptDemandKey(event)) {
        return;
      }
      if (!isTranscriptDemandKeyEventInScope(event, node)) {
        return;
      }
      if (shouldIgnoreTranscriptDemandKeyTarget(event)) {
        return;
      }
      hasDemandInteraction = true;
      if (
        node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestOlderTranscriptPage();
      }
    };
    const handleTouchStart = (event: TouchEvent) => {
      hasDemandInteraction = true;
      lastTouchClientY = event.touches[0]?.clientY ?? null;
    };
    const handleTouchMove = (event: TouchEvent) => {
      const touch = event.touches[0] ?? event.changedTouches[0] ?? null;
      if (!touch) {
        return;
      }
      if (
        lastTouchClientY !== null &&
        touch.clientY - lastTouchClientY >
          INITIAL_ACTIVE_TRANSCRIPT_TOUCH_PULL_DEMAND_THRESHOLD_PX &&
        node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestOlderTranscriptPage();
      }
      lastTouchClientY = touch.clientY;
    };
    const handleTouchEnd = () => {
      lastTouchClientY = null;
    };

    node.addEventListener("scroll", hydrateIfNearTop, { passive: true });
    node.addEventListener("wheel", handleWheel, { passive: true });
    node.addEventListener("mousedown", handleMouseDown, { passive: true });
    document.addEventListener("keydown", handleKeyDown, { capture: true });
    node.addEventListener("touchstart", handleTouchStart, { passive: true });
    node.addEventListener("touchmove", handleTouchMove, { passive: true });
    node.addEventListener("touchend", handleTouchEnd, { passive: true });
    node.addEventListener("touchcancel", handleTouchEnd, { passive: true });

    return () => {
      disposed = true;
      node.removeEventListener("scroll", hydrateIfNearTop);
      node.removeEventListener("wheel", handleWheel);
      node.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKeyDown, {
        capture: true,
      });
      node.removeEventListener("touchstart", handleTouchStart);
      node.removeEventListener("touchmove", handleTouchMove);
      node.removeEventListener("touchend", handleTouchEnd);
      node.removeEventListener("touchcancel", handleTouchEnd);
    };
  }, [
    hasConversationMarkers,
    hasConversationSearch,
    hasMessages,
    hasOlderHistory,
    isActive,
    requestOlderTranscriptPage,
    scrollContainerRef,
    sessionId,
  ]);

  useEffect(() => {
    if (!isActive || !hasNewerHistory) {
      return undefined;
    }
    const node = scrollContainerRef.current;
    if (!node) {
      return undefined;
    }
    let hasForwardDemandInteraction = false;
    let requestInFlight = false;
    const requestNewer = () => {
      if (requestInFlight) {
        return;
      }
      requestInFlight = true;
      void requestSessionHistoryNewerPage(sessionId).finally(() => {
        requestInFlight = false;
      });
    };
    const requestIfNearBottom = () => {
      const bottomGap = node.scrollHeight - node.clientHeight - node.scrollTop;
      if (
        hasForwardDemandInteraction &&
        bottomGap <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestNewer();
      }
    };
    const handleWheel = (event: WheelEvent) => {
      if (event.ctrlKey || event.deltaY < INITIAL_ACTIVE_TRANSCRIPT_WHEEL_DEMAND_THRESHOLD_PX) {
        return;
      }
      hasForwardDemandInteraction = true;
      requestIfNearBottom();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (transcriptBoundaryDemandDirection(event) === "down") {
        // The pane-level boundary command owns the one bounded tail request.
        // Ordinary forward demand must not race it with an after-cursor fetch.
        return;
      }
      if (
        !isTranscriptDemandKeyEventInScope(event, node) ||
        shouldIgnoreTranscriptDemandKeyTarget(event) ||
        (event.key !== "ArrowDown" &&
          event.key !== "End" &&
          event.key !== "PageDown")
      ) {
        return;
      }
      hasForwardDemandInteraction = true;
      requestIfNearBottom();
    };
    node.addEventListener("scroll", requestIfNearBottom, { passive: true });
    node.addEventListener("wheel", handleWheel, { passive: true });
    document.addEventListener("keydown", handleKeyDown, { capture: true });
    return () => {
      node.removeEventListener("scroll", requestIfNearBottom);
      node.removeEventListener("wheel", handleWheel);
      document.removeEventListener("keydown", handleKeyDown, {
        capture: true,
      });
    };
  }, [hasNewerHistory, isActive, scrollContainerRef, sessionId]);

  return {
    hasOlderHistory,
    hasNewerHistory,
    messages,
    requestOlderTranscriptPage,
  };
}
