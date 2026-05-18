// Owns: draft attachment add/remove action handlers for session composers.
// Does not own: prompt send, object URL release after send, or draft storage
// primitives.
// Split from: ui/src/app-session-actions.ts.

import type {
  UseAppSessionActionsRefs,
  UseAppSessionActionsReturn,
  UseAppSessionActionsSetters,
} from "./app-session-actions-types";
import {
  appendDraftAttachments,
  draftAttachmentsMatchingId,
  draftAttachmentsWithoutId,
  removeDraftAttachmentFromState,
} from "./app-session-draft-attachments";
import {
  releaseDraftAttachments,
  type DraftImageAttachment,
} from "./app-utils";

type DraftAttachmentActions = Pick<
  UseAppSessionActionsReturn,
  "handleDraftAttachmentsAdd" | "handleDraftAttachmentRemove"
>;

type DraftAttachmentActionsDeps = {
  draftAttachmentsBySessionIdRef: UseAppSessionActionsRefs["draftAttachmentsBySessionIdRef"];
  draftsBySessionIdRef: UseAppSessionActionsRefs["draftsBySessionIdRef"];
  setDraftAttachmentsBySessionId: UseAppSessionActionsSetters["setDraftAttachmentsBySessionId"];
  syncComposerDraftSlice: (
    sessionId: string,
    committedDraft: string,
    draftAttachments: readonly DraftImageAttachment[],
  ) => void;
};

export function createDraftAttachmentActions({
  draftAttachmentsBySessionIdRef,
  draftsBySessionIdRef,
  setDraftAttachmentsBySessionId,
  syncComposerDraftSlice,
}: DraftAttachmentActionsDeps): DraftAttachmentActions {
  function handleDraftAttachmentsAdd(
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) {
    const nextAttachments = [
      ...(draftAttachmentsBySessionIdRef.current[sessionId] ?? []),
      ...attachments,
    ];
    syncComposerDraftSlice(
      sessionId,
      draftsBySessionIdRef.current[sessionId] ?? "",
      nextAttachments,
    );
    setDraftAttachmentsBySessionId((current) =>
      appendDraftAttachments(current, sessionId, attachments),
    );
  }

  function handleDraftAttachmentRemove(
    sessionId: string,
    attachmentId: string,
  ) {
    const existingAttachments =
      draftAttachmentsBySessionIdRef.current[sessionId] ?? [];
    const removed = draftAttachmentsMatchingId(existingAttachments, attachmentId);
    if (removed.length === 0) {
      return;
    }

    releaseDraftAttachments(removed);
    const nextRefAttachments = draftAttachmentsWithoutId(
      existingAttachments,
      attachmentId,
    );
    syncComposerDraftSlice(
      sessionId,
      draftsBySessionIdRef.current[sessionId] ?? "",
      nextRefAttachments,
    );
    setDraftAttachmentsBySessionId((current) =>
      removeDraftAttachmentFromState(current, sessionId, attachmentId),
    );
  }

  return {
    handleDraftAttachmentsAdd,
    handleDraftAttachmentRemove,
  };
}
