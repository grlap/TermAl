// Pure position-space projection for the conversation overview rail.
//
// The server owns the whole-conversation map. This module deliberately knows
// nothing about resident messages, virtualizer layouts, measured heights,
// focus state, or transcript pixels. Its only coordinate is the stable global
// message position returned by /api/sessions/{id}/overview.

import type {
  ConversationOverviewBucket,
  ConversationOverviewMarker,
  SessionOverviewResponse,
} from "../api";

export type ConversationOverviewViewport = {
  startPosition: number;
  endPosition: number;
};

export type ConversationOverviewBucketProjection =
  ConversationOverviewBucket & {
    index: number;
    startPosition: number;
    endPosition: number;
    topPercent: number;
    heightPercent: number;
  };

export type ConversationOverviewMarkerProjection =
  ConversationOverviewMarker & {
    topPercent: number;
  };

export type ConversationOverviewViewportProjection = {
  topPercent: number;
  heightPercent: number;
};

export function projectConversationOverviewBuckets(
  overview: SessionOverviewResponse,
): ConversationOverviewBucketProjection[] {
  const bucketCount = overview.buckets.length;
  const messageCount = Math.max(0, overview.messageCount);
  if (bucketCount === 0 || messageCount === 0) {
    return [];
  }
  return overview.buckets.map((bucket, index) => {
    const startPosition = Math.floor((index * messageCount) / bucketCount);
    const endPosition = Math.max(
      startPosition + 1,
      Math.floor(((index + 1) * messageCount) / bucketCount),
    );
    return {
      ...bucket,
      index,
      startPosition,
      endPosition: Math.min(messageCount, endPosition),
      topPercent: (startPosition / messageCount) * 100,
      heightPercent: Math.max(
        100 / messageCount,
        ((Math.min(messageCount, endPosition) - startPosition) / messageCount) *
          100,
      ),
    };
  });
}

export function projectConversationOverviewMarkers(
  overview: SessionOverviewResponse,
): ConversationOverviewMarkerProjection[] {
  const denominator = Math.max(1, overview.messageCount);
  return overview.markers.map((marker) => ({
    ...marker,
    topPercent:
      (Math.min(
        Math.max(0, marker.position),
        Math.max(0, overview.messageCount - 1),
      ) /
        denominator) *
      100,
  }));
}

export function projectConversationOverviewViewport(
  overview: SessionOverviewResponse,
  viewport: ConversationOverviewViewport,
): ConversationOverviewViewportProjection {
  const messageCount = Math.max(1, overview.messageCount);
  const startPosition = Math.min(
    Math.max(0, viewport.startPosition),
    Math.max(0, overview.messageCount - 1),
  );
  const endPosition = Math.min(
    Math.max(startPosition + 1, viewport.endPosition),
    Math.max(1, overview.messageCount),
  );
  return {
    topPercent: (startPosition / messageCount) * 100,
    heightPercent: Math.max(
      100 / messageCount,
      ((endPosition - startPosition) / messageCount) * 100,
    ),
  };
}

export function conversationOverviewPositionAtFraction(
  overview: SessionOverviewResponse,
  fraction: number,
) {
  if (overview.messageCount <= 0) {
    return 0;
  }
  const clampedFraction = Math.min(Math.max(fraction, 0), 1);
  return Math.min(
    overview.latestPosition,
    Math.floor(clampedFraction * overview.messageCount),
  );
}

export function conversationOverviewBucketIndexForPosition(
  overview: SessionOverviewResponse,
  position: number,
) {
  if (overview.buckets.length === 0 || overview.messageCount <= 0) {
    return 0;
  }
  return Math.min(
    overview.buckets.length - 1,
    Math.floor(
      (Math.min(Math.max(0, position), overview.messageCount - 1) *
        overview.buckets.length) /
        overview.messageCount,
    ),
  );
}
