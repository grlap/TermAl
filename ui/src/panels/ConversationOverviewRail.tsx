import {
  startTransition,
  useEffect,
  useMemo,
  useRef,
  useState,
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
  type ConversationOverviewMessageMetadataCache,
  type ConversationOverviewProjection,
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
const CONVERSATION_OVERVIEW_FOCUSED_PROMPT_FALLBACK_DELAY_MS = 240;
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

type ConversationOverviewProjectionInput = {
  messages: readonly Message[];
  layoutSnapshot: VirtualizedConversationLayoutSnapshot | null;
  markers: readonly ConversationOverviewMarkerInput[];
  tailItems: readonly ConversationOverviewTailItemInput[];
  maxHeightPx?: number;
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
  const [focusedSegmentIndex, setFocusedSegmentIndex] = useState(0);
  const projection = useConversationOverviewProjection({
    layoutSnapshot,
    markers,
    maxHeightPx,
    messages,
    tailItems,
  });
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
  const currentViewportItem = findConversationOverviewItemAtY(
    projection,
    viewportProjection.viewportTopPx,
  );
  const currentViewportItemIndex = currentViewportItem
    ? projection.items.indexOf(currentViewportItem)
    : 0;
  const currentSegmentIndex = findOverviewSegmentIndexForItemIndex(
    segments,
    currentViewportItemIndex,
  );
  const currentSegment = segments[currentSegmentIndex] ?? segments[0] ?? null;
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

  useEffect(() => {
    setFocusedSegmentIndex((index) =>
      Math.min(index, Math.max(segments.length - 1, 0)),
    );
  }, [segments.length]);

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
    if (shouldCompactSegments) {
      event.currentTarget.focus({ preventScroll: true });
    }
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
    setFocusedSegmentIndex(index);
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
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      navigateToSegmentIndex(currentSegmentIndex);
      return;
    }

    const nextIndex = resolveOverviewSegmentKeyboardIndex(
      event.key,
      currentSegmentIndex,
      segments.length,
    );
    if (nextIndex === null || nextIndex === currentSegmentIndex) {
      return;
    }
    event.preventDefault();
    navigateToSegmentIndex(nextIndex);
  };

  return (
    <div
      aria-label="Conversation overview"
      aria-orientation={shouldCompactSegments ? "vertical" : undefined}
      aria-valuemax={shouldCompactSegments ? segments.length : undefined}
      aria-valuemin={shouldCompactSegments ? 1 : undefined}
      aria-valuenow={shouldCompactSegments ? currentSegmentIndex + 1 : undefined}
      aria-valuetext={
        shouldCompactSegments && currentSegment
          ? overviewSegmentLabel(currentSegment, projection.items)
          : undefined
      }
      className="conversation-overview-rail"
      role={shouldCompactSegments ? "slider" : "navigation"}
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
            onFocus={() => setFocusedSegmentIndex(index)}
            tabIndex={index === focusedSegmentIndex ? 0 : -1}
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

function useConversationOverviewProjection({
  messages,
  layoutSnapshot,
  markers,
  tailItems,
  maxHeightPx,
}: ConversationOverviewProjectionInput) {
  const metadataCacheRef = useRef<ConversationOverviewMessageMetadataCache>(
    new WeakMap(),
  );
  const latestInputRef = useRef<ConversationOverviewProjectionInput>({
    layoutSnapshot,
    markers,
    maxHeightPx,
    messages,
    tailItems,
  });
  const latestProjectionRef = useRef<ConversationOverviewProjection | null>(null);
  const [, setDeferredProjectionVersion] = useState(0);
  const isPromptFocused = useComposerPromptFocused();

  latestInputRef.current = {
    layoutSnapshot,
    markers,
    maxHeightPx,
    messages,
    tailItems,
  };

  const immediateProjection = useMemo(() => {
    if (isPromptFocused && latestProjectionRef.current !== null) {
      return null;
    }

    return buildConversationOverviewProjection({
      layoutSnapshot,
      markers,
      maxHeightPx,
      messageMetadataCache: metadataCacheRef.current,
      messages,
      tailItems,
    });
  }, [isPromptFocused, layoutSnapshot, markers, maxHeightPx, messages, tailItems]);

  if (immediateProjection) {
    latestProjectionRef.current = immediateProjection;
  }

  useEffect(() => {
    if (!isPromptFocused || immediateProjection) {
      return undefined;
    }

    let cancelled = false;
    const cancelProjectionBuild = scheduleConversationOverviewProjectionBuild(() => {
      if (cancelled) {
        return;
      }
      const latestInput = latestInputRef.current;
      const nextProjection = buildConversationOverviewProjection({
        layoutSnapshot: latestInput.layoutSnapshot,
        markers: latestInput.markers,
        maxHeightPx: latestInput.maxHeightPx,
        messageMetadataCache: metadataCacheRef.current,
        messages: latestInput.messages,
        tailItems: latestInput.tailItems,
      });

      if (cancelled) {
        return;
      }
      latestProjectionRef.current = nextProjection;
      startTransition(() => {
        if (cancelled) {
          return;
        }
        setDeferredProjectionVersion((version) => version + 1);
      });
    });
    return () => {
      cancelled = true;
      cancelProjectionBuild();
    };
  }, [
    immediateProjection,
    isPromptFocused,
    layoutSnapshot,
    markers,
    maxHeightPx,
    messages,
    tailItems,
  ]);

  if (latestProjectionRef.current) {
    return latestProjectionRef.current;
  }

  const fallbackProjection = buildConversationOverviewProjection({
    layoutSnapshot,
    markers,
    maxHeightPx,
    messageMetadataCache: metadataCacheRef.current,
    messages,
    tailItems,
  });
  latestProjectionRef.current = fallbackProjection;
  return fallbackProjection;
}

function useComposerPromptFocused() {
  const [isPromptFocused, setIsPromptFocused] = useState(
    isComposerPromptFocused,
  );

  useEffect(() => {
    const updateFocusedPrompt = () => {
      setIsPromptFocused(isComposerPromptFocused());
    };

    document.addEventListener("focusin", updateFocusedPrompt, true);
    document.addEventListener("focusout", updateFocusedPrompt, true);
    return () => {
      document.removeEventListener("focusin", updateFocusedPrompt, true);
      document.removeEventListener("focusout", updateFocusedPrompt, true);
    };
  }, []);

  return isPromptFocused;
}

function isComposerPromptFocused() {
  const activeElement = document.activeElement;
  return (
    activeElement instanceof HTMLTextAreaElement &&
    activeElement.dataset.conversationComposerInput === "true"
  );
}

function scheduleConversationOverviewProjectionBuild(run: () => void) {
  const idleWindow = window as Window &
    typeof globalThis & {
      requestIdleCallback?: (
        callback: IdleRequestCallback,
        options?: IdleRequestOptions,
      ) => number;
      cancelIdleCallback?: (handle: number) => void;
    };

  if (
    typeof idleWindow.requestIdleCallback === "function" &&
    typeof idleWindow.cancelIdleCallback === "function"
  ) {
    let idleHandle: number | null = null;
    let timeoutId: number | null = null;
    let didRun = false;
    const clearScheduled = () => {
      if (idleHandle !== null) {
        idleWindow.cancelIdleCallback?.(idleHandle);
        idleHandle = null;
      }
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
        timeoutId = null;
      }
    };
    const runOnce = () => {
      if (didRun) {
        return;
      }
      didRun = true;
      clearScheduled();
      run();
    };
    function scheduleIdle() {
      idleHandle =
        idleWindow.requestIdleCallback?.(runWhenIdle, {
          timeout: CONVERSATION_OVERVIEW_FOCUSED_PROMPT_FALLBACK_DELAY_MS,
        }) ?? null;
    }
    const runWhenIdle: IdleRequestCallback = (deadline) => {
      idleHandle = null;
      if (
        isComposerPromptFocused() &&
        deadline.timeRemaining() < 8 &&
        !deadline.didTimeout
      ) {
        scheduleIdle();
        return;
      }
      runOnce();
    };

    timeoutId = window.setTimeout(
      runOnce,
      CONVERSATION_OVERVIEW_FOCUSED_PROMPT_FALLBACK_DELAY_MS,
    );
    scheduleIdle();
    return () => {
      didRun = true;
      clearScheduled();
    };
  }

  const timeoutId = window.setTimeout(
    run,
    CONVERSATION_OVERVIEW_FOCUSED_PROMPT_FALLBACK_DELAY_MS,
  );
  return () => {
    window.clearTimeout(timeoutId);
  };
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
