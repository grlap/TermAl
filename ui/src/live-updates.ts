import type { CommandMessage, DeltaEvent, Session, TextMessage } from "./types";

export type DeltaApplyResult =
  | { kind: "applied"; sessions: Session[] }
  | { kind: "needsResync" };

export function applyDeltaToSessions(sessions: Session[], delta: DeltaEvent): DeltaApplyResult {
  const sessionIndex = sessions.findIndex((session) => session.id === delta.sessionId);
  if (sessionIndex === -1) {
    return { kind: "needsResync" };
  }

  const session = sessions[sessionIndex];

  switch (delta.type) {
    case "messageCreated": {
      if (
        delta.message.id !== delta.messageId ||
        delta.messageIndex < 0 ||
        delta.messageIndex > session.messages.length
      ) {
        return { kind: "needsResync" };
      }

      const updatedMessages = session.messages.slice();
      updatedMessages.splice(delta.messageIndex, 0, delta.message);

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          preview: delta.preview,
          status: delta.status,
        }),
      };
    }

    case "textDelta": {
      const message = session.messages[delta.messageIndex];
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
      updatedMessages[delta.messageIndex] = updatedMessage;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          preview: delta.preview ?? session.preview,
        }),
      };
    }

    case "commandUpdate": {
      const message = session.messages[delta.messageIndex];
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
      updatedMessages[delta.messageIndex] = updatedMessage;

      return {
        kind: "applied",
        sessions: replaceSession(sessions, sessionIndex, {
          ...session,
          messages: updatedMessages,
          preview: delta.preview,
        }),
      };
    }
  }
}

function replaceSession(sessions: Session[], index: number, session: Session) {
  const updatedSessions = sessions.slice();
  updatedSessions[index] = session;
  return updatedSessions;
}
