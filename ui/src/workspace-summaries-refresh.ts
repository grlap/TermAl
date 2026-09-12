// Owns the shared refresh options/callback types for workspace
// summaries. Does not own GET reuse, request tokens, loading/error
// state, or the Workspaces panel. Split from app-workspace-layout.ts
// so AppControlSurface can type refreshWorkspaceSummaries without
// importing UseAppWorkspaceLayoutReturn.

export type WorkspaceSummariesRefreshOptions = {
  preserveError?: boolean;
  forceFresh?: boolean;
};

export type RefreshWorkspaceSummaries = (
  options?: WorkspaceSummariesRefreshOptions,
) => Promise<void>;
