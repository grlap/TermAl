import type {
  CommandMessage,
  DeltaEvent,
  Message,
  ParallelAgentsMessage,
  Session,
  TextMessage,
} from "./types";
import { reconcileSessions } from "./session-reconcile";

export const LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS = 15000;
export const LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS = 5000;
// Keep watchdog retries aligned with one full stale-transport window for now.
export const LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS =
  LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS;

function isResolvedInteractionTurnBoundary(message: Message) {
  switch (message.type) {
    case "approval":
      return message.decision !== "pending";
    case "userInputRequest":
    case "mcpElicitationRequest":
    case "codexAppRequest":
      return message.state !== "pending";
    default:
      return false;
  }
}

function hasAssistantActivitySinceCurrentTurnBoundary(session: Session) {
  // Resolved interaction cards are assistant-authored, but they hand control back
  // to the user until newer assistant output arrives.
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const message = session.messages[index];
    if (isResolvedInteractionTurnBoundary(message)) {
      return false;
    }
    if (message.author === "assistant") {
      return true;
    }
    if (message.author === "you") {
      return false;
    }
  }

  return false;
}

export function sessionHasPotentiallyStaleTransport(
  session: Session,
  lastLiveTransportActivityAt: number | undefined,
  now: number,
) {
  return (
    session.status === "active" &&
    hasAssistantActivitySinceCurrentTurnBoundary(session) &&
    lastLiveTransportActivityAt !== undefined &&
    now - lastLiveTransportActivityAt >=
      LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS
  );
}

// Mutates the effect-local transport-activity map in place.
export function pruneLiveTransportActivitySessions(
  liveTransportActivityAtBySessionId: Map<string, number>,
  sessions: Session[],
) {
  const activeSessionIds = new Set(sessions.map((session) => session.id));
  for (const sessionId of liveTransportActivityAtBySessionId.keys()) {
    if (!activeSessionIds.has(sessionId)) {
      liveTransportActivityAtBySessionId.delete(sessionId);
    }
  }
}

type SessionDeltaEvent = Exclude<
  DeltaEvent,
  { type: "codexUpdated" } | { type: "orchestratorsUpdated" }
>;
type MessageCreatedDelta = Extract<
  SessionDeltaEvent,
  { type: "messageCreated" }
>;
type MessageUpdatedDelta = Extract<
  SessionDeltaEvent,
  { type: "messageUpdated" }
>;

export type DeltaApplyResult =
  | { kind: "applied"; sessions: Session[] }
  | { kind: "needsResync" };

function resolveSessionMutationStamp(
  session: Session,
  sessionMutationStamp: number | null | undefined,
) {
  return sessionMutationStamp ?? session.sessionMutationStamp ?? null;
}

function isValidMessageIndex(messageIndex: number) {
  return Number.isSafeInteger(messageIndex) && messageIndex >= 0;
}

function isValidMessageCount(messageCount: number) {
  return Number.isSafeInteger(messageCount) && messageCount >= 0;
}

function applyMetadataOnlySessionDelta(
  session: Session,
  delta: Exclude<SessionDeltaEvent, { type: "sessionCreated" }>,
): Session {
  const pendingPrompts =
    delta.type === "messageCreated"
      ? removePendingPromptById(session.pendingPrompts, delta.messageId)
      : session.pendingPrompts;
  const base = {
    ...session,
    messageCount: delta.messageCount,
    pendingPrompts,
    sessionMutationStamp: resolveSessionMutationStamp(
      session,
      delta.sessionMutationStamp,
    ),
  };

  switch (delta.type) {
    case "messageCreated":
    case "messageUpdated":
      return {
        ...base,
        preview: delta.preview,
        status: delta.status,
      };
    case "textDelta":
    case "textReplace":
      return {
        ...base,
        preview: delta.preview ?? session.preview,
      };
    case "commandUpdate":
    case "parallelAgentsUpdate":
      return {
        ...base,
        preview: delta.preview,
      };
  }
}

// Protocol-shape violations must trigger a full resync before the
// unhydrated metadata-only fallback runs: invalid ids/indexes, invalid or
// regressing counts, and new messages whose count does not advance are all
// inconsistent with a monotonic transcript.
function messageCreatedDeltaHasProtocolViolation(
  session: Session,
  delta: MessageCreatedDelta,
) {
  if (
    delta.message.id !== delta.messageId ||
    !isValidMessageIndex(delta.messageIndex) ||
    !isValidMessageCount(delta.messageCount)
  ) {
    return true;
  }

  const knownMessageCount = Math.max(
    session.messageCount ?? 0,
    session.messages.length,
  );
  if (delta.messageCount < knownMessageCount) {
    return true;
  }

  const existingMessageIndex = findMessageIndex(
    session.messages,
    delta.messageId,
    delta.messageIndex,
  );
  return (
    existingMessageIndex === -1 &&
    delta.messageCount <= session.messages.length
  );
}

