// Owns pane-local tab visit history updates and MRU fallback selection.
// Deliberately does not own pane synchronization or workspace tree mutations;
// this was split out of `workspace.ts` so reducers share one history policy.

import type { WorkspacePane, WorkspaceTab } from "./workspace-types";

// Keep this producer contract aligned with the persisted boundary check in
// `workspace-storage.ts::hasValidPaneTabVisitHistory`.
function pruneHistoryCandidates(
  candidates: Array<string | null>,
  tabIds: ReadonlySet<string>,
): string[] {
  const seenTabIds = new Set<string>();
  return candidates.filter((candidateId): candidateId is string => {
    if (
      candidateId === null ||
      !tabIds.has(candidateId) ||
      seenTabIds.has(candidateId)
    ) {
      return false;
    }
    seenTabIds.add(candidateId);
    return true;
  });
}

export function selectActiveTabAfterRemoval(
  pane: WorkspacePane,
  remainingTabs: WorkspaceTab[],
  removedTabId: string | null,
  fallbackTabId: string | null,
): string | null {
  if (pane.activeTabId !== removedTabId) {
    return pane.activeTabId;
  }

  const remainingTabIds = new Set(remainingTabs.map((tab) => tab.id));
  return (
    pane.tabVisitHistory?.find(
      (tabId) => tabId !== removedTabId && remainingTabIds.has(tabId),
    ) ?? fallbackTabId
  );
}

export function withActivatedPaneTab(
  pane: WorkspacePane,
  activeTabId: string | null,
  tabs: WorkspaceTab[] = pane.tabs,
): WorkspacePane {
  const tabIds = new Set(tabs.map((tab) => tab.id));
  const tabVisitHistory = pruneHistoryCandidates(
    [activeTabId, pane.activeTabId, ...(pane.tabVisitHistory ?? [])],
    tabIds,
  );

  return {
    ...pane,
    tabs,
    activeTabId,
    tabVisitHistory,
  };
}

export function withPrunedPaneTabVisitHistory(
  pane: WorkspacePane,
  activeTabId: string | null,
): WorkspacePane {
  if (typeof pane.tabVisitHistory === "undefined") {
    return pane;
  }

  const tabIds = new Set(pane.tabs.map((tab) => tab.id));
  const tabVisitHistory = pruneHistoryCandidates(
    [activeTabId, ...pane.tabVisitHistory],
    tabIds,
  );

  return {
    ...pane,
    tabVisitHistory,
  };
}
