// Owns: deciding whether a resident transcript window reaches the live tail.
// Does not own: streaming evidence, session activity, paging, or scroll writes.
// New module: isolates the slot's predicate; the staged streaming classifier
// keeps its existing copy until a separate ownership-safe move can reuse this.
import type { Session } from "./types";

export function isSessionAtLiveTail(
  session: Pick<Session, "hasNewerHistory" | "messageCount" | "messageStartIndex" | "messages">,
): boolean {
  return session.hasNewerHistory !== true && !(
    typeof session.messageCount === "number" &&
    (session.messageStartIndex ?? 0) + session.messages.length < session.messageCount
  );
}
