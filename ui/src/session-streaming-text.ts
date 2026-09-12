// Streaming presentation evidence attached to published session snapshots.
// Resolver extracted from SessionPaneView.render-callbacks.tsx and extended
// here with completion evidence from session-store publications.
// A session becoming active is not evidence that its previous answer restarted.
// This is client-only state, not a wire field or a persisted turn identifier.
// On a fresh connection that first observes an already-active session there is
// no completion evidence: retain the live-tail fallback. Distinguishing turns
// in that case requires a server-provided message/turn identity, not a queue or
// timestamp guess. Values below contain strings only, never previous snapshots.
import type { Session, TextMessage } from "./types";

type SettledText = Readonly<{ id: string; text: string }>;
let settledTextBySnapshot = new WeakMap<Session, SettledText | null>();

function assistantTextAtLiveTail(session: Session): TextMessage | null {
  // The last resident message need not be the last transcript message.
  if (session.hasNewerHistory === true ||
      (typeof session.messageCount === "number" &&
        (session.messageStartIndex ?? 0) + session.messages.length < session.messageCount)) {
    return null;
  }
  const last = session.messages[session.messages.length - 1];
  return last?.author === "assistant" && last.type === "text" ? last : null;
}

// Called only at the session-store publication boundary, never during render.
// Carry evidence through cloned metadata, empty hydration windows and tab
// remounts without changing Session object identity or retaining old snapshots.
export function recordSessionStreamingTextState(
  session: Session,
  previousSession?: Session,
) {
  if (session === previousSession) return;
  let settled = previousSession?.id === session.id
    ? settledTextBySnapshot.get(previousSession) ?? null
    : null;
  if (session.status === "idle" || session.status === "error") {
    const last = assistantTextAtLiveTail(session);
    if (last) settled = { id: last.id, text: last.text };
  }
  settledTextBySnapshot.set(session, settled);
}

export function streamingAssistantTextMessageIdForSession(session: Session | null) {
  if (session?.status !== "active") return null;
  const last = assistantTextAtLiveTail(session);
  if (!last) return null;
  const settled = settledTextBySnapshot.get(session);
  // A new body is allowed to stream, even when an agent reuses its message ID.
  // Pending prompts alone say nothing about whether this body is still growing.
  if (settled?.id === last.id && settled.text === last.text) return null;
  return last.id;
}

export function resetSessionStreamingTextStateForTesting() {
  settledTextBySnapshot = new WeakMap();
}
