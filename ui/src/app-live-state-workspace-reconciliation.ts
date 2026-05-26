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

function workspaceHasDelegatedChildSessionReferences(
  workspace: WorkspaceState,
  sessions: readonly Session[],
  preserveSessionIds: readonly string[] = [],
) {
  const preservedSessionIds = new Set(preserveSessionIds);
  const delegatedChildSessionIds = new Set(
    sessions.flatMap((session) =>
      session.parentDelegationId && !preservedSessionIds.has(session.id)
        ? [session.id]
        : [],
    ),
  );
  if (delegatedChildSessionIds.size === 0) {
    return false;
  }

  return workspace.panes.some((pane) => {
    if (
      pane.activeSessionId &&
      delegatedChildSessionIds.has(pane.activeSessionId)
    ) {
      return true;
    }

    return pane.tabs.some((tab) => {
      if (tab.kind === "session") {
        return delegatedChildSessionIds.has(tab.sessionId);
      }
      if (tab.kind === "canvas") {
        return tab.cards.some((card) =>
          delegatedChildSessionIds.has(card.sessionId),
        );
      }
      return (
        "originSessionId" in tab &&
        !!tab.originSessionId &&
        delegatedChildSessionIds.has(tab.originSessionId)
      );
    });
  });
}

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
