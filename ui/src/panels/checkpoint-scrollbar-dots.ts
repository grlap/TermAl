// Owns checkpoint position and collision grouping for native-scrollbar dots.
// Does not own scrolling, history requests, or marker selection.
// Replaces the marker projection previously in conversation-overview-map.ts.
import type { ConversationMarker } from "../types";

export type CheckpointDotGroup = {
  markers: ConversationMarker[];
  topPx: number;
};

export function groupCheckpointScrollbarDots(
  markers: readonly ConversationMarker[],
  messageCount: number,
  heightPx: number,
  minimumGapPx = 14,
): CheckpointDotGroup[] {
  if (messageCount <= 0 || heightPx <= 0) return [];
  const usableHeight = Math.max(0, heightPx - minimumGapPx);
  const checkpoints = markers
    .filter(marker => marker.kind === "checkpoint" && Number.isFinite(marker.messageIndexHint))
    .map(marker => ({
      marker,
      topPx: minimumGapPx / 2 + usableHeight *
        Math.min(Math.max(marker.messageIndexHint, 0), messageCount - 1) /
        Math.max(1, messageCount - 1),
    }))
    .sort((left, right) => left.topPx - right.topPx || left.marker.id.localeCompare(right.marker.id));
  const groups: CheckpointDotGroup[] = [];
  for (const checkpoint of checkpoints) {
    const previous = groups[groups.length - 1];
    if (previous && checkpoint.topPx - previous.topPx < minimumGapPx) {
      previous.markers.push(checkpoint.marker);
    } else {
      groups.push({ topPx: checkpoint.topPx, markers: [checkpoint.marker] });
    }
  }
  return groups;
}
