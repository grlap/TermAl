// Verifies actual attached-tail body placement through the master reducer.
// Reducer success can mean metadata only; that is not successful body replay.
import { applyDeltaToSessions } from "./live-updates";
import type { Session } from "./types";
import type { BodyDelta } from "./transcript-body-sequence";

export type BodyRange = { readonly start: number; readonly end: number; readonly reachedTail: boolean };
export function bodyRange(session: Session): BodyRange {
  const start = session.messageStartIndex ?? Math.max(0,
    (session.messageCount ?? session.messages.length) - session.messages.length);
  return { start, end: start + session.messages.length, reachedTail: session.hasNewerHistory !== true };
}

export function applyBodyDeltaToRange(session: Session, delta: BodyDelta): Session | null {
  const range = bodyRange(session);
  const count = session.messageCount ?? session.messages.length;
  const index = delta.messageIndex;
  if (!Number.isSafeInteger(index) || index < 0 || !Number.isSafeInteger(delta.messageCount)) return null;
  const local = index - range.start;
  const inside = index >= range.start && index < range.end;
  const existing = session.messages.findIndex(message => message.id === delta.messageId);
  let fresh = false;
  if (delta.type === "messageCreated") {
    if (delta.message.id !== delta.messageId) return null;
    fresh = delta.messageCount === count + 1 && existing === -1;
    // An existing id outside this window has an unknown old position. A
    // count/index mismatch cannot be turned into proof by metadata fallback.
    if (index > count || (!fresh && (delta.messageCount !== count || existing === -1))) return null;
  } else if (delta.messageCount !== count || index >= count ||
      (inside ? session.messages[local]?.id !== delta.messageId : existing !== -1)) return null;

  const result = applyDeltaToSessions([session], delta);
  if (result.kind === "needsResync" || (result.kind === "appliedNeedsResync" && inside)) return null;
  const next = result.sessions[0];
  if (delta.type === "messageCreated") {
    const affectsBodies = !fresh || index < range.end || (index === range.end && range.reachedTail);
    if (affectsBodies) {
      // Verify placement, including every shifted identity, rather than
      // trusting `applied` from the detached metadata-only reducer branch.
      const expected = session.messages.slice();
      if (!fresh) expected.splice(existing, 1);
      const nextStart = range.start + (fresh && index < range.start ? 1 : 0);
      if (index >= nextStart && index <= nextStart + expected.length) {
        expected.splice(index - nextStart, 0, delta.message);
      }
      if (bodyRange(next).start !== nextStart || next.messages.length !== expected.length ||
          next.messages.some((message, i) => message.id !== expected[i].id) ||
          (index >= nextStart && next.messages[index - nextStart] !== delta.message)) return null;
    }
  } else if (inside && next.messages[local]?.id !== delta.messageId) return null;
  return next;
}
