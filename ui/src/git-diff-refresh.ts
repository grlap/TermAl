// Helpers for collecting the set of open git-diff preview tabs that
// need their payload re-fetched.
//
// What this file owns:
//   - `GitDiffPreviewRefresh` — the shape the refresh batch passes
//     around: the original `GitDiffRequestPayload`, its stable
//     request key, and the `GitDiffSection` it belongs to. Keyed
//     uniquely by `requestKey` so duplicate tabs collapse into a
//     single refresh.
//   - `collectRestoredGitDiffDocumentContentRefreshes` — walks a
//     workspace after a layout restore and returns the diff tabs
//     that are missing their `documentContent` hydration (skipping
//     tabs still loading, tabs with no request, and tabs whose
//     request key is already pending or already attempted).
//   - `collectGitDiffPreviewRefreshes` — walks a workspace in
//     response to a `WorkspaceFilesChangedEvent` and returns the
//     diff tabs whose files the event touched. Delegates the
//     per-tab touch check to
//     `workspaceFilesChangedEventTouchesGitDiffTab`.
//
// What this file does NOT own:
//   - Firing the actual refresh requests — callers drive the
//     refresh fetch / dispatch loop and track pending vs attempted
//     request keys.
//   - The workspace-file-event → tab touch predicate itself —
//     that lives in `./workspace-file-events`. This module
//     composes it.
//   - React state or components.
//
// Split out of `ui/src/App.tsx`. Same function signatures and
// behaviour as the inline definitions they replaced.

import type { GitDiffRequestPayload, GitDiffSection } from "./api";
import type { WorkspaceFilesChangedEvent } from "./types";
import { workspaceFilesChangedEventTouchesGitDiffTab } from "./workspace-file-events";
import type { WorkspaceState } from "./workspace";

export type GitDiffPreviewRefresh = {
  request: GitDiffRequestPayload;
  requestKey: string;
  sectionId: GitDiffSection;
};

export function collectRestoredGitDiffDocumentContentRefreshes(
  workspace: WorkspaceState,
  pendingRequestKeys: ReadonlySet<string>,
  attemptedRequestKeys: ReadonlySet<string>,
): GitDiffPreviewRefresh[] {
  const refreshes = new Map<string, GitDiffPreviewRefresh>();

  for (const pane of workspace.panes) {
    for (const tab of pane.tabs) {
      if (tab.kind !== "diffPreview") {
        continue;
      }
      if (
        tab.documentContent ||
        !tab.gitDiffRequestKey ||
        !tab.gitDiffRequest
      ) {
        continue;
      }
      if (tab.isLoading === true && tab.diff.trim().length === 0) {
        continue;
      }
      if (
        pendingRequestKeys.has(tab.gitDiffRequestKey) ||
        attemptedRequestKeys.has(tab.gitDiffRequestKey)
      ) {
        continue;
      }
      refreshes.set(tab.gitDiffRequestKey, {
        request: tab.gitDiffRequest,
        requestKey: tab.gitDiffRequestKey,
        sectionId: tab.gitSectionId ?? tab.gitDiffRequest.sectionId,
      });
    }
  }

  return Array.from(refreshes.values());
}

export function collectGitDiffPreviewRefreshes(
  workspace: WorkspaceState,
  event: WorkspaceFilesChangedEvent,
): GitDiffPreviewRefresh[] {
  const refreshes = new Map<string, GitDiffPreviewRefresh>();

  for (const pane of workspace.panes) {
    for (const tab of pane.tabs) {
      if (
        tab.kind !== "diffPreview" ||
        !tab.gitDiffRequestKey ||
        !tab.gitDiffRequest ||
        !workspaceFilesChangedEventTouchesGitDiffTab(event, tab)
      ) {
        continue;
      }

      refreshes.set(tab.gitDiffRequestKey, {
        request: tab.gitDiffRequest,
        requestKey: tab.gitDiffRequestKey,
        sectionId: tab.gitSectionId ?? tab.gitDiffRequest.sectionId,
      });
    }
  }

  return Array.from(refreshes.values());
}
