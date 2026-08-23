import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { createPortal } from "react-dom";

import type {
  ConversationOverviewKind,
  SessionOverviewResponse,
} from "../api";
import type { ConversationMarkerKind } from "../types";
import {
  conversationOverviewBucketIndexForPosition,
  conversationOverviewPositionAtFraction,
  projectConversationOverviewBuckets,
  projectConversationOverviewMarkers,
  projectConversationOverviewViewport,
  type ConversationOverviewViewport,
} from "./conversation-overview-map";

export const CONVERSATION_OVERVIEW_MIN_MESSAGES = 30;
export const CONVERSATION_OVERVIEW_VIEWPORT_HANDLE_HEIGHT_PX = 24;

export function ConversationOverviewRail({
  overview,
  messageCount,
  viewport,
  heightPx = null,
  rightPx = null,
  topPx = null,
  portalTarget,
  minMessages = CONVERSATION_OVERVIEW_MIN_MESSAGES,
  onNavigate,
}: {
  overview: SessionOverviewResponse | null;
  messageCount: number;
  viewport: ConversationOverviewViewport;
  heightPx?: number | null;
  rightPx?: number | null;
  topPx?: number | null;
  portalTarget: HTMLElement | null;
  minMessages?: number;
  onNavigate: (position: number) => void;
}) {
  const railRef = useRef<HTMLDivElement | null>(null);
  const dragPointerIdRef = useRef<number | null>(null);
  const pendingPositionRef = useRef<number | null>(null);
  const navigationFrameRef = useRef<number | null>(null);
  const onNavigateRef = useRef(onNavigate);
  onNavigateRef.current = onNavigate;

  const buckets = useMemo(
    () => (overview ? projectConversationOverviewBuckets(overview) : []),
    [overview],
  );
  const markers = useMemo(
    () => (overview ? projectConversationOverviewMarkers(overview) : []),
    [overview],
  );
  const boundedMessageCount = Math.max(0, messageCount);
  const latestPosition =
    overview?.latestPosition ?? Math.max(0, boundedMessageCount - 1);
  const viewportProjection = useMemo(
    () =>
      overview
        ? projectConversationOverviewViewport(overview, viewport)
        : projectPendingConversationOverviewViewport(
            boundedMessageCount,
            viewport,
          ),
    [boundedMessageCount, overview, viewport],
  );
  const currentBucketIndex = overview
    ? conversationOverviewBucketIndexForPosition(
        overview,
        viewport.startPosition,
      )
    : 0;
  const viewportCenterPercent =
    viewportProjection.topPercent + viewportProjection.heightPercent / 2;

  const cancelScheduledNavigation = useCallback(() => {
    if (navigationFrameRef.current !== null) {
      window.cancelAnimationFrame(navigationFrameRef.current);
      navigationFrameRef.current = null;
    }
    pendingPositionRef.current = null;
  }, []);

  useEffect(() => cancelScheduledNavigation, [cancelScheduledNavigation]);

  const flushScheduledNavigation = useCallback(() => {
    navigationFrameRef.current = null;
    const position = pendingPositionRef.current;
    pendingPositionRef.current = null;
    if (position !== null) {
      onNavigateRef.current(position);
    }
  }, []);

  const scheduleNavigation = useCallback(
    (position: number) => {
      pendingPositionRef.current = position;
      if (navigationFrameRef.current === null) {
        navigationFrameRef.current =
          window.requestAnimationFrame(flushScheduledNavigation);
      }
    },
    [flushScheduledNavigation],
  );

  const positionFromClientY = useCallback(
    (clientY: number) => {
      if (boundedMessageCount <= 0 || !railRef.current) {
        return null;
      }
      const bounds = railRef.current.getBoundingClientRect();
      if (bounds.height <= 0) {
        return null;
      }
      const fraction = (clientY - bounds.top) / bounds.height;
      return overview
        ? conversationOverviewPositionAtFraction(overview, fraction)
        : Math.min(
            latestPosition,
            Math.max(0, Math.floor(fraction * boundedMessageCount)),
          );
    },
    [boundedMessageCount, latestPosition, overview],
  );

  const handlePointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (boundedMessageCount <= 0 || event.button !== 0) {
      return;
    }
    dragPointerIdRef.current = event.pointerId;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    const position = positionFromClientY(event.clientY);
    if (position !== null) {
      cancelScheduledNavigation();
      onNavigateRef.current(position);
    }
    event.preventDefault();
  };

  const handlePointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (dragPointerIdRef.current !== event.pointerId) {
      return;
    }
    const position = positionFromClientY(event.clientY);
    if (position !== null) {
      scheduleNavigation(position);
    }
    event.preventDefault();
  };

  const finishPointerDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (dragPointerIdRef.current !== event.pointerId) {
      return;
    }
    dragPointerIdRef.current = null;
    if (event.type === "pointercancel") {
      cancelScheduledNavigation();
    } else {
      const position = positionFromClientY(event.clientY);
      cancelScheduledNavigation();
      if (position !== null) {
        onNavigateRef.current(position);
      }
    }
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
      event.currentTarget.releasePointerCapture?.(event.pointerId);
    }
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (boundedMessageCount <= 0) {
      return;
    }
    const bucketCount = overview?.buckets.length ?? 0;
    const pageStep = Math.max(
      1,
      bucketCount > 0
        ? Math.floor(bucketCount / 10)
        : Math.floor(boundedMessageCount / 10),
    );
    let nextBucketIndex = currentBucketIndex;
    let pendingPosition = viewport.startPosition;
    let boundaryPosition: number | null = null;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowRight":
        if (bucketCount > 0) {
          nextBucketIndex += 1;
        } else {
          pendingPosition += 1;
        }
        break;
      case "ArrowUp":
      case "ArrowLeft":
        if (bucketCount > 0) {
          nextBucketIndex -= 1;
        } else {
          pendingPosition -= 1;
        }
        break;
      case "PageDown":
        if (bucketCount > 0) {
          nextBucketIndex += pageStep;
        } else {
          pendingPosition += pageStep;
        }
        break;
      case "PageUp":
        if (bucketCount > 0) {
          nextBucketIndex -= pageStep;
        } else {
          pendingPosition -= pageStep;
        }
        break;
      case "Home":
        boundaryPosition = 0;
        break;
      case "End":
        boundaryPosition = latestPosition;
        break;
      default:
        return;
    }
    event.preventDefault();
    if (boundaryPosition !== null) {
      onNavigateRef.current(boundaryPosition);
      return;
    }
    if (!overview || bucketCount === 0) {
      onNavigateRef.current(
        Math.min(latestPosition, Math.max(0, pendingPosition)),
      );
      return;
    }
    const clampedBucketIndex = Math.min(
      overview.buckets.length - 1,
      Math.max(0, nextBucketIndex),
    );
    const position = Math.floor(
      (clampedBucketIndex * overview.messageCount) /
        overview.buckets.length,
    );
    onNavigateRef.current(position);
  };

  if (boundedMessageCount < minMessages) {
    return null;
  }
  if (
    !portalTarget ||
    heightPx === null ||
    rightPx === null ||
    topPx === null
  ) {
    return null;
  }

  const rail = (
    <div
      ref={railRef}
      aria-label={
        overview
          ? `Conversation overview, message ${Math.min(
              overview.messageCount,
              viewport.startPosition + 1,
            )} of ${overview.messageCount}`
          : `Conversation overview loading, message ${Math.min(
              boundedMessageCount,
              viewport.startPosition + 1,
            )} of ${boundedMessageCount}`
      }
      aria-orientation="vertical"
      aria-valuemax={latestPosition}
      aria-valuemin={0}
      aria-valuenow={viewport.startPosition}
      className={`conversation-overview-rail${overview ? "" : " is-pending"}`}
      data-testid="conversation-overview-rail"
      onKeyDown={handleKeyDown}
      onPointerCancel={finishPointerDrag}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={finishPointerDrag}
      role="slider"
      style={{
        position: "absolute",
        zIndex: "var(--z-pane-overlay)",
        height: `${heightPx}px`,
        right: `${rightPx}px`,
        top: `${topPx}px`,
      }}
      tabIndex={0}
    >
      <span
        aria-hidden="true"
        className="conversation-overview-visual-track"
        data-testid="conversation-overview-visual-track"
      >
        {buckets.map((bucket) => (
          <span
            key={bucket.index}
            className={`conversation-overview-visual-segment is-${bucket.k}${
              bucket.u > 0 ? " has-user-messages" : ""
            }`}
            data-count={bucket.c}
            data-kind={bucket.k}
            style={{
              height: `${bucket.heightPercent}%`,
              top: `${bucket.topPercent}%`,
            }}
          />
        ))}
      </span>
      {markers.map((marker, index) => (
        <span
          key={`${marker.position}:${marker.kind}:${marker.label ?? ""}:${index}`}
          aria-label={marker.label ?? `${marker.kind} marker`}
          className={`conversation-overview-marker is-${marker.kind}`}
          role="img"
          style={
            {
              top: `${marker.topPercent}%`,
              "--conversation-overview-marker-color":
                conversationOverviewMarkerColor(marker.kind),
            } as CSSProperties
          }
        />
      ))}
      <span
        aria-hidden="true"
        className="conversation-overview-viewport-range"
        data-testid="conversation-overview-viewport"
        style={{
          height: `${viewportProjection.heightPercent}%`,
          top: `${viewportProjection.topPercent}%`,
        }}
      />
      <span
        aria-hidden="true"
        className="conversation-overview-viewport-handle"
        data-testid="conversation-overview-viewport-handle"
        style={
          {
            "--conversation-overview-viewport-center": `${viewportCenterPercent}%`,
            height: `${CONVERSATION_OVERVIEW_VIEWPORT_HANDLE_HEIGHT_PX}px`,
          } as CSSProperties
        }
      />
    </div>
  );

  // The rail is a pane-local overlay whose geometry is measured from this
  // session's transcript viewport. Portaling to the owning workspace pane
  // keeps it outside the scroll container without letting stale body-relative
  // coordinates escape over a neighboring/control-panel pane when the layout
  // moves. React still preserves the logical owner/event tree.
  return createPortal(rail, portalTarget);
}

