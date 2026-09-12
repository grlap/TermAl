// Owns the started/completed contract for one owned workspace
// delete request. Does not own DELETE HTTP, list refresh, panel
// UI, or backend /api/workspaces wire types.
// Split from WorkspacesPanel.tsx and app-workspace-layout.ts.

export type WorkspaceDeleteRequest = {
  started: boolean;
  completed: Promise<void>;
};
