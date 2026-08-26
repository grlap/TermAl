// Owns: pure accumulation and classification of transcript content changes
// between the last pane-consumed signature and the current render.
// Does not own: React lifecycle, scroll intent, indicator state, or DOM writes.
// Split from: ui/src/SessionPaneView.scroll.ts.

import { messageChangeMarker } from "./app-utils";
import type { Message } from "./types";

export type TurnContentTransition = {
  fromMessageContentSignature: string | undefined;
  hasLiveLatestTurnChange: boolean;
  pendingPromptsAdvanced: boolean;
  tailMessageChanged: boolean;
  toMessageContentSignature: string;
};

type BuildTurnContentTransitionOptions = {
  lastConsumedMessageContentSignature: string | undefined;
  latestTurnChangedBeyondPromptResidency: boolean;
  pendingPromptsAdvanced: boolean;
  previousMessageContentSignature: string | undefined;
  previousTransition: TurnContentTransition | undefined;
  tailMessageChanged: boolean;
  toMessageContentSignature: string;
};

export function didLatestTurnContentChangeBeyondPromptResidency({
  currentMessages,
  currentPromptMessageId,
  latestTurnChanged,
  previousMessages,
  previousPromptMessageId,
  promptResidencyChanged,
}: {
  currentMessages: readonly Message[];
  currentPromptMessageId: string | null;
  latestTurnChanged: boolean;
  previousMessages: readonly Message[] | undefined;
  previousPromptMessageId: string | null | undefined;
  promptResidencyChanged: boolean;
}) {
  if (!latestTurnChanged) {
    return false;
  }
  if (!promptResidencyChanged) {
    return true;
  }
  if (!previousMessages) {
    return false;
  }

  const unchangedSuffix = (
    shorter: readonly Message[],
    longer: readonly Message[],
  ) => {
    if (shorter.length > longer.length) {
      return false;
    }
    const offset = longer.length - shorter.length;
    return shorter.every((message, index) => {
      const candidate = longer[offset + index];
      return (
        candidate?.id === message.id &&
        messageChangeMarker(candidate) === messageChangeMarker(message)
      );
    });
  };

  // A pure resident-history reveal/trim changes only the prefix around the
  // prompt boundary: the previously visible latest-turn suffix remains
  // unchanged. Any mutation or insertion inside that shared suffix is genuine
  // live progress. This comparison is deliberately paid only on that rare
  // compound boundary transition, not on every streamed delta.
  if (previousPromptMessageId === null && currentPromptMessageId !== null) {
    const currentPromptIndex = currentMessages.findIndex(
      (message) => message.id === currentPromptMessageId,
    );
    if (currentPromptIndex < 0) {
      return true;
    }
    return !unchangedSuffix(
      previousMessages,
      currentMessages.slice(currentPromptIndex + 1),
    );
  }
  if (previousPromptMessageId !== null && currentPromptMessageId === null) {
    const previousPromptIndex = previousMessages.findIndex(
      (message) => message.id === previousPromptMessageId,
    );
    if (previousPromptIndex < 0) {
      return true;
    }
    return !unchangedSuffix(
      currentMessages,
      previousMessages.slice(previousPromptIndex + 1),
    );
  }

  // The caller's residency predicate promises exactly one null prompt id. If
  // that invariant drifts, fail open to visible activity rather than hiding a
  // genuine update.
  return true;
}

export function buildTurnContentTransition({
  lastConsumedMessageContentSignature,
  latestTurnChangedBeyondPromptResidency,
  pendingPromptsAdvanced,
  previousMessageContentSignature,
  previousTransition,
  tailMessageChanged,
  toMessageContentSignature,
}: BuildTurnContentTransitionOptions): TurnContentTransition {
  const extendsUnconsumedTransition =
    previousTransition !== undefined &&
    previousTransition.fromMessageContentSignature ===
      lastConsumedMessageContentSignature &&
    previousTransition.toMessageContentSignature ===
      previousMessageContentSignature;

  return {
    fromMessageContentSignature: extendsUnconsumedTransition
      ? previousTransition.fromMessageContentSignature
      : previousMessageContentSignature,
    // A prompt crossing the resident-history boundary can change the latest
    // turn signature without representing live activity. Keep that narrow
    // explanation separate from genuine latest-turn mutations so an earlier
    // history change cannot mask later command/output progress while the pane
    // is inactive and the transition remains unconsumed.
    hasLiveLatestTurnChange:
      latestTurnChangedBeyondPromptResidency ||
      (extendsUnconsumedTransition &&
        previousTransition.hasLiveLatestTurnChange),
    pendingPromptsAdvanced:
      pendingPromptsAdvanced ||
      (extendsUnconsumedTransition &&
        previousTransition.pendingPromptsAdvanced),
    tailMessageChanged:
      tailMessageChanged ||
      (extendsUnconsumedTransition && previousTransition.tailMessageChanged),
    toMessageContentSignature,
  };
}

export type TurnContentTransitionKind =
  | "live"
  | "pendingPromptsAdvanced"
  | "residentHistoryOnly";

export function classifyTurnContentTransition({
  currentMessageContentSignature,
  previousMessageContentSignature,
  showWaitingIndicator,
  transition,
}: {
  currentMessageContentSignature: string;
  previousMessageContentSignature: string | undefined;
  showWaitingIndicator: boolean;
  transition: TurnContentTransition | undefined;
}): TurnContentTransitionKind {
  const describesConsumedMessageTransition =
    transition !== undefined &&
    transition.fromMessageContentSignature ===
      previousMessageContentSignature &&
    transition.toMessageContentSignature === currentMessageContentSignature;

  if (
    showWaitingIndicator &&
    describesConsumedMessageTransition &&
    !transition.tailMessageChanged &&
    transition.pendingPromptsAdvanced
  ) {
    return "pendingPromptsAdvanced";
  }

  if (
    previousMessageContentSignature !== currentMessageContentSignature &&
    describesConsumedMessageTransition &&
    !transition.tailMessageChanged &&
    !transition.hasLiveLatestTurnChange
  ) {
    return "residentHistoryOnly";
  }

  return "live";
}
