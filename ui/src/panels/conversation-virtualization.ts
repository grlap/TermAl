// Pure layout / scroll-position helpers that back the virtualized
// conversation message list.
//
// What this file owns:
//   - Virtualization tuning constants consumed by the helpers:
//     `VIRTUALIZED_MESSAGE_GAP_PX` (the CSS gap the layout leaves
//     between successive message cards),
//     `DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT` (fallback viewport
//     height used when the scroll container hasn't reported one
//     yet — matches the production default for an agent session
//     pane), and `DEFAULT_ESTIMATED_MESSAGE_HEIGHT` (the placeholder
//     height used for cards that haven't been measured).
//   - `buildVirtualizedMessageLayout` — turns an array of measured
//     message heights into `{ tops, totalHeight }` with consistent
//     gap handling + safe defaults for unmeasured items.
//   - `findVirtualizedMessageRange` — given the laid-out tops,
//     heights, viewport scroll position, viewport height, and
//     overscan budgets above/below the viewport, returns the
//     `[startIndex, endIndex)` slice of messages that need to be
//     rendered.
//   - `getScrollContainerBottomGap` — pixel distance from the
//     current viewport bottom to the end of the scroll content.
//   - `isScrollContainerNearBottom` — true when the viewport is
//     within 72 px of the bottom. Intentionally mirrors
//     `syncMessageStackScrollPosition`'s `< 72` threshold in
//     `ui/src/scroll-position.ts`; both must stay in sync.
//   - `getAdjustedVirtualizedScrollTopForHeightChange` — when a
//     message grows or shrinks after it has already been
//     measured, returns the new `scrollTop` that keeps the user's
//     anchor stable: leaves `scrollTop` alone for messages at or
//     below the viewport top, refuses to jump for growth of
//     partially-visible messages, and floors at zero for shrinks.
//   - `estimateConversationMessageHeight` — best-effort initial
//     height estimate per `Message` variant so the virtualizer
//     can place unmeasured items without laying them out twice.
//
// What this file does NOT own:
//   - The React components that render the virtualized list
//     (`VirtualizedConversationMessageList`,
//     `MeasuredMessageCard`, `ConversationMessageList`, etc.) —
//     those stay in `./AgentSessionPanel.tsx` with the rest of
//     the session-pane rendering.
//   - The `MessageStack` scroll-position tracker that drives the
//     parent-pane stick/unstick — `syncMessageStackScrollPosition`
//     lives in `../scroll-position`.
//
// Split out of `ui/src/panels/AgentSessionPanel.tsx`. Same
// constants, same function bodies; consumers (including
// `AgentSessionPanel.test.tsx`) import directly from here.

import type { Message } from "../types";

export const VIRTUALIZED_MESSAGE_GAP_PX = 12;
export const DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT = 720;
export const DEFAULT_ESTIMATED_MESSAGE_HEIGHT = 180;
export const VIEWPORT_TOP_HEIGHT_CHANGE_HYSTERESIS_PX = 24;
const DEFAULT_ESTIMATED_MESSAGE_WIDTH_PX = 1120;
const MIN_ESTIMATED_TEXT_CONTENT_WIDTH_PX = 320;
const ESTIMATED_USER_TEXT_HORIZONTAL_CHROME_PX = 96;
const ESTIMATED_ASSISTANT_TEXT_HORIZONTAL_CHROME_PX = 72;
const ESTIMATED_USER_TEXT_CHARACTER_WIDTH_PX = 10.2;
const ESTIMATED_ASSISTANT_TEXT_CHARACTER_WIDTH_PX = 9.8;
const MAX_ESTIMATED_TEXT_MESSAGE_HEIGHT = 4800;

function resolveEstimatedTextContentWidthPx(
  availableWidthPx: number | undefined,
  horizontalChromePx: number,
) {
  const safeAvailableWidthPx =
    Number.isFinite(availableWidthPx) && availableWidthPx && availableWidthPx > 0
      ? availableWidthPx
      : DEFAULT_ESTIMATED_MESSAGE_WIDTH_PX;
  return Math.max(
    safeAvailableWidthPx - horizontalChromePx,
    MIN_ESTIMATED_TEXT_CONTENT_WIDTH_PX,
  );
}

