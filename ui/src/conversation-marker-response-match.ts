// Owns client-side checks that prove a conversation-marker action response is
// already reflected in local state after a stale action response.
// Does not own marker CRUD requests, UI rendering, or marker color validation.
// Split from app-session-actions.ts to keep action orchestration smaller.
import { conversationMarkerColorsMatchForState } from "./conversation-marker-state-equality";
import type { ConversationMarker } from "./types";

export function conversationMarkerExactlyMatches(
  current: ConversationMarker,
  response: ConversationMarker,
) {
  return (
    current.id === response.id &&
    current.sessionId === response.sessionId &&
    current.kind === response.kind &&
    current.name === response.name &&
    (current.body ?? null) === (response.body ?? null) &&
    conversationMarkerColorsMatchForState(current.color, response.color) &&
    current.messageId === response.messageId &&
    current.messageIndexHint === response.messageIndexHint &&
    (current.endMessageId ?? null) === (response.endMessageId ?? null) &&
    (current.endMessageIndexHint ?? null) ===
      (response.endMessageIndexHint ?? null) &&
    current.createdAt === response.createdAt &&
    current.updatedAt === response.updatedAt &&
    current.createdBy === response.createdBy
  );
}

export function conversationMarkerSatisfiesResponse(
  current: ConversationMarker | undefined,
  response: ConversationMarker | undefined,
) {
  return (
    current !== undefined &&
    response !== undefined &&
    current.id === response.id &&
    conversationMarkerExactlyMatches(current, response)
  );
}