function messageUpdatedDeltaHasProtocolViolation(
  session: Session,
  delta: MessageUpdatedDelta,
) {
  if (
    delta.message.id !== delta.messageId ||
    !isValidMessageIndex(delta.messageIndex) ||
    !isValidMessageCount(delta.messageCount)
  ) {
    return true;
  }

  const knownMessageCount = Math.max(
    session.messageCount ?? 0,
    session.messages.length,
  );
  return delta.messageCount < knownMessageCount;
}

function applyMessageCreatedDeltaToRetainedTranscript(
  session: Session,
  delta: MessageCreatedDelta,
): Session | null {
  const updatedMessages = session.messages.slice();
  const existingMessageIndex = findMessageIndex(
    updatedMessages,
    delta.messageId,
    delta.messageIndex,
  );
  if (existingMessageIndex !== -1) {
    updatedMessages.splice(existingMessageIndex, 1);
  }
  if (delta.messageIndex > updatedMessages.length) {
    return null;
  }

  updatedMessages.splice(delta.messageIndex, 0, delta.message);
  const pendingPrompts = removePendingPromptById(
    session.pendingPrompts,
    delta.messageId,
  );

  return {
    ...session,
    messages: updatedMessages,
    messagesLoaded:
      updatedMessages.length === delta.messageCount
        ? true
        : session.messagesLoaded,
    messageCount: delta.messageCount,
    pendingPrompts,
    preview: delta.preview,
    status: delta.status,
    sessionMutationStamp: resolveSessionMutationStamp(
      session,
      delta.sessionMutationStamp,
    ),
  };
}

