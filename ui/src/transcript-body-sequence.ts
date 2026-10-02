// Per-session body order is independent of the broad state's revision. Reads
// replay only a contiguous suffix from their own snapshot; missing evidence
// refuses admission rather than certifying an unchanged-looking window.
import type { SessionDeltaEvent } from "./live-updates";
import type { DeltaEvent, Session } from "./types";

export type BodyDelta = Extract<SessionDeltaEvent, { type:
  "messageCreated" | "messageUpdated" | "textDelta" | "textReplace" |
  "commandUpdate" | "parallelAgentsUpdate" | "testRunCardUpdated" }>;

export function isBodyDelta(delta: DeltaEvent): delta is BodyDelta {
  return delta.type === "messageCreated" || delta.type === "messageUpdated" ||
    delta.type === "textDelta" || delta.type === "textReplace" ||
    delta.type === "commandUpdate" || delta.type === "parallelAgentsUpdate" ||
    delta.type === "testRunCardUpdated";
}

export function validBodySequence(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

export function bodySequenceProof(value: { bodySeq?: number; sessionSeq?: number; bodySeqEpoch?: string }):
    { seq: number; epoch: string } | null {
  const seq = value.bodySeq ?? value.sessionSeq;
  return validBodySequence(seq) && typeof value.bodySeqEpoch === "string"
    ? { seq, epoch: value.bodySeqEpoch } : null;
}

export const MAX_KEPT_BODY_DELTAS = 256;