function projectPendingConversationOverviewViewport(
  messageCount: number,
  viewport: ConversationOverviewViewport,
) {
  const boundedMessageCount = Math.max(1, messageCount);
  const startPosition = Math.min(
    Math.max(0, viewport.startPosition),
    Math.max(0, messageCount - 1),
  );
  const endPosition = Math.min(
    Math.max(startPosition + 1, viewport.endPosition),
    boundedMessageCount,
  );
  return {
    topPercent: (startPosition / boundedMessageCount) * 100,
    heightPercent: Math.max(
      100 / boundedMessageCount,
      ((endPosition - startPosition) / boundedMessageCount) * 100,
    ),
  };
}

function conversationOverviewMarkerColor(kind: ConversationMarkerKind) {
  switch (kind) {
    case "bug":
      return "var(--signal-red)";
    case "decision":
      return "var(--signal-green)";
    case "question":
      return "var(--signal-gold)";
    case "review":
      return "var(--signal-rose)";
    case "handoff":
      return "var(--signal-blue)";
    case "checkpoint":
    case "custom":
      return "var(--muted)";
  }
}

export function conversationOverviewKindLabel(kind: ConversationOverviewKind) {
  switch (kind) {
    case "command":
      return "Commands";
    case "diff":
      return "Diffs";
    case "error":
      return "Errors";
    case "text":
      return "Messages";
  }
}
