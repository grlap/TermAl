// Owns: naming why a session's queue is paused, from the session preview the
// backend writes when it pauses the queue, and the card text for that cause.
// Does not own: whether the queue is paused, which cards appear, their
// actions, or anything that holds or releases the queue.
// New module; its text was fixed inline in session-activity-cards.tsx and
// AgentSessionPanel.tsx before.
//
// The backend has no typed pause cause yet, so the cause is read from the
// preview strings it writes: the automatic-retry preview of a parked Engram
// admission, the Stop preview, and Engram's own hold previews.

import type { Session } from "../types";

/** The start of the preview of a parked Engram admission whose automatic
 * re-admission is scheduled (backend `ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX`). */
const ENGRAM_RETRY_PREVIEW_PREFIX = "Engram: waiting for admission; retrying automatically";

/** The whole retry preview: `<prefix>, attempt N, next at <RFC 3339>.` */
const ENGRAM_RETRY_PREVIEW = new RegExp(
  `^${ENGRAM_RETRY_PREVIEW_PREFIX}, attempt (\\d+), next at (\\S+)\\.$`,
);

/** The preview a real Stop leaves (backend `SESSION_STOPPED_BY_USER_MESSAGE`). */
const STOPPED_BY_USER_PREVIEW = "Turn stopped by user.";

export type QueuePauseCause =
  /** Engram retries the parked admission on its own. `retry` is absent when
   * the preview does not carry a readable attempt and time. */
  | { kind: "engramRetrying"; preview: string; retry: { attempt: number; nextAt: string } | null }
  /** Engram holds the queue until someone resumes or cancels. */
  | { kind: "engramHeld"; preview: string }
  /** Someone stopped the agent. */
  | { kind: "stopped" }
  /** Anything else; the preview, when there is one, names it. */
  | { kind: "other"; preview: string };

export function resolveQueuePauseCause(preview: string): QueuePauseCause {
  if (preview.startsWith(ENGRAM_RETRY_PREVIEW_PREFIX)) {
    return { kind: "engramRetrying", preview, retry: parseEngramRetry(preview) };
  }
  if (preview.startsWith("Engram")) {
    return { kind: "engramHeld", preview };
  }
  if (preview === STOPPED_BY_USER_PREVIEW) {
    return { kind: "stopped" };
  }
  return { kind: "other", preview };
}

function parseEngramRetry(preview: string): { attempt: number; nextAt: string } | null {
  const match = ENGRAM_RETRY_PREVIEW.exec(preview);
  if (!match) {
    return null;
  }
  const attempt = Number(match[1]);
  const due = new Date(match[2]);
  if (!Number.isSafeInteger(attempt) || Number.isNaN(due.getTime())) {
    return null;
  }
  return { attempt, nextAt: due.toLocaleTimeString() };
}

/** The paused-queue card's heading, the cause line under it (if any), and the
 * line with the waiting prompts. A cause that needs nobody asks for nothing. */
export function queuePausedCardText(
  cause: QueuePauseCause,
  agent: Session["agent"],
  queuedCount: number,
): { heading: string; detail: string | null; waiting: string } {
  const waitingLabel =
    queuedCount === 1 ? "1 prompt waiting" : `${queuedCount} prompts waiting`;
  const resumeHint = `${waitingLabel}. Send a new prompt or resume the queue to continue.`;
  switch (cause.kind) {
    case "engramRetrying":
      return {
        heading: "Waiting for Engram admission; retrying automatically",
        detail: cause.retry
          ? `Attempt ${cause.retry.attempt}, next at ${cause.retry.nextAt}. No action is needed.`
          : `${cause.preview} No action is needed.`,
        waiting: `${waitingLabel}.`,
      };
    case "engramHeld":
      return {
        heading: "Engram is holding the queue",
        detail: cause.preview,
        waiting: `${waitingLabel}.`,
      };
    case "stopped":
      return {
        heading: `${agent} was stopped; the queue is paused`,
        detail: null,
        waiting: resumeHint,
      };
    case "other":
      return {
        heading: "The queue is paused",
        detail: cause.preview || null,
        waiting: resumeHint,
      };
  }
}

/** The retained-prompt card's text for a retained head that is not
 * interrupted. */
export function retainedPromptHoldText(cause: QueuePauseCause): string {
  if (cause.kind === "engramRetrying") {
    return cause.retry
      ? `Engram admission is retrying automatically (attempt ${cause.retry.attempt}, next at ${cause.retry.nextAt}). Cancel drops the retained prompt.`
      : `${cause.preview} Cancel drops the retained prompt.`;
  }
  if (cause.kind === "engramHeld") {
    return cause.preview;
  }
  return "Authorization is held. Resume to retry, or cancel the retained prompt.";
}
