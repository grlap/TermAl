// Owns client-side construction of conversation-marker API payloads.
// Does not own marker request dispatch, stale-response recovery, or marker UI.
// Split from app-session-actions.ts to keep action orchestration smaller.
import type { CreateConversationMarkerRequest } from "./api";
import type { CreateConversationMarkerOptions } from "./types";

export function buildCreateConversationMarkerRequest(
  messageId: string,
  options: CreateConversationMarkerOptions = {},
): CreateConversationMarkerRequest {
  return {
    kind: "checkpoint",
    name: options.name?.trim() || "Checkpoint",
    body: null,
    color: "#3b82f6",
    messageId,
    endMessageId: null,
  };
}
