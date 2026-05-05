import {
  useMemo,
  useRef,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
} from "react";

import {
  buildConversationOverviewProjection,
  buildConversationOverviewSegments,
  findConversationOverviewItemAtY,
  projectConversationOverviewViewport,
  type ConversationOverviewItem,
  type ConversationOverviewMarkerInput,
  type ConversationOverviewSegment,
  type ConversationOverviewTailItemInput,
} from "./conversation-overview-map";
import type {
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationViewportSnapshot,
} from "./VirtualizedConversationMessageList";
import { normalizeConversationMarkerColor } from "../conversation-marker-colors";
import type { Message } from "../types";

export const CONVERSATION_OVERVIEW_MIN_MESSAGES = 80;
const EMPTY_CONVERSATION_OVERVIEW_MARKERS: readonly ConversationOverviewMarkerInput[] =
  [];
const EMPTY_CONVERSATION_OVERVIEW_TAIL_ITEMS: readonly ConversationOverviewTailItemInput[] =
  [];

export function ConversationOverviewRail({
  messages,
  layoutSnapshot,
  viewportSnapshot = layoutSnapshot,
  markers = EMPTY_CONVERSATION_OVERVIEW_MARKERS,
  tailItems = EMPTY_CONVERSATION_OVERVIEW_TAIL_ITEMS,
  minMessages = CONVERSATION_OVERVIEW_MIN_MESSAGES,
  maxHeightPx,
  onNavigate,
}: {
  messages: readonly Message[];
  layoutSnapshot: VirtualizedConversationLayoutSnapshot | null;
  viewportSnapshot?: VirtualizedConversationViewportSnapshot | null;
  markers?: readonly ConversationOverviewMarkerInput[];
  tailItems?: readonly ConversationOverviewTailItemInput[];
  minMessages?: number;
  maxHeightPx?: number;
  onNavigate: (item: ConversationOverviewItem) => void;
}) {
  const railRef = useRef<HTMLDivElement | null>(null);
  const dragPointerIdRef = useRef<number | null>(null);
  const dragStartedItemRef = useRef<HTMLElement | null>(null);
  const suppressNextClickRef = useRef(false);
  const projection = useMemo(
    () =>
      buildConversationOverviewProjection({
        layoutSnapshot,
        markers,
        maxHeightPx,
        messages,
        tailItems,
      }),
    [layoutSnapshot, markers, maxHeightPx, messages, tailItems],
  );
  const viewportProjection = useMemo(
    () => projectConversationOverviewViewport(projection, viewportSnapshot),
    [projection, viewportSnapshot],
  );
  const segments = useMemo(
    () => buildConversationOverviewSegments(projection),
    [projection],
  );

  if (messages.length < minMessages || projection.items.length === 0) {
    return null;
  }

  const navigateFromClientY = (clientY: number) => {
    const rail = railRef.current;
    if (!rail) {
      return;
    }
    const rect = rail.getBoundingClientRect();
    const mapY = rect.height > 0 ? clientY - rect.top : 0;
    const item = findConversationOverviewItemAtY(projection, mapY);
    if (item) {
      onNavigate(item);
    }
  };

  const handleRailPointerDown = (event: PointerEvent<HTMLElement>) => {
    if (event.button !== 0) {
      return;
    }
    const startedItem =
      event.target instanceof HTMLElement
        ? event.target.closest<HTMLElement>(".conversation-overview-segment")
        : null;
    dragPointerIdRef.current = event.pointerId;
    dragStartedItemRef.current = startedItem || null;
    suppressNextClickRef.current = startedItem !== null;
    if (typeof event.currentTarget.setPointerCapture === "function") {
      event.currentTarget.setPointerCapture(event.pointerId);
    }
    event.preventDefault();
    navigateFromClientY(event.clientY);
  };

  const handleRailPointerMove = (event: PointerEvent<HTMLElement>) => {
    if (dragPointerIdRef.current !== event.pointerId) {
      return;
    }
    event.preventDefault();
    navigateFromClientY(event.clientY);
  };

  const finishRailDrag = (event: PointerEvent<HTMLElement>) => {
    if (dragPointerIdRef.current !== event.pointerId) {
      return;
    }
    dragPointerIdRef.current = null;
    const startedItem = dragStartedItemRef.current;
    const releaseTarget = resolvePointerEventTarget(event);
    const releasedOnStartedItem =
      startedItem !== null &&
      releaseTarget !== null &&
      startedItem.contains(releaseTarget);
    if (!releasedOnStartedItem || event.type === "pointercancel") {
      suppressNextClickRef.current = false;
    }
    dragStartedItemRef.current = null;
    if (
      typeof event.currentTarget.hasPointerCapture === "function" &&
      event.currentTarget.hasPointerCapture(event.pointerId) &&
      typeof event.currentTarget.releasePointerCapture === "function"
    ) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };

  const focusOverviewSegmentAtIndex = (index: number) => {
    railRef.current
      ?.querySelector<HTMLButtonElement>(
        `[data-conversation-overview-index="${index}"]`,
      )
      ?.focus();
  };

  const handleSegmentKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    let nextIndex: number | null = null;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowRight":
        nextIndex = Math.min(index + 1, segments.length - 1);
        break;
      case "ArrowUp":
      case "ArrowLeft":
        nextIndex = Math.max(index - 1, 0);
        break;
      case "End":
        nextIndex = segments.length - 1;
        break;
      case "Home":
        nextIndex = 0;
        break;
    }
    if (nextIndex === null || nextIndex === index) {
      return;
    }
    event.preventDefault();
    focusOverviewSegmentAtIndex(nextIndex);
  };

  const navigateToSegment = (segment: ConversationOverviewSegment) => {
    const item = projection.items[segment.startItemIndex];
    if (item) {
      onNavigate(item);
    }
  };

  return (
    <div
      aria-label="Conversation overview"
      className="conversation-overview-rail"
      role="navigation"
      ref={railRef}
      onPointerDown={handleRailPointerDown}
      onPointerMove={handleRailPointerMove}
      onPointerUp={finishRailDrag}
      onPointerCancel={finishRailDrag}
      style={{ height: `${Math.ceil(projection.totalHeightPx)}px` }}
    >
      {segments.map((segment, index) => (
        <button
          key={segment.id}
          type="button"
          aria-label={overviewSegmentLabel(segment, projection.items)}
          className={`conversation-overview-item conversation-overview-segment is-${segment.kind}${
            segment.status ? ` has-status-${segment.status}` : ""
          }`}
          data-conversation-overview-index={index}
          onClick={(event) => {
            if (suppressNextClickRef.current) {
              suppressNextClickRef.current = false;
              return;
            }
            navigateToSegment(segment);
          }}
          onKeyDown={(event) => handleSegmentKeyDown(event, index)}
          style={{
            top: `${segment.mapTopPx}px`,
            height: `${segment.mapHeightPx}px`,
          }}
          tabIndex={index === 0 ? 0 : -1}
        />
      ))}
      {projection.markers.map((marker) => (
        <span
          key={marker.id}
          aria-label={marker.name}
          className="conversation-overview-marker"
          role="img"
          style={
            {
              top: `${marker.mapTopPx}px`,
              "--conversation-overview-marker-color":
                normalizeConversationMarkerColor(marker.color),
            } as CSSProperties
          }
        />
      ))}
      <span
        aria-hidden="true"
        className="conversation-overview-viewport"
        data-testid="conversation-overview-viewport"
        style={{
          top: `${viewportProjection.viewportTopPx}px`,
          height: `${Math.max(8, viewportProjection.viewportHeightPx)}px`,
        }}
      />
    </div>
  );
}

