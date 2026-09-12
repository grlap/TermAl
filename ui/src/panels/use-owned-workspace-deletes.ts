// Owns pending/generation/observed lifecycle for panel-owned
// workspace deletes. Unowned IDs that leave the parent deleting
// set are pruned so stale observation cannot retain a later
// same-id lock. Does not own confirm UI, retained summaries,
// overflow focus, or the parent DELETE adapter. Split from
// WorkspacesPanel.tsx.

import { useCallback, useEffect, useMemo, useRef } from "react";

import type { WorkspaceDeleteRequest } from "../workspace-delete-request";
import { useCommittedRef } from "./use-committed-ref";

export function useOwnedWorkspaceDeletes({
  deletingWorkspaceIds,
  onDeleteWorkspace,
  onOwnedDeletesSettled,
}: {
  deletingWorkspaceIds: readonly string[];
  onDeleteWorkspace: (workspaceId: string) => WorkspaceDeleteRequest;
  onOwnedDeletesSettled?: (workspaceIds: readonly string[]) => void;
}) {
  const pendingDeleteIdsRef = useRef(new Set<string>());
  const pendingDeleteGenerationRef = useRef(new Map<string, number>());
  const ownedDeleteRequestSeqRef = useRef(0);
  const observedDeletingIdsRef = useRef(new Set<string>());
  const deletingWorkspaceIdSet = useMemo(
    () => new Set(deletingWorkspaceIds),
    [deletingWorkspaceIds],
  );
  const deletingWorkspaceIdSetRef = useCommittedRef(deletingWorkspaceIdSet);
  const onOwnedDeletesSettledRef = useCommittedRef(onOwnedDeletesSettled);

  useEffect(() => {
    for (const workspaceId of deletingWorkspaceIdSet) {
      observedDeletingIdsRef.current.add(workspaceId);
    }
    for (const workspaceId of [...observedDeletingIdsRef.current]) {
      if (
        deletingWorkspaceIdSet.has(workspaceId)
        || pendingDeleteIdsRef.current.has(workspaceId)
      ) {
        continue;
      }
      observedDeletingIdsRef.current.delete(workspaceId);
    }
    const settledIds = [...pendingDeleteIdsRef.current].filter((workspaceId) => (
      observedDeletingIdsRef.current.has(workspaceId)
      && !deletingWorkspaceIdSet.has(workspaceId)
    ));
    if (!settledIds.length) {
      return;
    }
    for (const workspaceId of settledIds) {
      pendingDeleteIdsRef.current.delete(workspaceId);
      pendingDeleteGenerationRef.current.delete(workspaceId);
      observedDeletingIdsRef.current.delete(workspaceId);
    }
    onOwnedDeletesSettledRef.current?.(settledIds);
  }, [deletingWorkspaceIdSet, onOwnedDeletesSettledRef]);

  const isOwnedDeletePending = useCallback((workspaceId: string) => {
    return pendingDeleteIdsRef.current.has(workspaceId);
  }, []);

  function finishOwnedDeleteRequest(workspaceId: string, generation: number) {
    if (pendingDeleteGenerationRef.current.get(workspaceId) !== generation) {
      return;
    }
    if (
      observedDeletingIdsRef.current.has(workspaceId)
      || deletingWorkspaceIdSetRef.current.has(workspaceId)
    ) {
      return;
    }
    pendingDeleteIdsRef.current.delete(workspaceId);
    pendingDeleteGenerationRef.current.delete(workspaceId);
  }

  function confirmOwnedDelete(workspaceId: string) {
    if (
      pendingDeleteIdsRef.current.has(workspaceId)
      || deletingWorkspaceIdSet.has(workspaceId)
    ) {
      return;
    }
    const generation = ownedDeleteRequestSeqRef.current + 1;
    ownedDeleteRequestSeqRef.current = generation;
    pendingDeleteGenerationRef.current.set(workspaceId, generation);
    pendingDeleteIdsRef.current.add(workspaceId);
    try {
      const request = onDeleteWorkspace(workspaceId);
      if (!request.started) {
        finishOwnedDeleteRequest(workspaceId, generation);
        return;
      }
      void request.completed.then(
        () => finishOwnedDeleteRequest(workspaceId, generation),
        () => finishOwnedDeleteRequest(workspaceId, generation),
      );
    } catch {
      finishOwnedDeleteRequest(workspaceId, generation);
    }
  }

  return {
    isOwnedDeletePending,
    confirmOwnedDelete,
  };
}
