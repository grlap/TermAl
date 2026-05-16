// Owns action-layer draft ref updates plus session-store draft publication.
// Does not own send orchestration, attachment release, or React state setters.
// Split from app-session-actions.ts to keep session action flow smaller.
import type { MutableRefObject } from "react";
import type { DraftImageAttachment } from "./app-utils";
import { syncComposerDraftForSession } from "./session-store";

export function setDraftRefValue(
  draftsBySessionIdRef: MutableRefObject<Record<string, string>>,
  sessionId: string,
  nextValue: string,
) {
  const current = draftsBySessionIdRef.current;
  if ((current[sessionId] ?? "") === nextValue) {
    return;
  }

  draftsBySessionIdRef.current = {
    ...current,
    [sessionId]: nextValue,
  };
}

export function setDraftAttachmentRefs(
  draftAttachmentsBySessionIdRef: MutableRefObject<
    Record<string, DraftImageAttachment[]>
  >,
  sessionId: string,
  nextAttachments: readonly DraftImageAttachment[],
) {
  const current = draftAttachmentsBySessionIdRef.current;
  const currentAttachments = current[sessionId] ?? [];
  if (currentAttachments === nextAttachments) {
    return;
  }

  if (nextAttachments.length === 0) {
    if (!current[sessionId]) {
      return;
    }
    const nextState = { ...current };
    delete nextState[sessionId];
    draftAttachmentsBySessionIdRef.current = nextState;
    return;
  }

  draftAttachmentsBySessionIdRef.current = {
    ...current,
    [sessionId]: [...nextAttachments],
  };
}

export function syncActionComposerDraftSlice(
  refs: {
    draftsBySessionIdRef: MutableRefObject<Record<string, string>>;
    draftAttachmentsBySessionIdRef: MutableRefObject<
      Record<string, DraftImageAttachment[]>
    >;
  },
  sessionId: string,
  committedDraft: string,
  draftAttachments: readonly DraftImageAttachment[],
) {
  setDraftRefValue(refs.draftsBySessionIdRef, sessionId, committedDraft);
  setDraftAttachmentRefs(
    refs.draftAttachmentsBySessionIdRef,
    sessionId,
    draftAttachments,
  );
  syncComposerDraftForSession({
    sessionId,
    committedDraft,
    draftAttachments,
  });
}
