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
//     overscan budget, returns the `[startIndex, endIndex)` slice
//     of messages that need to be rendered.
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
  overscan: number,
) {
  if (itemHeights.length === 0) {
    return {
      startIndex: 0,
      endIndex: 0,
    };
  }

  const startBoundary = Math.max(scrollTop - overscan, 0);
  const endBoundary =
    scrollTop + Math.max(viewportHeight, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT) + overscan;

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

  // Never adjust for messages that start at or below the viewport top.
  if (messageTop >= currentScrollTop) {
    return currentScrollTop;
  }

  // Growing a partially visible message should not jump the viewport. Shrinks
  // still adjust so the anchor can move upward, floored at zero.
  if (heightDelta > 0 && messageTop + previousHeight > currentScrollTop) {
    return currentScrollTop;
  }

  return Math.max(currentScrollTop + heightDelta, 0);
}

export function estimateConversationMessageHeight(message: Message): number {
  switch (message.type) {
    case "text": {
      const lineCount = message.text.length === 0 ? 1 : message.text.split("\n").length;
      const attachmentHeight = (message.attachments?.length ?? 0) * 54;
      return Math.min(1800, Math.max(92, 78 + lineCount * 24 + attachmentHeight));
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
