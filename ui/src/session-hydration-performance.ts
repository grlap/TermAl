// Correlates targeted session-tail adoption with the transcript's next DOM
// commit. This is diagnostics only: it never delays rendering or changes
// hydration state, and it emits output only when the normally tiny adoption
// -> commit interval crosses the user-visible 500 ms boundary.

import type { Session } from "./types";

const SESSION_TRANSCRIPT_COMMIT_WARN_AFTER_MS = 500;
const SESSION_TRANSCRIPT_COMMIT_EXPIRE_AFTER_MS = 30_000;
const MAX_PENDING_SESSION_TRANSCRIPT_COMMITS = 64;

type PendingSessionTranscriptCommit = {
  adoptedAt: number;
  messageCount: number;
  token: number;
};

const pendingSessionTranscriptCommits = new Map<
  string,
  PendingSessionTranscriptCommit
>();
let nextSessionTranscriptCommitToken = 1;
let sessionTranscriptCommitTokens = new WeakMap<Session, number>();
const reportSessionHydrationWarning =
  import.meta.env.MODE === "test"
    ? (_message: string) => {}
    : (message: string) => console.warn(message);

export function noteSessionTailAdopted(
  session: Session,
  now = performance.now(),
) {
  const token = nextSessionTranscriptCommitToken;
  nextSessionTranscriptCommitToken += 1;
  sessionTranscriptCommitTokens.set(session, token);
  pendingSessionTranscriptCommits.delete(session.id);
  pendingSessionTranscriptCommits.set(session.id, {
    adoptedAt: now,
    messageCount: session.messages.length,
    token,
  });
  while (
    pendingSessionTranscriptCommits.size >
    MAX_PENDING_SESSION_TRANSCRIPT_COMMITS
  ) {
    const oldestSessionId = pendingSessionTranscriptCommits.keys().next().value;
    if (typeof oldestSessionId !== "string") {
      break;
    }
    pendingSessionTranscriptCommits.delete(oldestSessionId);
  }
  return token;
}

export function sessionTranscriptCommitToken(session: Session) {
  return sessionTranscriptCommitTokens.get(session) ?? null;
}

export function noteSessionTranscriptCommitted(
  sessionId: string,
  token: number,
  messageCount: number,
  now = performance.now(),
  reportWarning = reportSessionHydrationWarning,
) {
  const pending = pendingSessionTranscriptCommits.get(sessionId);
  if (!pending || pending.token !== token) {
    return null;
  }
  pendingSessionTranscriptCommits.delete(sessionId);
  const elapsedMs = Math.max(0, now - pending.adoptedAt);
  if (elapsedMs > SESSION_TRANSCRIPT_COMMIT_EXPIRE_AFTER_MS) {
    return null;
  }
  if (elapsedMs >= SESSION_TRANSCRIPT_COMMIT_WARN_AFTER_MS) {
    reportWarning(
      `session hydration> transcript commit delayed ${elapsedMs.toFixed(0)}ms ` +
        `after tail adoption for \`${sessionId}\` (${messageCount} messages)`,
    );
  }
  return elapsedMs;
}

export function cancelPendingSessionTranscriptCommit(sessionId: string) {
  pendingSessionTranscriptCommits.delete(sessionId);
}

export function __resetSessionHydrationPerformanceForTests() {
  pendingSessionTranscriptCommits.clear();
  nextSessionTranscriptCommitToken = 1;
  sessionTranscriptCommitTokens = new WeakMap<Session, number>();
}
