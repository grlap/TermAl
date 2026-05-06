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
const CONVERSATION_OVERVIEW_COMPACT_SEGMENT_THRESHOLD = 160;
const CONVERSATION_OVERVIEW_COMPACT_VISUAL_SEGMENT_COUNT = 96;
const EMPTY_CONVERSATION_OVERVIEW_MARKERS: readonly ConversationOverviewMarkerInput[] =
  [];
const EMPTY_CONVERSATION_OVERVIEW_TAIL_ITEMS: readonly ConversationOverviewTailItemInput[] =
  [];
const EMPTY_COMPACT_OVERVIEW_VISUAL_SEGMENTS: readonly CompactOverviewVisualSegment[] =
  [];

type CompactOverviewVisualSegment = {
  id: string;
  topPx: number;
  heightPx: number;
  fill: string;
};

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
  const shouldCompactSegments =
    segments.length > CONVERSATION_OVERVIEW_COMPACT_SEGMENT_THRESHOLD;
  const compactVisualSegments = useMemo(
    () =>
      shouldCompactSegments
        ? buildCompactOverviewVisualSegments(
            segments,
            projection.totalHeightPx,
          )
        : EMPTY_COMPACT_OVERVIEW_VISUAL_SEGMENTS,
    [projection.totalHeightPx, segments, shouldCompactSegments],
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

  const navigateToSegment = (segment: ConversationOverviewSegment) => {
    const item = projection.items[segment.startItemIndex];
    if (item) {
      onNavigate(item);
    }
  };

  const navigateToSegmentIndex = (index: number) => {
    const segment = segments[index];
    if (segment) {
      navigateToSegment(segment);
    }
  };

  const handleSegmentKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    const nextIndex = resolveOverviewSegmentKeyboardIndex(
      event.key,
      index,
      segments.length,
    );
    if (nextIndex === null || nextIndex === index) {
      return;
    }
    event.preventDefault();
    focusOverviewSegmentAtIndex(nextIndex);
  };

  const handleCompactRailKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const currentItem = findConversationOverviewItemAtY(
      projection,
      viewportProjection.viewportTopPx,
    );
    const currentItemIndex = currentItem
      ? projection.items.indexOf(currentItem)
      : 0;
    const currentIndex = findOverviewSegmentIndexForItemIndex(
      segments,
      currentItemIndex,
    );

    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      navigateToSegmentIndex(currentIndex);
      return;
    }

    const nextIndex = resolveOverviewSegmentKeyboardIndex(
      event.key,
      currentIndex,
      segments.length,
    );
    if (nextIndex === null || nextIndex === currentIndex) {
      return;
    }
    event.preventDefault();
    navigateToSegmentIndex(nextIndex);
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
      onKeyDown={shouldCompactSegments ? handleCompactRailKeyDown : undefined}
      style={{ height: `${Math.ceil(projection.totalHeightPx)}px` }}
      tabIndex={shouldCompactSegments ? 0 : undefined}
    >
      {shouldCompactSegments ? (
        <span
          aria-hidden="true"
          className="conversation-overview-visual-track"
          data-testid="conversation-overview-visual-track"
        >
          {compactVisualSegments.map((visualSegment) => (
            <span
              key={visualSegment.id}
              className="conversation-overview-visual-segment"
              style={{
                background: visualSegment.fill,
                height: formatCssPx(visualSegment.heightPx),
                top: formatCssPx(visualSegment.topPx),
              }}
            />
          ))}
        </span>
      ) : (
        segments.map((segment, index) => (
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
        ))
      )}
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

function resolveOverviewSegmentKeyboardIndex(
  key: string,
  index: number,
  segmentCount: number,
) {
  if (segmentCount <= 0) {
    return null;
  }
  switch (key) {
    case "ArrowDown":
    case "ArrowRight":
      return Math.min(index + 1, segmentCount - 1);
    case "ArrowUp":
    case "ArrowLeft":
      return Math.max(index - 1, 0);
    case "End":
      return segmentCount - 1;
    case "Home":
      return 0;
    default:
      return null;
  }
}

