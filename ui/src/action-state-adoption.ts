// action-state-adoption.ts
//
// Owns pure stale-action adoption classification for `useAppSessionActions`.
//
// Split out of: ui/src/app-session-actions.ts. The hook owns UI cleanup and
// recovery dispatch; this module only decides whether a rejected action state
// has enough target-specific evidence to be treated as already materialized.
// Relies on `state-revision.ts:isStaleSameInstanceSnapshot` for the
// same-server-instance monotonic-revision/stamp-reset invariant.

import type { StateResponse } from "./api";
import { isStaleSameInstanceSnapshot } from "./state-revision";
import type { Session } from "./types";

export type StaleActionTargetEvidenceOptions = {
  staleSuccessProjectId?: string;
  staleSuccessSessionEvidence?: Session | null;
  staleSuccessSessionId?: string;
};

export type RejectedActionStateDecision = "stale-success" | "recovering";

export function classifyRejectedActionState({
  currentProjects,
  currentRevision,
  currentServerInstanceId,
  currentSessions,
  options,
  state,
}: {
  currentProjects: ReadonlyMap<string, unknown>;
  currentRevision: number | null;
  currentServerInstanceId: string | null | undefined;
  currentSessions: readonly Session[];
  options: StaleActionTargetEvidenceOptions | undefined;
  state: StateResponse;
}): RejectedActionStateDecision {
  if (
    isStaleSameInstanceSnapshot(
      currentRevision,
      state.revision,
      currentServerInstanceId,
      state.serverInstanceId,
    ) &&
    staleActionTargetEvidenceExists({
      currentProjects,
      currentSessions,
      options,
      state,
    })
  ) {
    return "stale-success";
  }
  return "recovering";
}

export function staleActionTargetEvidenceExists({
  currentProjects,
  currentSessions,
  options,
  state,
}: {
  currentProjects: ReadonlyMap<string, unknown>;
  currentSessions: readonly Session[];
  options: StaleActionTargetEvidenceOptions | undefined;
  state: StateResponse;
}) {
  if (options?.staleSuccessSessionId !== undefined) {
    return staleSessionActionTargetEvidenceExists({
      currentSessions,
      sessionId: options.staleSuccessSessionId,
      staleSuccessSessionEvidence: options.staleSuccessSessionEvidence,
      state,
    });
  }
  if (options?.staleSuccessProjectId !== undefined) {
    return currentProjects.has(options.staleSuccessProjectId);
  }
  return false;
}

export function staleSessionActionTargetEvidenceExists({
  currentSessions,
  sessionId,
  staleSuccessSessionEvidence,
  state,
}: {
  currentSessions: readonly Session[];
  sessionId: string;
  staleSuccessSessionEvidence: Session | null | undefined;
  state: StateResponse;
}) {
  const responseSession =
    state.sessions.find((entry) => entry.id === sessionId) ?? null;
  const currentSession =
    currentSessions.find((entry) => entry.id === sessionId) ?? null;
  if (!responseSession) {
    return currentSession === null;
  }
  if (!currentSession) {
    return false;
  }

  const responseMutationStamp = responseSession.sessionMutationStamp;
  if (typeof responseMutationStamp !== "number") {
    return false;
  }

  // Settings actions can optimistically write an older session object before
  // the stale response arrives. Preserve the pre-optimistic live session as
  // target evidence so that temporary local UI state cannot hide a mutation
  // that already materialized through SSE.
  const evidenceSessions =
    staleSuccessSessionEvidence?.id === sessionId
      ? [currentSession, staleSuccessSessionEvidence]
      : [currentSession];
  return evidenceSessions.some((session) => {
    const currentMutationStamp = session?.sessionMutationStamp;
    return (
      typeof currentMutationStamp === "number" &&
      currentMutationStamp >= responseMutationStamp
    );
  });
}
