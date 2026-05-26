// Owns virtualized conversation page segmentation, page-height estimation,
// and DOM measurement helpers.
// Does not own React state, mounted-range scheduling, or message rendering.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import { isExpandedPromptOpen } from "../ExpandedPromptPanel";
import type { Message } from "../types";
import {
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  VIRTUALIZED_MESSAGE_GAP_PX,
} from "./conversation-virtualization";

export const VIRTUALIZED_MESSAGES_PER_PAGE = 8;
const MIN_PAGE_COVERAGE_HEIGHT_PX = 64;

export type VirtualizedRange = { startIndex: number; endIndex: number };

export type MessageLocation = {
  message: Message;
  messageIndex: number;
  pageIndex: number;
  pageLocalIndex: number;
};

export type VisibleMessageAnchor = {
  messageId: string;
  viewportOffsetPx: number;
};

export type PendingVisibleMessageAnchor = VisibleMessageAnchor & {
  remainingAttempts: number;
};

export type MessagePage = {
  key: string;
  pageIndex: number;
  startIndex: number;
  endIndex: number;
  hasTrailingGap: boolean;
  messages: Message[];
};

export type EstimatedPageHeightEntry = {
  cacheKey: string;
  height: number;
};

export function buildMessagePages(messages: Message[]) {
  const pages: MessagePage[] = [];
  for (
    let startIndex = 0;
    startIndex < messages.length;
    startIndex += VIRTUALIZED_MESSAGES_PER_PAGE
  ) {
    const endIndex = Math.min(
      startIndex + VIRTUALIZED_MESSAGES_PER_PAGE,
      messages.length,
    );
    const pageMessages = messages.slice(startIndex, endIndex);
    const firstMessageId = pageMessages[0]?.id ?? `page-${startIndex}`;
    const lastMessageId =
      pageMessages[pageMessages.length - 1]?.id ?? firstMessageId;
    pages.push({
      key: `${startIndex}:${endIndex}:${firstMessageId}:${lastMessageId}`,
      pageIndex: pages.length,
      startIndex,
      endIndex,
      hasTrailingGap: endIndex < messages.length,
      messages: pageMessages,
    });
  }
  return pages;
}

export function buildPageLayout(pageHeights: number[]) {
  const tops = new Array<number>(pageHeights.length);
  let totalHeight = 0;
  for (let index = 0; index < pageHeights.length; index += 1) {
    tops[index] = totalHeight;
    totalHeight += pageHeights[index] ?? 0;
  }
  return { tops, totalHeight };
}

export function resolveRenderedPageCoverageHeight(pageNodes: HTMLElement[]) {
  let minHeight = Number.POSITIVE_INFINITY;
  for (const pageNode of pageNodes) {
    const height = pageNode.getBoundingClientRect().height;
    if (Number.isFinite(height) && height > 0) {
      minHeight = Math.min(minHeight, height);
    }
  }

  return Number.isFinite(minHeight)
    ? Math.max(minHeight, MIN_PAGE_COVERAGE_HEIGHT_PX)
    : null;
}

export function resolvePageCoverageHeight(
  pageHeights: number[],
  pageIndex: number,
  renderedCoverageHeight: number | null,
) {
  const measuredOrEstimatedHeight = pageHeights[pageIndex];
  const fallbackHeight =
    Number.isFinite(measuredOrEstimatedHeight) && measuredOrEstimatedHeight > 0
      ? measuredOrEstimatedHeight
      : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT;
  const coverageHeight =
    renderedCoverageHeight !== null
      ? Math.min(fallbackHeight, renderedCoverageHeight)
      : fallbackHeight;
  return Math.max(coverageHeight, MIN_PAGE_COVERAGE_HEIGHT_PX);
}

function getMountedMessageSlots(virtualizedListRoot: ParentNode | null) {
  if (!virtualizedListRoot) {
    return [];
  }
  return Array.from(
    virtualizedListRoot.querySelectorAll<HTMLElement>(
      ".virtualized-message-slot[data-message-id]",
    ),
  );
}

export function findMountedMessageSlotById(
  virtualizedListRoot: ParentNode | null,
  messageId: string,
) {
  return (
    getMountedMessageSlots(virtualizedListRoot).find(
      (slot) => slot.dataset.messageId === messageId,
    ) ?? null
  );
}

export function captureFirstVisibleMountedMessageAnchor(
  virtualizedListRoot: ParentNode | null,
  scrollContainerNode: HTMLElement,
): VisibleMessageAnchor | null {
  const containerRect = scrollContainerNode.getBoundingClientRect();
  for (const slot of getMountedMessageSlots(virtualizedListRoot)) {
    const rect = slot.getBoundingClientRect();
    if (rect.bottom <= containerRect.top || rect.top >= containerRect.bottom) {
      continue;
    }
    if (!slot.dataset.messageId) {
      return null;
    }
    return {
      messageId: slot.dataset.messageId,
      viewportOffsetPx: rect.top - containerRect.top,
    };
  }
  return null;
}

export function getMountedSlotViewportOffsetPx(
  scrollContainerNode: HTMLElement,
  slotNode: HTMLElement,
) {
  const containerRect = scrollContainerNode.getBoundingClientRect();
  const slotRect = slotNode.getBoundingClientRect();
  return slotRect.top - containerRect.top;
}

export function doesMountedPageIntersectViewport(
  scrollContainerNode: HTMLElement,
  pageNode: HTMLElement | null | undefined,
) {
  if (!pageNode) {
    return false;
  }
  const containerRect = scrollContainerNode.getBoundingClientRect();
  const pageRect = pageNode.getBoundingClientRect();
  return pageRect.bottom > containerRect.top && pageRect.top < containerRect.bottom;
}

export function estimatePageHeight(
  page: MessagePage,
  estimateMessageHeight: (message: Message) => number,
) {
  if (page.messages.length === 0) {
    return 0;
  }

  let total = 0;
  for (let index = 0; index < page.messages.length; index += 1) {
    total += estimateMessageHeight(page.messages[index]!);
    if (index < page.messages.length - 1) {
      total += VIRTUALIZED_MESSAGE_GAP_PX;
    }
  }
  if (page.hasTrailingGap) {
    total += VIRTUALIZED_MESSAGE_GAP_PX;
  }
  return total;
}

export function buildPageEstimateCacheKey(
  page: MessagePage,
  availableWidthPx: number,
) {
  const widthBucket = resolveEstimateWidthBucket(availableWidthPx);
  const expandedPromptKey = page.messages
    .flatMap((message) =>
      message.type === "text" &&
      message.author === "you" &&
      message.expandedText &&
      isExpandedPromptOpen(message.id)
        ? [message.id]
        : [],
    )
    .join(",");
  return `${page.key}:${widthBucket}:${expandedPromptKey}`;
}

function resolveEstimateWidthBucket(availableWidthPx: number) {
  return Number.isFinite(availableWidthPx) && availableWidthPx > 0
    ? Math.round(availableWidthPx)
    : 0;
}

export function estimateMessageOffsetWithinPage(
  page: MessagePage,
  messageLocalIndex: number,
  estimateMessageHeight: (message: Message) => number,
) {
  let offset = 0;
  for (let index = 0; index < messageLocalIndex; index += 1) {
    offset += estimateMessageHeight(page.messages[index]!);
    offset += VIRTUALIZED_MESSAGE_GAP_PX;
  }
  return offset;
}
