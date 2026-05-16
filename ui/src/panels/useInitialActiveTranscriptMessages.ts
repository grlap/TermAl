// Owns first-paint active transcript tail-windowing and demand hydration.
// Does not own conversation rendering, virtualization page bands, overview rail
// rendering, or composer state; those stay in AgentSessionPanel.tsx.
// Split from AgentSessionPanel.tsx to keep the panel focused on composition.

import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type RefObject,
} from "react";
import { resolvePaneScrollCommand } from "../pane-keyboard";
import {
  SESSION_TAIL_RENDER_MIN_MESSAGES,
  SESSION_TAIL_WINDOW_MESSAGE_COUNT,
} from "../session-tail-policy";
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

  const sharedLength = Math.min(deferredMessages.length, currentMessages.length);
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

function shouldUseInitialActiveTranscriptTailWindow({
  hasConversationMarkers,
  hasConversationSearch,
  isActive,
  messageCount,
}: {
  hasConversationMarkers: boolean;
  hasConversationSearch: boolean;
  isActive: boolean;
  messageCount: number;
}) {
  return (
    isActive &&
    messageCount > SESSION_TAIL_RENDER_MIN_MESSAGES &&
    !hasConversationMarkers &&
    !hasConversationSearch
  );
}

function isTranscriptTopBoundaryDemandKey(event: KeyboardEvent) {
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
  return command?.kind === "boundary" && command.direction === "up";
}

function isTranscriptDemandKey(event: KeyboardEvent) {
  return (
    event.key === "ArrowUp" ||
    event.key === "Home" ||
    event.key === "PageUp"
  );
}

function shouldIgnoreTranscriptDemandKeyTarget(event: KeyboardEvent) {
  const target = event.target;
  if (isTranscriptTopBoundaryDemandKey(event)) {
    return false;
  }
  if (!(target instanceof Element)) {
    return false;
  }
  return Boolean(
    target.closest(
      "input, textarea, select, option, [contenteditable]",
    ),
  );
}

function isTranscriptDemandKeyEventInScope(
  event: KeyboardEvent,
  scrollNode: HTMLElement,
) {
  const path =
    typeof event.composedPath === "function" ? event.composedPath() : [];
  if (path.length > 0) {
    return path.includes(scrollNode);
  }
  return event.target instanceof Node && scrollNode.contains(event.target);
}

// This panel-level tail window avoids building React elements for hundreds of
// offscreen messages on first paint. The virtualizer still owns mounted page
// bands and scroll positioning once the full transcript is available.
export function useInitialActiveTranscriptMessages({
  hasConversationMarkers,
  hasConversationSearch,
  isActive,
  messages,
  scrollContainerRef,
  sessionId,
}: {
  hasConversationMarkers: boolean;
  hasConversationSearch: boolean;
  isActive: boolean;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
}) {
  const [hydrationState, setHydrationState] = useState({
    hydrated: false,
    sessionId,
  });

  const isTailEligible = shouldUseInitialActiveTranscriptTailWindow({
    hasConversationMarkers,
    hasConversationSearch,
    isActive,
    messageCount: messages.length,
  });
  const isImplicitlyHydrated =
    !isTailEligible &&
    messages.length > SESSION_TAIL_RENDER_MIN_MESSAGES;
  const isExplicitlyHydrated =
    hydrationState.sessionId === sessionId && hydrationState.hydrated;
  const isHydrated = isExplicitlyHydrated || isImplicitlyHydrated;
  const isWindowed = isTailEligible && !isHydrated;
  const hasMessages = messages.length > 0;

  useEffect(() => {
    setHydrationState((current) => {
      const currentHydrated =
        current.sessionId === sessionId ? current.hydrated : false;
      const nextHydrated = currentHydrated || isImplicitlyHydrated;
      if (
        current.sessionId === sessionId &&
        current.hydrated === nextHydrated
      ) {
        return current;
      }
      return {
        hydrated: nextHydrated,
        sessionId,
      };
    });
  }, [isImplicitlyHydrated, sessionId]);

  const requestFullTranscriptRender = useCallback(() => {
    if (isHydrated) {
      return false;
    }

    setHydrationState((current) =>
      current.sessionId === sessionId && current.hydrated
        ? current
        : {
            hydrated: true,
            sessionId,
          },
    );
    return true;
  }, [isHydrated, sessionId]);

  useEffect(() => {
    if (isHydrated || !isWindowed) {
      return undefined;
    }
    if (
      !isActive ||
      hasConversationMarkers ||
      hasConversationSearch ||
      !hasMessages
    ) {
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
    const requestFullTranscriptRenderAfterWheel = () => {
      if (hasQueuedWheelDemandRender) {
        return;
      }
      hasQueuedWheelDemandRender = true;
      queueMicrotask(() => {
        hasQueuedWheelDemandRender = false;
        if (!disposed) {
          requestFullTranscriptRender();
        }
      });
    };
    const hydrateIfNearTop = () => {
      if (
        hasDemandInteraction &&
        node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestFullTranscriptRender();
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
        requestFullTranscriptRenderAfterWheel();
      }
    };
    const handleMouseDown = (event: MouseEvent) => {
      if (event.target === node) {
        hasDemandInteraction = true;
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
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
      requestFullTranscriptRender();
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
        requestFullTranscriptRender();
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
    isActive,
    isWindowed,
    requestFullTranscriptRender,
    scrollContainerRef,
    sessionId,
  ]);

  const windowedMessages = useMemo(
    () =>
      isWindowed
        ? messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT)
        : messages,
    [isWindowed, messages],
  );

  return {
    isWindowed,
    messages: windowedMessages,
    requestFullTranscriptRender,
  };
}