function estimateCharactersPerLineForWidth(contentWidthPx: number, characterWidthPx: number) {
  return Math.max(28, Math.floor(contentWidthPx / characterWidthPx));
}

function estimateWrappedPlainTextLineCount(text: string, charactersPerLine: number) {
  if (text.length === 0) {
    return 1;
  }

  return text.split("\n").reduce((count, line) => {
    return count + Math.max(1, Math.ceil(line.length / charactersPerLine));
  }, 0);
}

function normalizeAssistantMarkdownForHeightEstimate(markdown: string) {
  return markdown
    .replace(/```[\s\S]*?```/g, "[code block]")
    .replace(/\[(.*?)\]\((.*?)\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^>\s?/gm, "")
    .replace(/^\s*(?:[-*+]|\d+\.)\s+/gm, "")
    .replace(/`/g, "")
    .trim();
}

function countMarkdownPatternMatches(markdown: string, pattern: RegExp) {
  return (markdown.match(pattern) ?? []).length;
}

function countFencedCodeBlockLines(markdown: string) {
  const fencedBlocks = markdown.match(/```[\s\S]*?```/g) ?? [];
  return fencedBlocks.reduce((count, block) => {
    const lineCount = block.split("\n").length;
    return count + Math.max(lineCount - 2, 1);
  }, 0);
}

export function buildVirtualizedMessageLayout(itemHeights: number[]) {
  const tops = new Array<number>(itemHeights.length);
  let offset = 0;

  for (let index = 0; index < itemHeights.length; index += 1) {
    const itemHeight =
      Number.isFinite(itemHeights[index]) && itemHeights[index] > 0
        ? itemHeights[index]
        : DEFAULT_ESTIMATED_MESSAGE_HEIGHT;
    tops[index] = offset;
    offset += itemHeight + VIRTUALIZED_MESSAGE_GAP_PX;
  }

  return {
    tops,
    totalHeight: Math.max(offset - VIRTUALIZED_MESSAGE_GAP_PX, 0),
  };
}

export function findVirtualizedMessageRange(
  tops: number[],
  itemHeights: number[],
  scrollTop: number,
  viewportHeight: number,
  overscanAbove: number,
  overscanBelow: number = overscanAbove,
) {
  if (itemHeights.length === 0) {
    return {
      startIndex: 0,
      endIndex: 0,
    };
  }

  const startBoundary = Math.max(scrollTop - overscanAbove, 0);
  const endBoundary =
    scrollTop +
    Math.max(viewportHeight, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT) +
    overscanBelow;

  let startIndex = 0;
  while (
    startIndex < itemHeights.length - 1 &&
    tops[startIndex] + itemHeights[startIndex] < startBoundary
  ) {
    startIndex += 1;
  }

  let endIndex = startIndex;
  while (endIndex < itemHeights.length && tops[endIndex] < endBoundary) {
    endIndex += 1;
  }

  return {
    startIndex,
    endIndex: Math.max(startIndex + 1, endIndex),
  };
}

export function clampVirtualizedViewportScrollTop({
  scrollTop,
  viewportHeight,
  totalHeight,
}: {
  scrollTop: number;
  viewportHeight: number;
  totalHeight: number;
}) {
  const safeScrollTop = Number.isFinite(scrollTop) ? Math.max(scrollTop, 0) : 0;
  const safeViewportHeight =
    Number.isFinite(viewportHeight) && viewportHeight > 0
      ? viewportHeight
      : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT;
  const safeTotalHeight =
    Number.isFinite(totalHeight) && totalHeight > 0 ? totalHeight : 0;
  const maxScrollTop = Math.max(safeTotalHeight - safeViewportHeight, 0);
  return Math.min(safeScrollTop, maxScrollTop);
}

export function getScrollContainerBottomGap(
  node: Pick<HTMLElement, "clientHeight" | "scrollHeight" | "scrollTop">,
) {
  return Math.max(node.scrollHeight - node.clientHeight - node.scrollTop, 0);
}

// Intentionally mirrors `syncMessageStackScrollPosition`'s `< 72`
// stickiness threshold in `ui/src/scroll-position.ts`. A previous revision used
// 96 px, which left a 72-96 px band where the parent pane had already recorded
// `shouldStick: false` (because the user scrolled up past its own
// threshold) but a later measurement in the virtualized panel would still
// re-pin the viewport to the latest message — the user would feel
// "snatched back" when a code block tokenized or an image loaded. Any
// change here should be made symmetrically in both files.
export function isScrollContainerNearBottom(
  node: Pick<HTMLElement, "clientHeight" | "scrollHeight" | "scrollTop">,
) {
  return getScrollContainerBottomGap(node) < 72;
}

export function getAdjustedVirtualizedScrollTopForHeightChange({
  currentScrollTop,
  messageTop,
  nextHeight,
  previousHeight,
}: {
  currentScrollTop: number;
  messageTop: number;
  nextHeight: number;
  previousHeight: number;
}) {
  const heightDelta = nextHeight - previousHeight;
  const previousBottom = messageTop + previousHeight;

  // Never adjust for messages that start at or below the viewport top.
  if (messageTop >= currentScrollTop) {
    return currentScrollTop;
  }

  // Growing a message that is partially visible — or has only just crossed
  // above the fold — should not jump the viewport. In practice tall cards can
  // report a late measurement a few pixels after their bottom clears the
  // viewport top; without a small hysteresis band the helper flips from
  // "leave scrollTop alone" to "add the full height delta", which reads as a
  // sudden forward jump right as the user passes the card.
  //
  // Shrinks still adjust so the anchor can move upward, floored at zero.
  if (
    heightDelta > 0 &&
    previousBottom >= currentScrollTop - VIEWPORT_TOP_HEIGHT_CHANGE_HYSTERESIS_PX
  ) {
    return currentScrollTop;
  }

  return Math.max(currentScrollTop + heightDelta, 0);
}

export function estimateConversationMessageHeight(
  message: Message,
  options: { availableWidthPx?: number; expandedPromptOpen?: boolean } = {},
) {
  switch (message.type) {
    case "text": {
      if (message.author === "assistant") {
        const assistantCharactersPerLine = estimateCharactersPerLineForWidth(
          resolveEstimatedTextContentWidthPx(
            options.availableWidthPx,
            ESTIMATED_ASSISTANT_TEXT_HORIZONTAL_CHROME_PX,
          ),
          ESTIMATED_ASSISTANT_TEXT_CHARACTER_WIDTH_PX,
        );
        const normalizedMarkdown = normalizeAssistantMarkdownForHeightEstimate(message.text);
        const wrappedLineCount = estimateWrappedPlainTextLineCount(
          normalizedMarkdown,
          assistantCharactersPerLine,
        );
        const paragraphCount =
          normalizedMarkdown.length === 0
            ? 1
            : normalizedMarkdown.split(/\n\s*\n/).length;
        const listItemCount = countMarkdownPatternMatches(
          message.text,
          /^\s*(?:[-*+]|\d+\.)\s+/gm,
        );
        const headingCount = countMarkdownPatternMatches(message.text, /^#{1,6}\s+/gm);
        const blockquoteCount = countMarkdownPatternMatches(message.text, /^>\s?/gm);
        const fencedCodeLineCount = countFencedCodeBlockLines(message.text);
        const attachmentHeight = (message.attachments?.length ?? 0) * 54;
        const paragraphGapHeight = Math.max(0, paragraphCount - 1) * 12;
        const listGapHeight = Math.max(0, listItemCount - 1) * 6;
        const headingHeight = headingCount * 10;
        const blockquoteHeight = blockquoteCount * 8;
        const fencedCodeHeight = Math.min(fencedCodeLineCount, 96) * 18;
        return Math.min(
          MAX_ESTIMATED_TEXT_MESSAGE_HEIGHT,
          Math.max(
            108,
            88 +
              wrappedLineCount * 24 +
              attachmentHeight +
              paragraphGapHeight +
              listGapHeight +
              headingHeight +
              blockquoteHeight +
              fencedCodeHeight,
          ),
        );
      }

      const userCharactersPerLine = estimateCharactersPerLineForWidth(
        resolveEstimatedTextContentWidthPx(
          options.availableWidthPx,
          ESTIMATED_USER_TEXT_HORIZONTAL_CHROME_PX,
        ),
        ESTIMATED_USER_TEXT_CHARACTER_WIDTH_PX,
      );
      const lineCount = estimateWrappedPlainTextLineCount(
        message.text,
        userCharactersPerLine,
      );
      const attachmentHeight = (message.attachments?.length ?? 0) * 54;
      const expandedPromptToggleHeight = message.expandedText ? 40 : 0;
      const expandedPromptLineCount =
        options.expandedPromptOpen && message.expandedText
          ? estimateWrappedPlainTextLineCount(message.expandedText, userCharactersPerLine)
          : 0;
      const expandedPromptContentHeight =
        options.expandedPromptOpen && message.expandedText
          ? Math.max(104, 72 + expandedPromptLineCount * 20)
          : 0;
      return Math.min(
        MAX_ESTIMATED_TEXT_MESSAGE_HEIGHT,
        Math.max(
          92,
          78 +
            lineCount * 24 +
            attachmentHeight +
            expandedPromptToggleHeight +
            expandedPromptContentHeight,
        ),
      );
    }
    case "thinking":
      return Math.min(900, Math.max(140, 112 + message.lines.length * 28));
    case "command": {
      const commandLineCount = message.command.length === 0 ? 1 : message.command.split("\n").length;
      const outputLineCount = message.output ? message.output.split("\n").length : 3;
      return Math.min(
        1400,
        Math.max(180, 152 + commandLineCount * 22 + Math.min(outputLineCount, 14) * 20),
      );
    }
    case "diff": {
      const diffLineCount = message.diff.length === 0 ? 1 : message.diff.split("\n").length;
      return Math.min(1500, Math.max(180, 156 + Math.min(diffLineCount, 20) * 20));
    }
    case "markdown": {
      const markdownLineCount =
        message.markdown.length === 0 ? 1 : message.markdown.split("\n").length;
      return Math.min(1600, Math.max(140, 124 + markdownLineCount * 24));
    }
    case "parallelAgents": {
      const detailLineCount = message.agents.reduce((count, agent) => {
        return count + (agent.detail?.split("\n").length ?? 1);
      }, 0);
      return Math.min(900, Math.max(168, 136 + message.agents.length * 52 + detailLineCount * 18));
    }
    case "fileChanges":
      return Math.min(900, Math.max(160, 136 + message.files.length * 44));
    case "subagentResult":
      return Math.min(720, Math.max(132, 128 + Math.min(message.summary.split("\n").length, 4) * 24));
    case "approval":
      return Math.max(220, 188 + (message.detail.length === 0 ? 1 : message.detail.split("\n").length) * 22);
    case "userInputRequest":
      return Math.min(
        1200,
        Math.max(
          220,
          188 +
            message.questions.length * 76 +
            (message.detail.length === 0 ? 1 : message.detail.split("\n").length) * 20,
        ),
      );
    case "mcpElicitationRequest": {
      const detailLineCount = message.detail.length === 0 ? 1 : message.detail.split("\n").length;
      const fieldCount =
        message.request.mode === "form"
          ? Object.keys(message.request.requestedSchema.properties).length
          : 1;
      return Math.min(1200, Math.max(220, 192 + detailLineCount * 20 + fieldCount * 64));
    }
    case "codexAppRequest": {
      const detailLineCount = message.detail.length === 0 ? 1 : message.detail.split("\n").length;
      return Math.min(900, Math.max(220, 188 + detailLineCount * 20));
    }
  }
}