function findOverviewSegmentIndexForItemIndex(
  segments: readonly ConversationOverviewSegment[],
  itemIndex: number,
) {
  if (segments.length === 0) {
    return 0;
  }
  const segmentIndex = segments.findIndex(
    (segment) =>
      itemIndex >= segment.startItemIndex && itemIndex <= segment.endItemIndex,
  );
  return segmentIndex === -1 ? 0 : segmentIndex;
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

function buildCompactOverviewVisualSegments(
  segments: readonly ConversationOverviewSegment[],
  totalHeightPx: number,
) {
  if (segments.length === 0) {
    return EMPTY_COMPACT_OVERVIEW_VISUAL_SEGMENTS;
  }

  const boundedTotalHeightPx = Math.max(1, totalHeightPx);
  const visualBucketCount = Math.min(
    CONVERSATION_OVERVIEW_COMPACT_VISUAL_SEGMENT_COUNT,
    Math.max(1, Math.ceil(boundedTotalHeightPx)),
    segments.length,
  );
  const visualSegments: CompactOverviewVisualSegment[] = [];
  let sourceSegmentIndex = 0;

  for (let bucketIndex = 0; bucketIndex < visualBucketCount; bucketIndex += 1) {
    const bucketTopPx =
      (bucketIndex / visualBucketCount) * boundedTotalHeightPx;
    const bucketBottomPx =
      ((bucketIndex + 1) / visualBucketCount) * boundedTotalHeightPx;
    const sampleY = bucketTopPx + (bucketBottomPx - bucketTopPx) / 2;

    while (
      sourceSegmentIndex < segments.length - 1 &&
      sampleY >=
        segments[sourceSegmentIndex].mapTopPx +
          segments[sourceSegmentIndex].mapHeightPx
    ) {
      sourceSegmentIndex += 1;
    }

    const sourceSegment = segments[sourceSegmentIndex];
    const fill =
      sourceSegment && segmentContainsY(sourceSegment, sampleY)
        ? overviewSegmentCompactFill(sourceSegment)
        : "transparent";
    const previousVisualSegment = visualSegments[visualSegments.length - 1];
    if (
      previousVisualSegment &&
      previousVisualSegment.fill === fill &&
      Math.abs(
        previousVisualSegment.topPx +
          previousVisualSegment.heightPx -
          bucketTopPx,
      ) < 0.5
    ) {
      previousVisualSegment.heightPx = bucketBottomPx - previousVisualSegment.topPx;
      continue;
    }

    visualSegments.push({
      id: `compact:${bucketIndex}`,
      topPx: bucketTopPx,
      heightPx: Math.max(1, bucketBottomPx - bucketTopPx),
      fill,
    });
  }

  return visualSegments;
}

function segmentContainsY(segment: ConversationOverviewSegment, y: number) {
  const segmentTopPx = segment.mapTopPx;
  const segmentBottomPx = segment.mapTopPx + segment.mapHeightPx;
  return y >= segmentTopPx && y <= segmentBottomPx;
}

function overviewSegmentCompactFill(segment: ConversationOverviewSegment) {
  if (segment.status === "error") {
    return "color-mix(in srgb, var(--signal-red) 72%, transparent)";
  }
  if (segment.status === "running") {
    return "color-mix(in srgb, var(--signal-blue) 72%, transparent)";
  }
  switch (segment.kind) {
    case "user_prompt":
      return "color-mix(in srgb, var(--signal-blue) 62%, transparent)";
    case "command":
      return "color-mix(in srgb, var(--signal-green) 42%, transparent)";
    case "diff":
    case "file_changes":
      return "color-mix(in srgb, var(--signal-rose) 42%, transparent)";
    case "approval":
    case "input_request":
      return "color-mix(in srgb, var(--signal-gold) 42%, transparent)";
    case "live_turn":
      return "color-mix(in srgb, var(--signal-blue) 82%, transparent)";
    case "mixed":
    default:
      return "color-mix(in srgb, var(--muted) 50%, transparent)";
  }
}

function formatCssPx(value: number) {
  return `${Math.round(value * 100) / 100}px`;
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
