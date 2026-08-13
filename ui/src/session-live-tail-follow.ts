// Owns the pure state machine and motion policy for session live-tail follow.
// Does not own DOM writes, virtualizer notifications, or user-detach input.
// Split from: ui/src/SessionPaneView.scroll.ts.

import { buildMessageListSignature } from "./app-utils";
import type { Message } from "./types";

export const SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS = 1000 / 60;
export const SESSION_BOTTOM_FOLLOW_MAX_FRAME_MS = 100;
export const SESSION_BOTTOM_FOLLOW_SNAP_DISTANCE_PX = 2;
const SESSION_BOTTOM_FOLLOW_TIME_CONSTANT_MS = 36;
const SESSION_BOTTOM_FOLLOW_MAX_SPEED_PX_PER_MS = 2.5;

export type LatestTurnOutputState = {
  hasAgentOutput: boolean;
  promptMessageId: string | null;
};

export function resolveSessionBottomFollowScrollTop(
  currentScrollTop: number,
  targetScrollTop: number,
  frameDurationMs = SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS,
) {
  // Layout can briefly report a smaller bottom while the live-turn card is
  // replaced or a virtualized card swaps its estimate for a measurement. The
  // browser clamps scrollTop if content truly shrank; the follow authority must
  // not add a second explicit upward write before the next growth.
  if (targetScrollTop <= currentScrollTop) {
    return currentScrollTop;
  }

  const distance = targetScrollTop - currentScrollTop;
  if (distance <= SESSION_BOTTOM_FOLLOW_SNAP_DISTANCE_PX) {
    return targetScrollTop;
  }

  const boundedFrameDurationMs = Number.isFinite(frameDurationMs)
    ? Math.min(Math.max(frameDurationMs, 1), SESSION_BOTTOM_FOLLOW_MAX_FRAME_MS)
    : SESSION_BOTTOM_FOLLOW_REFERENCE_FRAME_MS;
  const followFactor =
    1 -
    Math.exp(-boundedFrameDurationMs / SESSION_BOTTOM_FOLLOW_TIME_CONSTANT_MS);
  // A newly inserted command/result card can move the bottom by hundreds of
  // pixels in one commit. Pure exponential convergence applies roughly 37% of
  // that distance on the first 60 Hz frame, which is monotonic but still looks
  // like the whole transcript jumped. Cap travel by elapsed frame time so
  // structural additions glide at a stable velocity while small streaming
  // growth keeps the existing responsive easing.
  const maximumFrameTravel =
    boundedFrameDurationMs * SESSION_BOTTOM_FOLLOW_MAX_SPEED_PX_PER_MS;
  const nextScrollTop =
    currentScrollTop + Math.min(distance * followFactor, maximumFrameTravel);
  return targetScrollTop - nextScrollTop <= SESSION_BOTTOM_FOLLOW_SNAP_DISTANCE_PX
    ? targetScrollTop
    : Math.min(nextScrollTop, targetScrollTop);
}

export function resolveSessionBottomFollowWriteScrollTop({
  currentScrollTop,
  frameDurationMs,
  snapBeforePaint,
  targetScrollTop,
}: {
  currentScrollTop: number;
  frameDurationMs?: number;
  snapBeforePaint: boolean;
  targetScrollTop: number;
}) {
  // Before-paint synchronization may skip easing, but it must preserve the
  // same no-reversal invariant. A shrink can be unclamped in test geometry or
  // for one browser layout phase; never issue an explicit upward correction.
  return snapBeforePaint
    ? Math.max(currentScrollTop, targetScrollTop)
    : resolveSessionBottomFollowScrollTop(
        currentScrollTop,
        targetScrollTop,
        frameDurationMs,
      );
}

export function resolveSessionBottomFollowPersistedScrollTop({
  behavior,
  observedScrollTop,
  writeScrollTop,
  wroteScrollTop,
}: {
  behavior: ScrollBehavior;
  observedScrollTop: number;
  writeScrollTop: number;
  wroteScrollTop: boolean;
}) {
  // `scrollTo({ behavior: "smooth" })` returns before the browser reaches its
  // target, so an immediate `scrollTop` read is the pre-animation position.
  // While bottom-follow owns the viewport, persist its requested destination;
  // native scroll events will replace it with settled geometry as motion runs.
  return wroteScrollTop && behavior === "smooth"
    ? writeScrollTop
    : observedScrollTop;
}

export function resolveLatestTurnOutputState(
  messages: readonly Message[],
): LatestTurnOutputState {
  let hasAgentOutput = false;

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message) {
      continue;
    }
    if (message.author === "you") {
      return {
        hasAgentOutput,
        promptMessageId: message.id,
      };
    }
    if (message.author === "assistant") {
      hasAgentOutput = true;
    }
  }

  return {
    hasAgentOutput,
    promptMessageId: null,
  };
}

export function resolveLatestTurnTailSignature(
  messages: readonly Message[],
) {
  let promptMessageId: string | null = null;
  let tailMessage: Message | undefined;

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message) {
      continue;
    }
    tailMessage ??= message;
    if (message.author === "you") {
      promptMessageId = message.id;
      break;
    }
  }

  // Deliberately exclude the resident-window length. Trimming old messages
  // must not look like new output for a post-live turn; an appended/reparsed
  // tail still changes the last message marker, and a new prompt changes its
  // identity even before the agent responds.
  return [
    promptMessageId ?? "no-prompt",
    buildMessageListSignature(tailMessage ? [tailMessage] : []),
  ].join("|");
}

export function isFirstAgentOutputForObservedPrompt(
  previous: LatestTurnOutputState | undefined,
  current: LatestTurnOutputState,
) {
  return (
    previous !== undefined &&
    previous.promptMessageId !== null &&
    previous.promptMessageId === current.promptMessageId &&
    !previous.hasAgentOutput &&
    current.hasAgentOutput
  );
}

export function resolvePostLiveMessageFollowTransition({
  awaitingPromptMessageId,
  currentLiveFlowActive,
  currentPromptMessageId,
  latestTurnContentChanged,
  previousLiveFlowActive,
}: {
  awaitingPromptMessageId: string | null | undefined;
  currentLiveFlowActive: boolean;
  currentPromptMessageId: string | null;
  latestTurnContentChanged: boolean;
  previousLiveFlowActive: boolean;
}) {
  const liveFlowJustEnded = previousLiveFlowActive && !currentLiveFlowActive;
  if (currentLiveFlowActive) {
    return {
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: false,
    };
  }

  const originatingPromptMessageId = liveFlowJustEnded
    ? currentPromptMessageId
    : awaitingPromptMessageId;
  if (originatingPromptMessageId === undefined) {
    return {
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: false,
    };
  }

  if (!latestTurnContentChanged) {
    return {
      awaitingPostLivePromptMessageId: originatingPromptMessageId,
      shouldFollowPostLiveMessage: false,
    };
  }

  return {
    awaitingPostLivePromptMessageId: undefined,
    shouldFollowPostLiveMessage:
      originatingPromptMessageId === currentPromptMessageId,
  };
}
