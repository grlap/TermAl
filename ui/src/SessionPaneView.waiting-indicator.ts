// Owns the small waiting-indicator helpers split from SessionPaneView.
// Does not own pane rendering, scrolling, or footer wiring.
// Split from ui/src/SessionPaneView.tsx.

import type { DelegationWaitRecord } from "./api";
import type { Message } from "./types";

export function delegationWaitIndicatorPrompt(
  waits: readonly DelegationWaitRecord[],
) {
  if (waits.length === 0) {
    return null;
  }
  const first = waits[0];
  const childCount = waits.reduce(
    (count, wait) => count + wait.delegationIds.length,
    0,
  );
  const childLabel =
    childCount === 1 ? "1 delegated session" : `${childCount} delegated sessions`;
  const mode = first.mode === "any" ? "any" : "all";
  const title = first.title?.trim();
  if (waits.length === 1) {
    return title
      ? `Waiting for ${mode} of ${childLabel}: ${title}`
      : `Waiting for ${mode} of ${childLabel}`;
  }
  const firstTitle = waits
    .map((wait) => wait.title?.trim() ?? "")
    .find((candidate) => candidate.length > 0);
  const titleSuffix = firstTitle
    ? `: ${firstTitle} (+${waits.length - 1} more)`
    : "";
  return `Waiting on ${waits.length} delegation waits covering ${childLabel}${titleSuffix}`;
}

export function hasAgentOutputAfterLatestUserPrompt(messages: readonly Message[]) {
  let sawLatestUserPrompt = false;
  let sawAgentOutputAfterLatestUserPrompt = false;

  for (const message of messages) {
    if (message.author === "you") {
      sawLatestUserPrompt = true;
      sawAgentOutputAfterLatestUserPrompt = false;
      continue;
    }

    if (sawLatestUserPrompt && message.author === "assistant") {
      sawAgentOutputAfterLatestUserPrompt = true;
    }
  }

  return sawAgentOutputAfterLatestUserPrompt;
}

export function hasTurnFinalizingOutputAfterLatestUserPrompt(
  messages: readonly Message[],
) {
  let sawLatestUserPrompt = false;
  let sawTurnFinalizingOutputAfterLatestUserPrompt = false;

  for (const message of messages) {
    if (message.author === "you") {
      sawLatestUserPrompt = true;
      sawTurnFinalizingOutputAfterLatestUserPrompt = false;
      continue;
    }

    if (
      sawLatestUserPrompt &&
      message.author === "assistant" &&
      message.type === "fileChanges"
    ) {
      sawTurnFinalizingOutputAfterLatestUserPrompt = true;
    }
  }

  return sawTurnFinalizingOutputAfterLatestUserPrompt;
}
