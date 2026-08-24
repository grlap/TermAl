// Owns Response Board tab-list synchronization, selection intent, and causal request ordering.
// Deliberately does not own tab CRUD requests, card state, canvas state, or rendering.
// Extracted from ResponseBoardPanel.tsx.

import { useCallback, useEffect, useRef, useState } from "react";

import {
  fetchResponseBoardTabs,
  type ResponseBoardTab,
} from "../api";
import type { WorkspaceResponseBoardView } from "../workspace";
import { useCommittedRef } from "./use-committed-ref";

type BoardView = WorkspaceResponseBoardView;

export function mergeResponseBoardTabOrder(
  currentTabs: ResponseBoardTab[],
  orderedTabs: ResponseBoardTab[],
) {
  // Current tabs own membership and metadata. The ordered snapshot contributes
  // only relative ordering, so a delayed mutation response cannot resurrect a
  // tab that a fresher list refresh has already removed.
  const orderById = new Map(orderedTabs.map((tab, index) => [tab.id, index]));
  return [...currentTabs]
    .sort((left, right) => {
      const leftOrder = orderById.get(left.id);
      const rightOrder = orderById.get(right.id);
      if (leftOrder !== undefined && rightOrder !== undefined) {
        return leftOrder - rightOrder;
      }
      if (leftOrder !== undefined) {
        return -1;
      }
      if (rightOrder !== undefined) {
        return 1;
      }
      return left.sortOrder - right.sortOrder;
    })
    .map((tab, sortOrder) =>
      tab.sortOrder === sortOrder ? tab : { ...tab, sortOrder },
    );
}

