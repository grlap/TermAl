// Owns: message-list selection helpers for SessionPaneView modes.
// Does not own: transcript rendering, search indexing, or scroll restoration.
// Split from: ui/src/SessionPaneView.tsx.

import type {
  CommandMessage,
  DiffMessage,
  Message,
  Session,
} from "./types";
import type { PaneViewMode } from "./workspace";

export function commandMessagesForPaneViewMode(
  viewMode: PaneViewMode,
  session: Session | null,
): CommandMessage[] {
  return viewMode === "commands" && session
    ? session.messages.filter(
        (message): message is CommandMessage => message.type === "command",
      )
    : [];
}

export function diffMessagesForPaneViewMode(
  viewMode: PaneViewMode,
  session: Session | null,
): DiffMessage[] {
  return viewMode === "diffs" && session
    ? session.messages.filter(
        (message): message is DiffMessage => message.type === "diff",
      )
    : [];
}

export function visibleMessagesForPaneViewMode(
  viewMode: PaneViewMode,
  commandMessages: CommandMessage[],
  diffMessages: DiffMessage[],
): Message[] {
  if (viewMode === "commands") {
    return commandMessages;
  }
  if (viewMode === "diffs") {
    return diffMessages;
  }
  return [];
}

export function paneViewModeDefaultsToBottomScroll(
  viewMode: PaneViewMode,
): boolean {
  return viewMode === "session" || viewMode === "commands" || viewMode === "diffs";
}

export function latestAssistantMessageIdForSession(
  session: Session | null,
): string | null {
  const sessionMessages = session?.messages ?? [];
  for (let index = sessionMessages.length - 1; index >= 0; index -= 1) {
    const candidate = sessionMessages[index];
    if (candidate && candidate.author === "assistant") {
      return candidate.id;
    }
  }
  return null;
}
