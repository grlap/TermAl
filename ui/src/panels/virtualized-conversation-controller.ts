// Owns pure mounted-range and scroll-authority policy for the virtualized
// conversation controller. React effects and DOM rendering stay in their
// dedicated modules.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.

import type { Message } from "../types";
import type { VirtualizedRange } from "./virtualized-conversation-measurement";
import type { MessageWindowSnapshot } from "./virtualized-conversation-types";

// Keep the same four-viewport reserve in both directions. The virtualizer's
// page-height estimates can be much taller than compact command/message rows;
// a smaller upward reserve lets fast wheel or touch momentum reach the top
// spacer before real DOM replaces it.
export const ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 4;
// Expandable delegation and peer-message bodies are capped at 60vh. When one
// collapses, content below it can move upward by most of a viewport before the
// ResizeObserver measurement and native scroll anchoring settle. The matching
// below reserve plus whole-page hysteresis keeps that movement on real DOM.
export const ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS = 4;
export const BOUNDARY_SEEK_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 1;
export const BOUNDARY_SEEK_MOUNTED_RESERVE_BELOW_VIEWPORTS = 0;
export const ACTIVE_MOUNTED_EXTRA_PAGES_BELOW = 2;
export const ACTIVE_VIEWPORT_STARTUP_RESYNC_FRAMES = 12;
export const BOTTOM_BOUNDARY_REVEAL_SETTLE_FRAMES = 12;
export const BOTTOM_BOUNDARY_REVEAL_DELAY_MS = 220;
export const POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES = 20;
const ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_EXTRA_PAGES = 12;
const ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_MULTIPLIER = 2;
export const VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS = 200;
export const PREPENDED_MESSAGE_ANCHOR_RESTORE_ATTEMPTS = 3;
// Heavy content paint resumes almost immediately after input stops, while the
// broader range controller keeps its 200ms quiet window.
export const DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS = 10;

export function resolveVirtualizedScrollWriteTarget({
  hasUserScrollInteraction,
  isDetachedFromBottom,
  realDomBottom,
  requestedScrollTop,
  shouldKeepBottom,
}: {
  hasUserScrollInteraction: boolean;
  isDetachedFromBottom: boolean;
  realDomBottom: number;
  requestedScrollTop: number;
  shouldKeepBottom: boolean;
}) {
  return shouldKeepBottom && !isDetachedFromBottom && !hasUserScrollInteraction
    ? realDomBottom
    : requestedScrollTop;
}

export function rangesEqual(
  first: VirtualizedRange,
  second: VirtualizedRange,
) {
  return (
    first.startIndex === second.startIndex && first.endIndex === second.endIndex
  );
}

function getRangePageCount(range: VirtualizedRange) {
  return Math.max(range.endIndex - range.startIndex, 0);
}

export function shouldCollapseIncrementalMountedRange(
  currentRange: VirtualizedRange,
  targetRange: VirtualizedRange,
) {
  const targetPageCount = Math.max(getRangePageCount(targetRange), 1);
  const combinedPageCount =
    Math.max(currentRange.endIndex, targetRange.endIndex) -
    Math.min(currentRange.startIndex, targetRange.startIndex);
  const maxMountedPageCount = Math.max(
    targetPageCount * ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_MULTIPLIER,
    targetPageCount + ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_EXTRA_PAGES,
  );
  return combinedPageCount > maxMountedPageCount;
}

export function resolvePrependedMessageCount(
  previous: MessageWindowSnapshot,
  currentMessages: readonly Message[],
  sessionId: string,
) {
  if (
    previous.sessionId !== sessionId ||
    previous.ids.length === 0 ||
    previous.ids.length >= currentMessages.length
  ) {
    return null;
  }

  const firstPreviousId = previous.ids[0];
  const maxStartIndex = currentMessages.length - previous.ids.length;
  for (let startIndex = 0; startIndex <= maxStartIndex; startIndex += 1) {
    if (currentMessages[startIndex]?.id !== firstPreviousId) {
      continue;
    }
    const matchesPreviousWindow = previous.ids.every(
      (messageId, index) => currentMessages[startIndex + index]?.id === messageId,
    );
    if (matchesPreviousWindow) {
      return startIndex > 0 ? startIndex : null;
    }
  }

  return null;
}