export function useResponseBoardTabs({
  activeBoardTabId,
  boardViews,
  onNoTabs,
  onWorkspaceStateChange,
  view,
  workspaceTabId,
}: {
  activeBoardTabId: string | null;
  boardViews: Record<string, WorkspaceResponseBoardView>;
  onNoTabs: () => void;
  onWorkspaceStateChange: (
    workspaceTabId: string,
    activeBoardTabId: string,
    view: WorkspaceResponseBoardView,
    knownBoardTabIds?: readonly string[],
  ) => void;
  view: BoardView;
  workspaceTabId: string;
}) {
  const [tabs, setTabs] = useState<ResponseBoardTab[]>([]);
  const [selectedTabId, setSelectedTabId] = useState<string | null>(
    activeBoardTabId,
  );
  const [tabRefreshError, setTabRefreshError] = useState<string | null>(null);
  const tabListRequestVersionRef = useRef(0);
  const selectedTabIdIntentRef = useRef<string | null>(activeBoardTabId);
  const pendingTabOrderRef = useRef<ResponseBoardTab[] | null>(null);
  const lastWorkspaceTabListSyncRef = useRef<{
    workspaceTabId: string;
    selectedTabId: string;
    knownTabIdsSignature: string;
  } | null>(null);
  const isMountedRef = useRef(true);
  const activeBoardTabIdRef = useCommittedRef(activeBoardTabId);
  const boardViewsRef = useCommittedRef(boardViews);
  const onNoTabsRef = useCommittedRef(onNoTabs);
  const onWorkspaceStateChangeRef = useCommittedRef(onWorkspaceStateChange);
  const selectedTabIdRef = useCommittedRef(selectedTabId);
  const tabsRef = useCommittedRef(tabs);
  const viewRef = useCommittedRef(view);

  const selectResponseBoardTab = useCallback((tabId: string | null) => {
    selectedTabIdIntentRef.current = tabId;
    setSelectedTabId(tabId);
  }, []);

  const applyRefreshedTabs = useCallback(
    (requestVersion: number, nextTabs: ResponseBoardTab[]) => {
      if (
        !isMountedRef.current ||
        requestVersion !== tabListRequestVersionRef.current
      ) {
        return;
      }
      const appliedTabs = pendingTabOrderRef.current
        ? mergeResponseBoardTabOrder(nextTabs, pendingTabOrderRef.current)
        : nextTabs;
      setTabs(appliedTabs);
      setTabRefreshError(null);
      const current = selectedTabIdIntentRef.current;
      const requested = activeBoardTabIdRef.current;
      const nextSelectedTabId =
        (current && appliedTabs.some((tab) => tab.id === current)
          ? current
          : requested && appliedTabs.some((tab) => tab.id === requested)
            ? requested
            : appliedTabs[0]?.id) ?? null;
      selectResponseBoardTab(nextSelectedTabId);
      if (!nextSelectedTabId) {
        lastWorkspaceTabListSyncRef.current = null;
        onNoTabsRef.current();
        return;
      }

      const nextWorkspaceView =
        nextSelectedTabId === selectedTabIdRef.current
          ? viewRef.current
          : (boardViewsRef.current[nextSelectedTabId] ?? {
              panX: 0,
              panY: 0,
              zoom: 1,
            });
      const knownTabIds = appliedTabs.map((tab) => tab.id);
      // Workspace persistence uses these ids only as a membership set when it
      // prunes stale per-tab camera state, so tab reorders need no extra sync.
      const workspaceSync = {
        workspaceTabId,
        selectedTabId: nextSelectedTabId,
        knownTabIdsSignature: JSON.stringify([...knownTabIds].sort()),
      };
      const previousWorkspaceSync = lastWorkspaceTabListSyncRef.current;
      if (
        previousWorkspaceSync?.workspaceTabId === workspaceSync.workspaceTabId &&
        previousWorkspaceSync.selectedTabId === workspaceSync.selectedTabId &&
        previousWorkspaceSync.knownTabIdsSignature ===
          workspaceSync.knownTabIdsSignature
      ) {
        return;
      }
      lastWorkspaceTabListSyncRef.current = workspaceSync;
      onWorkspaceStateChangeRef.current(
        workspaceTabId,
        nextSelectedTabId,
        nextWorkspaceView,
        knownTabIds,
      );
    },
    [
      activeBoardTabIdRef,
      boardViewsRef,
      onNoTabsRef,
      onWorkspaceStateChangeRef,
      selectResponseBoardTab,
      selectedTabIdRef,
      viewRef,
      workspaceTabId,
    ],
  );

  const refreshTabs = useCallback(
    async ({
      onFailure,
    }: {
      onFailure: (reason: unknown) => void;
    }) => {
      const requestVersion = ++tabListRequestVersionRef.current;
      let nextTabs: ResponseBoardTab[];
      try {
        const response = await fetchResponseBoardTabs();
        nextTabs = response.tabs;
      } catch (reason) {
        if (
          isMountedRef.current &&
          requestVersion === tabListRequestVersionRef.current
        ) {
          onFailure(reason);
        }
        return;
      }
      applyRefreshedTabs(requestVersion, nextTabs);
    },
    [applyRefreshedTabs],
  );

  const refreshTabsAfterCommittedMutation = useCallback(
    async (diagnosticContext: string, failureMessage: string) => {
      await refreshTabs({
        onFailure: (reason) => {
          console.warn(
            `[TermAl] response-board tab refresh failed after ${diagnosticContext}`,
            reason,
          );
          setTabRefreshError(failureMessage);
        },
      });
    },
    [refreshTabs],
  );

  const selectCreatedTab = useCallback(
    (tab: ResponseBoardTab) => {
      // The create response is authoritative and must invalidate reads served
      // before the commit. Route it through the shared application path so
      // selection and workspace membership survive a failed reconciliation.
      const requestVersion = ++tabListRequestVersionRef.current;
      selectedTabIdIntentRef.current = tab.id;
      const nextTabs = tabsRef.current.some(
        (candidate) => candidate.id === tab.id,
      )
        ? tabsRef.current.map((candidate) =>
            candidate.id === tab.id ? tab : candidate,
          )
        : [...tabsRef.current, tab].sort(
            (left, right) => left.sortOrder - right.sortOrder,
          );
      applyRefreshedTabs(
        requestVersion,
        nextTabs,
      );
    },
    [applyRefreshedTabs, tabsRef],
  );

  const applyRenamedTab = useCallback(
    (tab: ResponseBoardTab) => {
      // The mutation response is newer than every read started before it
      // committed. Apply it locally even if the reconciliation read fails.
      const requestVersion = ++tabListRequestVersionRef.current;
      applyRefreshedTabs(
        requestVersion,
        tabsRef.current.map((candidate) =>
          candidate.id === tab.id ? tab : candidate,
        ),
      );
    },
    [applyRefreshedTabs, tabsRef],
  );

  const removeDeletedTab = useCallback(
    (removedTabId: string) => {
      // A successful delete owns membership immediately. Older reads must not
      // resurrect the tab while the authoritative reconciliation is pending.
      const requestVersion = ++tabListRequestVersionRef.current;
      applyRefreshedTabs(
        requestVersion,
        tabsRef.current.filter((candidate) => candidate.id !== removedTabId),
      );
    },
    [applyRefreshedTabs, tabsRef],
  );

  const beginTabReorder = useCallback((nextTabs: ResponseBoardTab[]) => {
    const normalizedTabs = mergeResponseBoardTabOrder(nextTabs, nextTabs);
    pendingTabOrderRef.current = normalizedTabs;
    setTabs(normalizedTabs);
    tabListRequestVersionRef.current += 1;
  }, []);

  const commitTabReorder = useCallback((orderedTabs: ResponseBoardTab[]) => {
    pendingTabOrderRef.current = orderedTabs;
    tabListRequestVersionRef.current += 1;
    setTabs((currentTabs) =>
      mergeResponseBoardTabOrder(currentTabs, orderedTabs),
    );
  }, []);

  const rollbackTabReorder = useCallback((previousTabs: ResponseBoardTab[]) => {
    setTabs((currentTabs) =>
      mergeResponseBoardTabOrder(currentTabs, previousTabs),
    );
  }, []);

  const finishTabReorder = useCallback(() => {
    pendingTabOrderRef.current = null;
  }, []);

  const clearTabRefreshError = useCallback(() => {
    setTabRefreshError(null);
  }, []);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (activeBoardTabId) {
      selectResponseBoardTab(activeBoardTabId);
    }
  }, [activeBoardTabId, selectResponseBoardTab]);

  return {
    applyRenamedTab,
    beginTabReorder,
    clearTabRefreshError,
    commitTabReorder,
    finishTabReorder,
    refreshTabs,
    refreshTabsAfterCommittedMutation,
    removeDeletedTab,
    rollbackTabReorder,
    selectCreatedTab,
    selectedTabId,
    selectedTabIdRef,
    selectResponseBoardTab,
    tabRefreshError,
    tabs,
  };
}
