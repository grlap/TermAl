// Owns: workspace reconciliation helpers used while adopting live-state
// session snapshots, especially delegated-child pruning during backend
// restart recovery.
// Does not own: workspace layout persistence/fetch readiness or general
// workspace tree operations, which remain in app-workspace-layout.ts and
// workspace.ts.
// Split from: ui/src/app-live-state.ts.

import type { Session } from "./types";
import {
  openSessionInWorkspaceState,
  reconcileWorkspaceState,
  workspaceHasDelegatedChildSessionReferences,
  type WorkspaceState,
} from "./workspace";

type ReconcileAdoptedSessionsWorkspaceOptions = {
  applyControlPanelLayout: (workspace: WorkspaceState) => WorkspaceState;
  canOpenPendingSession: boolean;
  current: WorkspaceState;
  mergedSessions: Session[];
  pendingOpenSessionId?: string;
  pendingPaneId: string | null;
  pruneDelegatedChildWorkspaceTabs: boolean;
  sessionsChanged: boolean;
};

export function reconcileAdoptedSessionsWorkspace({
  applyControlPanelLayout,
  canOpenPendingSession,
  current,
  mergedSessions,
  pendingOpenSessionId,
  pendingPaneId,
  pruneDelegatedChildWorkspaceTabs,
  sessionsChanged,
}: ReconcileAdoptedSessionsWorkspaceOptions) {
  const preservedSessionIds = pendingOpenSessionId ? [pendingOpenSessionId] : [];
  const shouldPruneCurrentWorkspace =
    pruneDelegatedChildWorkspaceTabs &&
    workspaceHasDelegatedChildSessionReferences(
      current,
      mergedSessions,
      preservedSessionIds,
    );
  const shouldReconcileCurrentWorkspace =
    sessionsChanged || canOpenPendingSession || shouldPruneCurrentWorkspace;
  if (!shouldReconcileCurrentWorkspace) {
    return current;
  }

  const reconciled =
    sessionsChanged || shouldPruneCurrentWorkspace
      ? applyControlPanelLayout(
          reconcileWorkspaceState(current, mergedSessions, {
            preserveSessionIds: preservedSessionIds,
            pruneDelegatedChildSessionTabs: shouldPruneCurrentWorkspace,
          }),
        )
      : current;
  if (!canOpenPendingSession || !pendingOpenSessionId) {
    return reconciled;
  }

  return applyControlPanelLayout(
    openSessionInWorkspaceState(reconciled, pendingOpenSessionId, pendingPaneId),
  );
}