function resolvePointerEventTarget(event: PointerEvent<HTMLElement>) {
  if (
    typeof document.elementFromPoint === "function" &&
    Number.isFinite(event.clientX) &&
    Number.isFinite(event.clientY)
  ) {
    const target = document.elementFromPoint(event.clientX, event.clientY);
    if (target instanceof HTMLElement) {
      return target;
    }
  }
  return event.target instanceof HTMLElement ? event.target : null;
}

function overviewItemLabel(item: ConversationOverviewItem) {
  const sample = item.textSample ? `: ${item.textSample}` : "";
  return `${overviewKindLabel(item.kind)} ${item.messageIndex + 1}${sample}`;
}

function overviewSegmentLabel(
  segment: ConversationOverviewSegment,
  items: readonly ConversationOverviewItem[],
) {
  const firstItem = items[segment.startItemIndex];
  const lastItem = items[segment.endItemIndex] ?? firstItem;
  if (!firstItem || !lastItem || segment.itemCount <= 1) {
    return firstItem ? overviewItemLabel(firstItem) : "Conversation overview segment";
  }

  const sample = firstItem.textSample ? `: ${firstItem.textSample}` : "";
  return `${overviewSegmentKindLabel(segment)} ${firstItem.messageIndex + 1}-${
    lastItem.messageIndex + 1
  } (${segment.itemCount} messages)${sample}`;
}

function overviewSegmentKindLabel(segment: ConversationOverviewSegment) {
  if (segment.kind === "mixed") {
    return "Mixed messages";
  }
  switch (segment.kind) {
    case "approval":
      return "Approvals";
    case "assistant_text":
      return "Assistant responses";
    case "command":
      return "Commands";
    case "diff":
      return "Diffs";
    case "file_changes":
      return "File changes";
    case "input_request":
      return "Input requests";
    case "live_turn":
      return "Live turns";
    case "parallel_agents":
      return "Parallel agent updates";
    case "subagent_result":
      return "Subagent results";
    case "thinking":
      return "Thinking updates";
    case "user_prompt":
      return "User prompts";
  }
}

function overviewKindLabel(kind: ConversationOverviewItem["kind"]) {
  switch (kind) {
    case "approval":
      return "Approval";
    case "assistant_text":
      return "Assistant response";
    case "command":
      return "Command";
    case "diff":
      return "Diff";
    case "file_changes":
      return "File changes";
    case "input_request":
      return "Input request";
    case "live_turn":
      return "Live turn";
    case "parallel_agents":
      return "Parallel agents";
    case "subagent_result":
      return "Subagent result";
    case "thinking":
      return "Thinking";
    case "user_prompt":
      return "User prompt";
  }
}
