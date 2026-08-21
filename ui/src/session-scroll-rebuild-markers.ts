// Owns the reversible marker transaction used when a workspace rebuild must
// restore session transcripts at the bottom after their panes remount.
// Does not own workspace mutation, drag/drop acceptance, or scroll rendering;
// callers decide when to begin or roll back the transaction.
// Transactions must not overlap: each caller resolves its rollback decision
// synchronously before another marker transaction can begin.
// Split from: ui/src/App.tsx.

import type { SessionFlagMap } from "./app-utils";
import type { WorkspaceState, WorkspaceTab } from "./workspace";

export type SessionScrollRebuildMarkerOptions = {
  sessionIds?: readonly string[];
  tabs?: readonly WorkspaceTab[];
};

export function beginSessionScrollBottomRebuild(
  getMarkers: () => SessionFlagMap,
  workspace: WorkspaceState,
  options?: SessionScrollRebuildMarkerOptions,
) {
  const markers = getMarkers();
  const sessionIds = new Set<string>();
  for (const pane of workspace.panes) {
    for (const tab of pane.tabs) {
      if (tab.kind === "session") {
        sessionIds.add(tab.sessionId);
      }
    }
  }

  for (const sessionId of options?.sessionIds ?? []) {
    sessionIds.add(sessionId);
  }
  for (const tab of options?.tabs ?? []) {
    if (tab.kind === "session") {
      sessionIds.add(tab.sessionId);
    }
  }

  const previousMarkers = new Map(
    [...sessionIds].map((sessionId) => [
      sessionId,
      {
        existed: Object.prototype.hasOwnProperty.call(markers, sessionId),
        value: markers[sessionId],
      },
    ]),
  );
  for (const sessionId of sessionIds) {
    markers[sessionId] = true;
  }

  return () => {
    const currentMarkers = getMarkers();
    for (const [sessionId, previousMarker] of previousMarkers) {
      if (previousMarker.existed && previousMarker.value !== undefined) {
        currentMarkers[sessionId] = previousMarker.value;
      } else {
        delete currentMarkers[sessionId];
      }
    }
  };
}
