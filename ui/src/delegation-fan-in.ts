// Owns presentation-only detection and labeling for delegation fan-in prompt
// text. It deliberately does not render messages or manage delegation waits.
// Split out of `message-cards.tsx`.

import type { TextMessage } from "./types";

export const DELEGATION_FAN_IN_AUTHOR_LABEL = "Fan-in";

// Whether a "you" text message is a delegation fan-in prompt — the block that
// `termal_resume_after_delegations` queues into the parent session when delegated
// children finish.
//
// Detected by the fan-in template's three structural anchors (see
// `build_delegation_wait_resume_prompt` in `src/delegations.rs`), not by a title
// prefix, so it keeps working if the title wording changes. The detection only
// changes presentation: false positives alter attribution/collapse behavior, and
// false negatives leave the ordinary user-message presentation in place.
export function isDelegationFanInText(text: string): boolean {
  return (
    text.includes("\nWait id: `") &&
    text.includes("\nDelegations:\n") &&
    text.includes("\nResults:\n")
  );
}

// Source of truth shared by the renderer and virtualizer estimator. A peer
// message may contain the same protocol-shaped text, but it keeps its peer
// presentation instead of using the default-collapsed fan-in card.
export function shouldCollapseDelegationFanInMessage(
  message: Pick<TextMessage, "author" | "source" | "text">,
): boolean {
  return (
    message.author === "you" &&
    !message.source &&
    isDelegationFanInText(message.text)
  );
}