export function applyDeltaToSessions(
  sessions: Session[],
  delta: SessionDeltaEvent,
): DeltaApplyResult {
  const sessionIndex = sessions.findIndex(
    (session) => session.id === delta.sessionId,
  );

  switch (delta.type) {
    case "sessionCreated": {
      if (delta.session.id !== delta.sessionId) {
        return { kind: "needsResync" };
      }

      if (sessionIndex === -1) {
        return {
          kind: "applied",
          sessions: [...sessions, delta.session],
        };
      }

      return {
        kind: "applied",
        sessions: replaceSession(
          sessions,
          sessionIndex,
          reconcileSessions([sessions[sessionIndex]], [delta.session], {
            disableMutationStampFastPath: true,
          })[0],
        ),
      };
    }

    case "messageCreated": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (messageCreatedDeltaHasProtocolViolation(session, delta)) {
        return { kind: "needsResync" };
      }

      if (session.messagesLoaded === false) {
        const retainedTranscriptUpdate =
          applyMessageCreatedDeltaToRetainedTranscript(session, delta);
        if (retainedTranscriptUpdate) {
          return {
            kind: "applied",
            sessions: replaceSession(
              sessions,
              sessionIndex,
              retainedTranscriptUpdate,
            ),
          };
        }

        return {
          kind: "applied",
          sessions: replaceSession(
            sessions,
            sessionIndex,
            applyMetadataOnlySessionDelta(session, delta),
          ),
        };
      }

      const retainedTranscriptUpdate =
        applyMessageCreatedDeltaToRetainedTranscript(session, delta);
      if (
        !retainedTranscriptUpdate ||
        retainedTranscriptUpdate.messages.length !== delta.messageCount
      ) {
        return { kind: "needsResync" };
      }

      return {
        kind: "applied",
        sessions: replaceSession(
          sessions,
          sessionIndex,
          retainedTranscriptUpdate,
        ),
      };
    }

    case "messageUpdated": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (messageUpdatedDeltaHasProtocolViolation(session, delta)) {
        return { kind: "needsResync" };
      }

      if (session.messagesLoaded === false) {
        return {
          kind: "applied",
          sessions: replaceSession(
            sessions,
            sessionIndex,
            applyMetadataOnlySessionDelta(session, delta),
          ),
        };
      }

      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        return { kind: "needsResync" };
      }

      const updatedMessages = session.messages.slice();
      // In-place replacement only; unlike messageCreated this must not reorder.
      updatedMessages[messageIndex] = delta.message;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          messageCount: delta.messageCount,
          preview: delta.preview,
          status: delta.status,
          sessionMutationStamp: resolveSessionMutationStamp(
            session,
            delta.sessionMutationStamp,
          ),
        }),
      };
    }

    case "textDelta": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (session.messagesLoaded === false) {
        return {
          kind: "applied",
          sessions: replaceSession(
            sessions,
            sessionIndex,
            applyMetadataOnlySessionDelta(session, delta),
          ),
        };
      }
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        return { kind: "needsResync" };
      }

      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return { kind: "needsResync" };
      }
      if (message.type !== "text") {
        return { kind: "needsResync" };
      }

      const updatedMessage: TextMessage = {
        ...message,
        text: message.text + delta.delta,
      };
      const updatedMessages = session.messages.slice();
      updatedMessages[messageIndex] = updatedMessage;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          messageCount: delta.messageCount,
          preview: delta.preview ?? session.preview,
          sessionMutationStamp: resolveSessionMutationStamp(
            session,
            delta.sessionMutationStamp,
          ),
        }),
      };
    }

    case "textReplace": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (session.messagesLoaded === false) {
        return {
          kind: "applied",
          sessions: replaceSession(
            sessions,
            sessionIndex,
            applyMetadataOnlySessionDelta(session, delta),
          ),
        };
      }
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        return { kind: "needsResync" };
      }

      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return { kind: "needsResync" };
      }
      if (message.type !== "text") {
        return { kind: "needsResync" };
      }

      const updatedMessage: TextMessage = {
        ...message,
        text: delta.text,
      };
      const updatedMessages = session.messages.slice();
      updatedMessages[messageIndex] = updatedMessage;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          messageCount: delta.messageCount,
          preview: delta.preview ?? session.preview,
          sessionMutationStamp: resolveSessionMutationStamp(
            session,
            delta.sessionMutationStamp,
          ),
        }),
      };
    }

    case "commandUpdate": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (session.messagesLoaded === false) {
        return {
          kind: "applied",
          sessions: replaceSession(
            sessions,
            sessionIndex,
            applyMetadataOnlySessionDelta(session, delta),
          ),
        };
      }
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        return { kind: "needsResync" };
      }

      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return { kind: "needsResync" };
      }
      if (message.type !== "command") {
        return { kind: "needsResync" };
      }

      const updatedMessage: CommandMessage = {
        ...message,
        command: delta.command,
        commandLanguage: delta.commandLanguage,
        output: delta.output,
        outputLanguage: delta.outputLanguage,
        status: delta.status,
      };
      const updatedMessages = session.messages.slice();
      updatedMessages[messageIndex] = updatedMessage;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          messageCount: delta.messageCount,
          preview: delta.preview,
          sessionMutationStamp: resolveSessionMutationStamp(
            session,
            delta.sessionMutationStamp,
          ),
        }),
      };
    }

    case "parallelAgentsUpdate": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (session.messagesLoaded === false) {
        return {
          kind: "applied",
          sessions: replaceSession(
            sessions,
            sessionIndex,
            applyMetadataOnlySessionDelta(session, delta),
          ),
        };
      }
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        return { kind: "needsResync" };
      }

      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return { kind: "needsResync" };
      }
      if (message.type !== "parallelAgents") {
        return { kind: "needsResync" };
      }

      const updatedMessage: ParallelAgentsMessage = {
        ...message,
        agents: delta.agents,
      };
      const updatedMessages = session.messages.slice();
      updatedMessages[messageIndex] = updatedMessage;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          messageCount: delta.messageCount,
          preview: delta.preview,
          sessionMutationStamp: resolveSessionMutationStamp(
            session,
            delta.sessionMutationStamp,
          ),
        }),
      };
    }
    default: {
      const _exhaustive: never = delta;
      void _exhaustive;
      return { kind: "needsResync" };
    }
  }
}

function replaceSession(sessions: Session[], index: number, session: Session) {
  const updatedSessions = sessions.slice();
  updatedSessions[index] = session;
  return updatedSessions;
}

function removePendingPromptById(
  pendingPrompts: Session["pendingPrompts"],
  promptId: string,
): Session["pendingPrompts"] {
  if (!pendingPrompts?.length) {
    return pendingPrompts;
  }

  const nextPendingPrompts = pendingPrompts.filter(
    (prompt) => prompt.id !== promptId,
  );
  if (nextPendingPrompts.length === pendingPrompts.length) {
    return pendingPrompts;
  }

  return nextPendingPrompts.length > 0 ? nextPendingPrompts : undefined;
}

function findMessageIndex(
  messages: Session["messages"],
  messageId: string,
  preferredIndex: number,
) {
  const preferredMessage = messages[preferredIndex];
  if (preferredMessage?.id === messageId) {
    return preferredIndex;
  }

  return messages.findIndex((message) => message.id === messageId);
}
