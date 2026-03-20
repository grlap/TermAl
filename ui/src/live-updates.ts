import type { CommandMessage, DeltaEvent, ParallelAgentsMessage, Session, TextMessage } from "./types";

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
      if (delta.message.id !== delta.messageId || delta.messageIndex < 0) {
        return { kind: "needsResync" };
      }

      const updatedMessages = session.messages.slice();
      const existingMessageIndex = findMessageIndex(updatedMessages, delta.messageId, delta.messageIndex);
      if (existingMessageIndex === -1) {
        if (delta.messageIndex > updatedMessages.length) {
          return { kind: "needsResync" };
        }

        updatedMessages.splice(delta.messageIndex, 0, delta.message);
      } else if (existingMessageIndex === delta.messageIndex) {
        updatedMessages[existingMessageIndex] = delta.message;
      } else {
        updatedMessages.splice(existingMessageIndex, 1);
        const nextMessageIndex =
          existingMessageIndex < delta.messageIndex ? delta.messageIndex - 1 : delta.messageIndex;
        updatedMessages.splice(nextMessageIndex, 0, delta.message);
      }

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
      const messageIndex = findMessageIndex(session.messages, delta.messageId, delta.messageIndex);
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
          preview: delta.preview ?? session.preview,
        }),
      };
    }

    case "commandUpdate": {
      const messageIndex = findMessageIndex(session.messages, delta.messageId, delta.messageIndex);
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
          preview: delta.preview,
        }),
      };
    }

    case "parallelAgentsUpdate": {
      const messageIndex = findMessageIndex(session.messages, delta.messageId, delta.messageIndex);
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

function findMessageIndex(messages: Session["messages"], messageId: string, preferredIndex: number) {
  const preferredMessage = messages[preferredIndex];
  if (preferredMessage?.id === messageId) {
    return preferredIndex;
  }

  return messages.findIndex((message) => message.id === messageId);
}
