// Owns immutable draft-attachment collection transforms for session actions.
// Does not own object URL cleanup, draft text sync, or composer UI wiring.
// Split from app-session-actions.ts to keep action orchestration smaller.

import type { DraftImageAttachment } from "./app-utils";

export type DraftAttachmentState = Record<string, DraftImageAttachment[]>;

export function appendDraftAttachments(
  current: DraftAttachmentState,
  sessionId: string,
  attachments: DraftImageAttachment[],
): DraftAttachmentState {
  return {
    ...current,
    [sessionId]: [...(current[sessionId] ?? []), ...attachments],
  };
}

export function draftAttachmentsMatchingId(
  attachments: DraftImageAttachment[],
  attachmentId: string,
): DraftImageAttachment[] {
  return attachments.filter((attachment) => attachment.id === attachmentId);
}

export function draftAttachmentsWithoutId(
  attachments: DraftImageAttachment[],
  attachmentId: string,
): DraftImageAttachment[] {
  return attachments.filter((attachment) => attachment.id !== attachmentId);
}

export function removeDraftAttachmentFromState(
  current: DraftAttachmentState,
  sessionId: string,
  attachmentId: string,
): DraftAttachmentState {
  const existing = current[sessionId];
  if (!existing) {
    return current;
  }

  const nextAttachments = draftAttachmentsWithoutId(existing, attachmentId);
  if (nextAttachments.length === existing.length) {
    return current;
  }
  if (nextAttachments.length === 0) {
    const nextState = { ...current };
    delete nextState[sessionId];
    return nextState;
  }

  return {
    ...current,
    [sessionId]: nextAttachments,
  };
}
