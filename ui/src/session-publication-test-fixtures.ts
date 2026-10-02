// Legacy hook fixtures seed and observe the owner's publication boundary.
// The writable facade exists only in tests: assigning it calls commit rather
// than mutating the application's read-only ref behind the owner.
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type { UseAppLiveStateParams } from "./app-live-state-types";
import type { UseAppSessionActionsParams } from "./app-session-actions-types";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import type { Session } from "./types";

type TestSetter = { setSessions: Dispatch<SetStateAction<Session[]>> };
export type TestLiveStateParams = Omit<UseAppLiveStateParams, "adoptionRefs" | "stateSetters"> & {
  adoptionRefs: Omit<UseAppLiveStateParams["adoptionRefs"], "sessionsRef"> & { sessionsRef: MutableRefObject<Session[]> };
  stateSetters: UseAppLiveStateParams["stateSetters"] & TestSetter;
};
export type TestSessionActionsParams = Omit<UseAppSessionActionsParams, "refs" | "setters"> & {
  refs: Omit<UseAppSessionActionsParams["refs"], "sessionsRef"> & { sessionsRef: MutableRefObject<Session[]> };
  setters: UseAppSessionActionsParams["setters"] & TestSetter;
};

export function withLiveSessionAuthority(params: Omit<TestLiveStateParams, "sessionAuthority">): TestLiveStateParams {
  const refs = params.adoptionRefs;
  const owner = new TranscriptRepairAuthority({
    publish: next => params.stateSetters.setSessions(next()),
    drafts: () => refs.draftsBySessionIdRef.current,
    attachments: () => refs.draftAttachmentsBySessionIdRef.current,
  }, refs.sessionsRef.current);
  refs.sessionsRef = writableTestRef(owner);
  return { ...params, sessionAuthority: owner };
}

export function withActionSessionAuthority(params: Omit<TestSessionActionsParams, "sessionAuthority">): TestSessionActionsParams {
  const refs = params.refs;
  const owner = new TranscriptRepairAuthority({
    publish: next => params.setters.setSessions(next()),
    drafts: () => refs.draftsBySessionIdRef.current,
    attachments: () => refs.draftAttachmentsBySessionIdRef.current,
  }, refs.sessionsRef.current);
  refs.sessionsRef = writableTestRef(owner);
  return { ...params, sessionAuthority: owner };
}

function writableTestRef(owner: TranscriptRepairAuthority): MutableRefObject<Session[]> {
  return {
    get current() { return owner.sessionsRef.current as Session[]; },
    set current(next: Session[]) { owner.commit(next, "metadata"); },
  };
}
