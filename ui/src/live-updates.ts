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

function hasInTurnActivitySinceTurnBoundary(session: Session) {
  // Walks backward from the latest message looking for evidence that an
  // agent turn is currently in progress. We return true on either:
  //   - an assistant-authored message (the agent has spoken in this turn), or
  //   - a user-authored message (the user kicked off a turn that is still
  //     waiting for the assistant — silence past the staleness threshold here
  //     is exactly the "I sent a prompt and nothing came back" wedge we want
  //     the watchdog to recover from).
  // Resolved interaction cards (approval decided, user-input answered, …) are
  // assistant-authored but they hand control back to the user until newer
  // assistant output arrives, so they end the walk with no in-turn activity.
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const message = session.messages[index];
    if (isResolvedInteractionTurnBoundary(message)) {
      return false;
    }
    if (message.author === "assistant" || message.author === "you") {
      return true;
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
    hasInTurnActivitySinceTurnBoundary(session) &&
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

export type SessionDeltaEvent = Exclude<
  DeltaEvent,
  | { type: "codexUpdated" }
  | { type: "orchestratorsUpdated" }
  | { type: "delegationCreated" }
  | { type: "delegationUpdated" }
  | { type: "delegationCompleted" }
  | { type: "delegationFailed" }
  | { type: "delegationCanceled" }
>;
type MessageCreatedDelta = Extract<
  SessionDeltaEvent,
  { type: "messageCreated" }
>;
type MessageUpdatedDelta = Extract<
  SessionDeltaEvent,
  { type: "messageUpdated" }
>;
type TranscriptDelta = Extract<
  SessionDeltaEvent,
  | { type: "messageCreated" }
  | { type: "messageUpdated" }
  | { type: "textDelta" }
  | { type: "textReplace" }
  | { type: "commandUpdate" }
  | { type: "parallelAgentsUpdate" }
>;

export type DeltaApplyResult =
  | { kind: "applied"; sessions: Session[] }
  // Same shape as "applied" but signals that the metadata patch alone is not
  // enough — the caller must also schedule an authoritative state resync. Used
  // when the unhydrated metadata-only fallback fires for a delta whose target
  // message is missing from the retained transcript: the metadata patch keeps
  // `messageCount`/`preview`/`status` fresh in the sidebar, but the message
  // body itself only arrives via targeted hydration. Without an explicit
  // resync nudge the session can stay stuck on a stale transcript if its
  // hydration is queued behind another retry or wedged on a mismatch — see
  // `docs/bugs.md`'s "Unhydrated session + missing-target delta silently
  // absorbs into metadata-only" entry.
  | { kind: "appliedNeedsResync"; sessions: Session[] }
  | { kind: "needsResync" };

function resolveSessionMutationStamp(
  session: Session,
  sessionMutationStamp: number | null | undefined,
) {
  return sessionMutationStamp ?? session.sessionMutationStamp ?? null;
}

export function sessionDeltaAdvancesCurrentMutationStamp(
  sessions: readonly Session[],
  delta: SessionDeltaEvent,
) {
  if (!("sessionMutationStamp" in delta)) {
    return false;
  }

  const nextStamp = delta.sessionMutationStamp;
  if (typeof nextStamp !== "number" || !Number.isSafeInteger(nextStamp)) {
    return false;
  }

  const session = sessions.find((entry) => entry.id === delta.sessionId);
  if (!session) {
    return false;
  }

  const currentStamp = session?.sessionMutationStamp;
  if (currentStamp === null || currentStamp === undefined) {
    return true;
  }

  return (
    typeof currentStamp === "number" &&
    Number.isSafeInteger(currentStamp) &&
    nextStamp > currentStamp
  );
}

function isValidMessageIndex(messageIndex: number) {
  return Number.isSafeInteger(messageIndex) && messageIndex >= 0;
}

function isValidMessageCount(messageCount: number) {
  return Number.isSafeInteger(messageCount) && messageCount >= 0;
}

function isValidMessageIndexForCount(
  messageIndex: number,
  messageCount: number,
) {
  return isValidMessageIndex(messageIndex) && messageIndex < messageCount;
}

function applyMetadataOnlySessionDelta(
  session: Session,
  delta: TranscriptDelta,
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
    !isValidMessageCount(delta.messageCount) ||
    !isValidMessageIndexForCount(delta.messageIndex, delta.messageCount)
  ) {
    return true;
  }

  const existingMessageIndex = findMessageIndex(
    session.messages,
    delta.messageId,
    delta.messageIndex,
  );
  const knownMessageCount = Math.max(
    session.messageCount ?? 0,
    session.messages.length,
  );
  if (delta.messageCount < knownMessageCount) {
    const currentStamp = session.sessionMutationStamp;
    const deltaStamp = delta.sessionMutationStamp;
    const isSameRevisionProgressiveSuffixCreate =
      existingMessageIndex === -1 &&
      typeof currentStamp === "number" &&
      typeof deltaStamp === "number" &&
      currentStamp === deltaStamp &&
      delta.messageIndex === session.messages.length &&
      delta.messageCount === delta.messageIndex + 1;
    const isReplayOfKnownMessage =
      existingMessageIndex !== -1 &&
      typeof currentStamp === "number" &&
      typeof deltaStamp === "number" &&
      currentStamp >= deltaStamp;
    if (!isSameRevisionProgressiveSuffixCreate && !isReplayOfKnownMessage) {
      return true;
    }
  }

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
    !isValidMessageCount(delta.messageCount) ||
    !isValidMessageIndexForCount(delta.messageIndex, delta.messageCount)
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
    const currentStamp = session.sessionMutationStamp;
    const deltaStamp = delta.sessionMutationStamp;
    if (
      typeof currentStamp === "number" &&
      typeof deltaStamp === "number" &&
      deltaStamp < currentStamp
    ) {
      return session;
    }
    updatedMessages.splice(existingMessageIndex, 1);
  }
  if (delta.messageIndex > updatedMessages.length) {
    if (session.messagesLoaded !== false) {
      return null;
    }
    updatedMessages.push(delta.message);
  } else {
    updatedMessages.splice(delta.messageIndex, 0, delta.message);
  }

  const pendingPrompts = removePendingPromptById(
    session.pendingPrompts,
    delta.messageId,
  );

  const nextMessageCount = Math.max(
    session.messageCount ?? 0,
    updatedMessages.length,
    delta.messageCount,
  );

  return {
    ...session,
    messages: updatedMessages,
    messagesLoaded:
      updatedMessages.length >= nextMessageCount
        ? true
        : false,
    messageCount: nextMessageCount,
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
        retainedTranscriptUpdate.messages.length < delta.messageCount
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

      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        if (session.messagesLoaded === false) {
          return {
            kind: "appliedNeedsResync",
            sessions: replaceSession(
              sessions,
              sessionIndex,
              applyMetadataOnlySessionDelta(session, delta),
            ),
          };
        }
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
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        if (session.messagesLoaded === false) {
          return {
            kind: "appliedNeedsResync",
            sessions: replaceSession(
              sessions,
              sessionIndex,
              applyMetadataOnlySessionDelta(session, delta),
            ),
          };
        }
        return { kind: "needsResync" };
      }
      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return {
          kind: "needsResync",
        };
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
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        if (session.messagesLoaded === false) {
          return {
            kind: "appliedNeedsResync",
            sessions: replaceSession(
              sessions,
              sessionIndex,
              applyMetadataOnlySessionDelta(session, delta),
            ),
          };
        }
        return { kind: "needsResync" };
      }
      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return {
          kind: "needsResync",
        };
      }
      if (message.type !== "text") {
        return { kind: "needsResync" };
      }

      // No-op short-circuit: when the delta's text/preview/mutation-stamp/
      // count all match what the session already has, return the original
      // `sessions` array (preserving identity). Callers can detect this
      // case via `result.sessions === input` and skip downstream churn
      // (transport-activity marking, watchdog baseline reset, re-render).
      // This prevents stale stream replays at the same revision — the
      // server re-emitting a textReplace whose content is already on the
      // client — from masking a stalled active session and stopping the
      // watchdog from firing.
      const previewIfApplied = delta.preview ?? session.preview;
      const stampIfApplied = resolveSessionMutationStamp(
        session,
        delta.sessionMutationStamp,
      );
      if (
        message.text === delta.text &&
        previewIfApplied === session.preview &&
        stampIfApplied === (session.sessionMutationStamp ?? null) &&
        delta.messageCount === session.messageCount
      ) {
        return { kind: "applied", sessions };
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
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        if (session.messagesLoaded === false) {
          return {
            kind: "appliedNeedsResync",
            sessions: replaceSession(
              sessions,
              sessionIndex,
              applyMetadataOnlySessionDelta(session, delta),
            ),
          };
        }
        return { kind: "needsResync" };
      }
      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return {
          kind: "needsResync",
        };
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
      const messageIndex = findMessageIndex(
        session.messages,
        delta.messageId,
        delta.messageIndex,
      );
      if (messageIndex === -1) {
        if (session.messagesLoaded === false) {
          return {
            kind: "appliedNeedsResync",
            sessions: replaceSession(
              sessions,
              sessionIndex,
              applyMetadataOnlySessionDelta(session, delta),
            ),
          };
        }
        return { kind: "needsResync" };
      }
      const message = session.messages[messageIndex];
      if (!message || message.id !== delta.messageId) {
        return {
          kind: "needsResync",
        };
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
    case "conversationMarkerCreated":
    case "conversationMarkerUpdated": {
      if (sessionIndex === -1 || delta.marker.sessionId !== delta.sessionId) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (session.messagesLoaded === false) {
        return { kind: "needsResync" };
      }
      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          markers: upsertConversationMarker(
            session.markers ?? [],
            delta.marker,
          ),
          sessionMutationStamp: resolveSessionMutationStamp(
            session,
            delta.sessionMutationStamp,
          ),
        }),
      };
    }
    case "conversationMarkerDeleted": {
      if (sessionIndex === -1) {
        return { kind: "needsResync" };
      }
      const session = sessions[sessionIndex];
      if (session.messagesLoaded === false) {
        return { kind: "needsResync" };
      }
      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          markers: (session.markers ?? []).filter(
            (marker) => marker.id !== delta.markerId,
          ),
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

function upsertConversationMarker(
  markers: NonNullable<Session["markers"]>,
  marker: NonNullable<Session["markers"]>[number],
) {
  const index = markers.findIndex((entry) => entry.id === marker.id);
  if (index === -1) {
    return [...markers, marker];
  }
  const updatedMarkers = markers.slice();
  updatedMarkers[index] = marker;
  return updatedMarkers;
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
