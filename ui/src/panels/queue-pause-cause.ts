// Names ordinary queue pauses without interpreting stored previews as live
// scheduling or control authority. Queue ownership stays with the backend.
import type { Session } from "../types";

export type QueuePauseCause =
  | { kind: "stopped" }
  | { kind: "other"; preview: string };

export function resolveQueuePauseCause(preview: string): QueuePauseCause {
  return preview === "Turn stopped by user."
    ? { kind: "stopped" }
    : { kind: "other", preview };
}

export function queuePausedCardText(
  cause: QueuePauseCause,
  agent: Session["agent"],
  queuedCount: number,
): { heading: string; detail: string | null; waiting: string } {
  const waitingLabel = queuedCount === 1 ? "1 prompt waiting" : `${queuedCount} prompts waiting`;
  return {
    heading: cause.kind === "stopped"
      ? `${agent} was stopped; the queue is paused`
      : "The queue is paused",
    detail: cause.kind === "other" ? cause.preview || null : null,
    waiting: `${waitingLabel}. Send a new prompt or resume the queue to continue.`,
  };
}
