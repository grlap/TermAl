import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type DragEvent as ReactDragEvent,
  type MutableRefObject,
  type PointerEvent as ReactPointerEvent,
  type SetStateAction,
} from "react";
import { flushSync } from "react-dom";
import { getWorkspaceSplitResizeBounds } from "./workspace-queries";
import {
  TAB_DRAG_CHANNEL_NAME,
  attachWorkspaceTabDragData,
  createWorkspaceTabDrag,
  isWorkspaceTabDragChannelMessage,
  readWorkspaceTabDragData,
  type WorkspaceTabDrag,
  type WorkspaceTabDragChannelMessage,
} from "./tab-drag";
import { readSessionDragData } from "./session-drag";
import { clamp } from "./app-utils";
import {
  closeWorkspaceTab,
  getSplitRatio,
  placeDraggedTab,
  placeExternalTab,
  placeSessionDropInWorkspaceState,
  updateSplitRatio,
  type TabDropPlacement,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";
import type { ControlPanelSectionId } from "./panels/ControlPanelSurface";
import type { ControlPanelSide } from "./workspace-storage";
import { TAB_DRAG_STALE_TIMEOUT_MS } from "./app-shell-internals";
import type { PaneScrollPositionMigration } from "./pane-scroll-position-migration";

// Each accepted reducer result receives a per-drop Symbol token. It is
// enumerable so same-flush workspace spreads preserve the commit evidence,
// while JSON persistence still ignores symbol-keyed properties. The latest
// token intentionally remains on live state: later accepted drops replace the
// single property, and stale tokens are inert because verification compares
// per-drop identity immediately after flushSync.
const WORKSPACE_DROP_COMMIT_TOKEN = Symbol("workspaceDropCommitToken");

type WorkspaceWithDropCommitToken = WorkspaceState & {
  [WORKSPACE_DROP_COMMIT_TOKEN]?: symbol;
};

type UseAppDragResizeArgs = {
  windowId: string;
  workspace: WorkspaceState;
  paneLookup: Map<string, WorkspacePane>;
  controlPanelSide: ControlPanelSide;
  setControlPanelSide: Dispatch<SetStateAction<ControlPanelSide>>;
  // Wrappers around the React setter must preserve enumerable symbol keys.
  // Drops that create a gesture-owned tab id also carry independent structural
  // evidence, but moves or activations of existing tabs require the token. The
  // production app passes the raw setter. All cross-window transfers allocate a
  // fresh id, so their acknowledgement retains structural evidence even if a
  // wrapper strips the token. Only ambiguous same-window existing-tab drops
  // deliberately fail closed: the previous dock side is restored and the
  // invariant violation is warned once.
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: "left" | "right",
  ) => WorkspaceState;
  workspaceLayoutLoadPendingRef: MutableRefObject<boolean>;
  ignoreFetchedWorkspaceLayoutRef: MutableRefObject<boolean>;
  migrateSessionTabScrollPosition: (input: {
    sessionId: string;
    sourcePaneId: string;
    targetPaneId: string;
  }) => boolean;
  beginSessionTabScrollPositionMigration: (input: {
    sessionId: string;
    sourcePaneId: string;
    targetPaneId: string;
  }) => PaneScrollPositionMigration | null;
};

type WorkspaceDropExpectation = {
  allowsControlPanelSideOnly?: boolean;
  migrateSessionId?: string;
  migrateSessionSourcePaneId?: string;
  sourcePaneId?: string;
  structuralPlacementIsCommitEvidence?: boolean;
  tabId: string;
  targetPaneId: string;
};

type UseAppDragResizeResult = {
  activeDraggedTab: WorkspaceTabDrag | null;
  getKnownWorkspaceTabDrag: () => WorkspaceTabDrag | null;
  handleSplitResizeStart: (
    splitId: string,
    direction: "row" | "column",
    event: ReactPointerEvent<HTMLDivElement>,
  ) => void;
  handleTabDragStart: (drag: WorkspaceTabDrag) => void;
  handleTabDragEnd: () => void;
  handleControlPanelLauncherDragStart: (
    event: ReactDragEvent<HTMLButtonElement>,
    paneId: string,
    sectionId: ControlPanelSectionId,
    tab: WorkspaceTab,
  ) => void;
  handleControlPanelLauncherDragEnd: () => void;
  handleTabDrop: (
    targetPaneId: string,
    placement: TabDropPlacement,
    tabIndex?: number,
    dataTransfer?: DataTransfer | null,
  ) => void;
};

function resolveControlPanelSideAfterDrop(
  tab: WorkspaceTab,
  placement: TabDropPlacement,
  currentSide: ControlPanelSide,
): ControlPanelSide {
  return tab.kind === "controlPanel" &&
    (placement === "left" || placement === "right")
    ? placement
    : currentSide;
}

function workspaceContainsGestureTabId(
  workspace: WorkspaceState,
  expectation: WorkspaceDropExpectation,
) {
  // Every fallback-capable drop owns a unique tab id allocated before the
  // reducer runs. Its final presence is sufficient evidence for that gesture,
  // even when the control-panel layout pass relocates or nests the new pane.
  return workspace.panes.some((pane) =>
    pane.tabs.some((candidate) => candidate.id === expectation.tabId),
  );
}

function workspacePaneContainsTab(
  workspace: WorkspaceState,
  paneId: string,
  tabId: string,
) {
  return workspace.panes.some(
    (pane) =>
      pane.id === paneId && pane.tabs.some((tab) => tab.id === tabId),
  );
}

function markWorkspaceDropCommit(
  workspace: WorkspaceState,
  token: symbol,
): WorkspaceState {
  // Enumeration lets ordinary workspace spreads preserve this transient
  // reducer evidence; symbol keys remain outside the serializable model.
  const committedWorkspace = { ...workspace } as WorkspaceWithDropCommitToken;
  committedWorkspace[WORKSPACE_DROP_COMMIT_TOKEN] = token;
  return committedWorkspace;
}

function workspaceHasDropCommit(
  workspace: WorkspaceState,
  token: symbol,
): boolean {
  return (workspace as WorkspaceWithDropCommitToken)[
    WORKSPACE_DROP_COMMIT_TOKEN
  ] === token;
}

export function useAppDragResize({
  windowId,
  workspace,
  paneLookup,
  controlPanelSide,
  setControlPanelSide,
  setWorkspace,
  applyControlPanelLayout,
  workspaceLayoutLoadPendingRef,
  ignoreFetchedWorkspaceLayoutRef,
  beginSessionTabScrollPositionMigration,
  migrateSessionTabScrollPosition,
}: UseAppDragResizeArgs): UseAppDragResizeResult {
  const [draggedTab, setDraggedTab] = useState<WorkspaceTabDrag | null>(null);
  const [launcherDraggedTab, setLauncherDraggedTab] =
    useState<WorkspaceTabDrag | null>(null);
  const [externalDraggedTab, setExternalDraggedTab] =
    useState<WorkspaceTabDrag | null>(null);
  const resizeStateRef = useRef<{
    splitId: string;
    direction: "row" | "column";
    startRatio: number;
    minRatio: number;
    maxRatio: number;
    startX: number;
    startY: number;
    size: number;
  } | null>(null);
  const dragChannelRef = useRef<BroadcastChannel | null>(null);
  const draggedTabRef = useRef<WorkspaceTabDrag | null>(null);
  const launcherDraggedTabRef = useRef<WorkspaceTabDrag | null>(null);
  const workspaceRef = useRef(workspace);
  workspaceRef.current = workspace;
  const applyControlPanelLayoutRef = useRef(applyControlPanelLayout);
  applyControlPanelLayoutRef.current = applyControlPanelLayout;
  const warnedAboutStructuralDropFallbackRef = useRef(false);
  const warnedAboutUnverifiableDropRef = useRef(false);

  const broadcastTabDragMessage = useCallback(
    (message: WorkspaceTabDragChannelMessage) => {
      dragChannelRef.current?.postMessage(message);
    },
    [],
  );

  const clearStaleTabDragState = useCallback(() => {
    const endedDrag = draggedTabRef.current;
    draggedTabRef.current = null;
    setDraggedTab(null);
    launcherDraggedTabRef.current = null;
    setLauncherDraggedTab(null);
    setExternalDraggedTab(null);
    if (!endedDrag) {
      return;
    }

    broadcastTabDragMessage({
      type: "drag-end",
      dragId: endedDrag.dragId,
      sourceWindowId: endedDrag.sourceWindowId,
    });
  }, [broadcastTabDragMessage]);

  const getKnownWorkspaceTabDrag = useCallback(
    () =>
      draggedTabRef.current ??
      draggedTab ??
      launcherDraggedTabRef.current ??
      launcherDraggedTab ??
      externalDraggedTab,
    [draggedTab, launcherDraggedTab, externalDraggedTab],
  );

  const handleSplitResizeStart = useCallback(
    (
      splitId: string,
      direction: "row" | "column",
      event: ReactPointerEvent<HTMLDivElement>,
    ) => {
      event.preventDefault();
      event.stopPropagation();

      const container = event.currentTarget.parentElement;
      const ratio = getSplitRatio(workspace.root, splitId);
      if (!container || ratio === null) {
        return;
      }

      const rect = container.getBoundingClientRect();
      const { minRatio, maxRatio } = getWorkspaceSplitResizeBounds(
        workspace.root,
        splitId,
        direction,
        direction === "row" ? rect.width : rect.height,
        paneLookup,
      );
      resizeStateRef.current = {
        splitId,
        direction,
        startRatio: ratio,
        minRatio,
        maxRatio,
        startX: event.clientX,
        startY: event.clientY,
        size: direction === "row" ? rect.width : rect.height,
      };
    },
    [paneLookup, workspace.root],
  );

  const handleTabDragStart = useCallback(
    (drag: WorkspaceTabDrag) => {
      draggedTabRef.current = drag;
      setDraggedTab(drag);
      broadcastTabDragMessage({
        type: "drag-start",
        payload: drag,
      });
    },
    [broadcastTabDragMessage],
  );

  const handleTabDragEnd = useCallback(() => {
    const endedDrag = draggedTabRef.current;
    draggedTabRef.current = null;
    setDraggedTab(null);
    if (!endedDrag) {
      return;
    }

    broadcastTabDragMessage({
      type: "drag-end",
      dragId: endedDrag.dragId,
      sourceWindowId: endedDrag.sourceWindowId,
    });
  }, [broadcastTabDragMessage]);

  const handleControlPanelLauncherDragStart = useCallback(
    (
      event: ReactDragEvent<HTMLButtonElement>,
      paneId: string,
      sectionId: ControlPanelSectionId,
      tab: WorkspaceTab,
    ) => {
      const drag = createWorkspaceTabDrag(
        windowId,
        `control-panel-launcher:${paneId}:${sectionId}`,
        tab,
      );
      event.dataTransfer.effectAllowed = "copyMove";
      attachWorkspaceTabDragData(event.dataTransfer, drag);
      launcherDraggedTabRef.current = drag;
      setTimeout(() => setLauncherDraggedTab(drag), 0);
    },
    [windowId],
  );

  const handleControlPanelLauncherDragEnd = useCallback(() => {
    launcherDraggedTabRef.current = null;
    setLauncherDraggedTab(null);
  }, []);

  const commitWorkspaceDrop = useCallback(
    (
      placeWorkspace: (current: WorkspaceState) => WorkspaceState,
      expectation: WorkspaceDropExpectation,
      nextControlPanelSide: ControlPanelSide = controlPanelSide,
    ) => {
      const previewWorkspace = placeWorkspace(workspace);
      const changesControlPanelSide =
        nextControlPanelSide !== controlPanelSide;
      const allowsControlPanelSideOnly =
        expectation.allowsControlPanelSideOnly === true &&
        changesControlPanelSide &&
        expectation.sourcePaneId === expectation.targetPaneId;
      if (previewWorkspace === workspace && !allowsControlPanelSideOnly) {
        return false;
      }

      // The preview rejects known no-ops. The synchronous functional update
      // then safely rebases the accepted drop over queued workspace state.
      // After the commit, verify the actual pane/tab placement so a rebase-time
      // refusal cannot acknowledge a cross-window drop or migrate saved state.
      const commitToken = Symbol("workspaceDropCommit");
      const speculativeScrollMigration =
        expectation.migrateSessionId &&
        expectation.migrateSessionSourcePaneId
          ? beginSessionTabScrollPositionMigration({
              sessionId: expectation.migrateSessionId,
              sourcePaneId: expectation.migrateSessionSourcePaneId,
              targetPaneId: expectation.targetPaneId,
            })
          : null;
      let appliedNextControlPanelSide = false;
      let rebasedPlacementAccepted = false;
      flushSync(() => {
        setWorkspace((current) => {
          const placedWorkspace = placeWorkspace(current);
          rebasedPlacementAccepted = placedWorkspace !== current;
          const commitsExistingControlPanelSide =
            allowsControlPanelSideOnly &&
            workspacePaneContainsTab(
              current,
              expectation.targetPaneId,
              expectation.tabId,
            );
          const committedSide =
            placedWorkspace !== current || commitsExistingControlPanelSide
              ? nextControlPanelSide
              : controlPanelSide;
          // React may replay this updater, but the assignment is a deterministic
          // observation of its latest evaluation. It only avoids a redundant
          // recovery layout; drop acknowledgement still comes from state.
          appliedNextControlPanelSide =
            changesControlPanelSide &&
            committedSide === nextControlPanelSide;
          const laidOutWorkspace = applyControlPanelLayout(
            placedWorkspace,
            committedSide,
          );
          return placedWorkspace === current
            ? laidOutWorkspace
            : markWorkspaceDropCommit(laidOutWorkspace, commitToken);
        });
      });
      const commitsExistingControlPanelSide =
        allowsControlPanelSideOnly &&
        workspacePaneContainsTab(
          workspaceRef.current,
          expectation.targetPaneId,
          expectation.tabId,
        );
      const reducerCommitted = workspaceHasDropCommit(
        workspaceRef.current,
        commitToken,
      );
      // The exact token is authoritative: it is attached only when the
      // rebased placement reducer accepts this drop, before layout
      // normalization may relocate panes or restore other focus. Structural
      // matching is only a fallback for unambiguous drops when an outer
      // setter wrapper reconstructs state and sheds symbol-keyed metadata.
      const structurallyCommittedWithoutToken =
        !reducerCommitted &&
        expectation.structuralPlacementIsCommitEvidence === true &&
        workspaceContainsGestureTabId(workspaceRef.current, expectation);
      if (
        structurallyCommittedWithoutToken &&
        !warnedAboutStructuralDropFallbackRef.current
      ) {
        warnedAboutStructuralDropFallbackRef.current = true;
        console.warn(
          "[TermAl] Workspace setter stripped drop commit evidence; " +
            "acknowledging via the structural fallback. Preserve enumerable " +
            "symbol keys in workspace setter wrappers.",
        );
      }
      const missingCommitEvidenceAfterStateChange =
        rebasedPlacementAccepted &&
        !reducerCommitted &&
        !structurallyCommittedWithoutToken &&
        !commitsExistingControlPanelSide &&
        workspaceRef.current !== workspace;
      if (
        missingCommitEvidenceAfterStateChange &&
        !warnedAboutUnverifiableDropRef.current
      ) {
        warnedAboutUnverifiableDropRef.current = true;
        console.warn(
          "[TermAl] Could not verify a workspace drop after state changed; " +
            "the rebased reducer may have refused it, or a workspace setter " +
            "wrapper may have stripped enumerable Symbol commit evidence. " +
            "Rejecting post-commit side effects.",
        );
      }
      const didCommit =
        reducerCommitted ||
        structurallyCommittedWithoutToken ||
        commitsExistingControlPanelSide;
      if (!didCommit) {
        speculativeScrollMigration?.rollback();
        if (appliedNextControlPanelSide) {
          // The placement reducer may have committed before an invalid setter
          // wrapper stripped its token. Keep the visible dock consistent with
          // the unpublished preference by restoring the prior side.
          flushSync(() => {
            setWorkspace((current) =>
              applyControlPanelLayout(current, controlPanelSide),
            );
          });
        }
      } else if (changesControlPanelSide) {
        // Only publish the dock-side preference after the rebased workspace
        // contains the transferred tab. A local control-panel edge gesture is
        // the sole side-only commit because it moves the existing dock.
        flushSync(() => setControlPanelSide(nextControlPanelSide));
      }
      if (
        didCommit &&
        expectation.migrateSessionId &&
        expectation.migrateSessionSourcePaneId
      ) {
        const committedPane = workspaceRef.current.panes.find((pane) =>
          pane.tabs.some((tab) => tab.id === expectation.tabId),
        );
        if (committedPane?.id !== expectation.targetPaneId) {
          speculativeScrollMigration?.rollback();
        }
        if (
          committedPane &&
          committedPane.id !== expectation.targetPaneId
        ) {
          migrateSessionTabScrollPosition({
            sessionId: expectation.migrateSessionId,
            sourcePaneId: expectation.migrateSessionSourcePaneId,
            targetPaneId: committedPane.id,
          });
        }
      }
      return didCommit;
    },
    [
      applyControlPanelLayout,
      beginSessionTabScrollPositionMigration,
      controlPanelSide,
      migrateSessionTabScrollPosition,
      setControlPanelSide,
      setWorkspace,
      workspace,
    ],
  );

  const handleTabDrop = useCallback(
    (
      targetPaneId: string,
      placement: TabDropPlacement,
      tabIndex?: number,
      dataTransfer?: DataTransfer | null,
    ) => {
      const droppedSession = readSessionDragData(dataTransfer ?? null);
      if (droppedSession) {
        // This gesture-owned id is used only if the rebased reducer must
        // create a tab. Its final presence is therefore unambiguous fallback
        // evidence when an outer state wrapper sheds the commit token.
        const newSessionTabId = crypto.randomUUID();
        const existingSessionTab = workspace.panes
          .flatMap((pane) =>
            pane.tabs.map((tab) => ({ paneId: pane.id, tab })),
          )
          .find(
            (entry) =>
              entry.tab.kind === "session" &&
              entry.tab.sessionId === droppedSession.sessionId,
          );
        commitWorkspaceDrop(
          (current) =>
            placeSessionDropInWorkspaceState(
              current,
              droppedSession.sessionId,
              targetPaneId,
              placement,
              tabIndex,
              newSessionTabId,
            ),
          {
            migrateSessionId: existingSessionTab
              ? droppedSession.sessionId
              : undefined,
            migrateSessionSourcePaneId: existingSessionTab?.paneId,
            structuralPlacementIsCommitEvidence: !existingSessionTab,
            tabId: existingSessionTab?.tab.id ?? newSessionTabId,
            targetPaneId,
          },
        );
        return;
      }

      const parsedDrag = readWorkspaceTabDragData(dataTransfer);
      const sameWindowParsedDrag =
        parsedDrag && parsedDrag.sourceWindowId === windowId ? parsedDrag : null;
      const parsedLauncherDrag = sameWindowParsedDrag?.sourcePaneId.startsWith(
        "control-panel-launcher:",
      )
        ? sameWindowParsedDrag
        : null;
      const parsedPaneDrag =
        sameWindowParsedDrag &&
        !sameWindowParsedDrag.sourcePaneId.startsWith("control-panel-launcher:")
          ? sameWindowParsedDrag
          : null;
      const currentDraggedTab =
        draggedTabRef.current ?? draggedTab ?? parsedPaneDrag;
      const currentLauncherDraggedTab =
        launcherDraggedTabRef.current ??
        launcherDraggedTab ??
        parsedLauncherDrag;
      const currentExternalDraggedTab =
        externalDraggedTab ??
        (parsedDrag && parsedDrag.sourceWindowId !== windowId
          ? parsedDrag
          : null);

      if (currentDraggedTab) {
        const drop = currentDraggedTab;
        const splitPaneId =
          placement === "tabs" ? null : crypto.randomUUID();
        const expectedCommittedPaneId =
          splitPaneId &&
          !(
            drop.tab.kind === "controlPanel" &&
            drop.sourcePaneId === targetPaneId
          )
            ? splitPaneId
            : targetPaneId;
        draggedTabRef.current = null;
        setDraggedTab(null);
        const nextControlPanelSide = resolveControlPanelSideAfterDrop(
          drop.tab,
          placement,
          controlPanelSide,
        );
        commitWorkspaceDrop(
          (current) =>
            placeDraggedTab(
              current,
              drop.sourcePaneId,
              drop.tabId,
              targetPaneId,
              placement,
              tabIndex,
              splitPaneId ?? undefined,
            ),
          {
            allowsControlPanelSideOnly:
              drop.tab.kind === "controlPanel" &&
              (placement === "left" || placement === "right"),
            sourcePaneId: drop.sourcePaneId,
            migrateSessionId:
              drop.tab.kind === "session" ? drop.tab.sessionId : undefined,
            migrateSessionSourcePaneId:
              drop.tab.kind === "session" ? drop.sourcePaneId : undefined,
            tabId: drop.tabId,
            targetPaneId: expectedCommittedPaneId,
          },
          nextControlPanelSide,
        );
        return;
      }

      if (currentLauncherDraggedTab) {
        const drop = currentLauncherDraggedTab;
        const transferredTabId = crypto.randomUUID();
        launcherDraggedTabRef.current = null;
        setLauncherDraggedTab(null);
        commitWorkspaceDrop(
          (current) =>
            placeExternalTab(
              current,
              drop.tab,
              targetPaneId,
              placement,
              tabIndex,
              transferredTabId,
            ),
          {
            structuralPlacementIsCommitEvidence: true,
            tabId: transferredTabId,
            targetPaneId,
          },
        );
        return;
      }

      if (!currentExternalDraggedTab) {
        return;
      }

      const drop = currentExternalDraggedTab;
      const transferredTabId = crypto.randomUUID();
      setExternalDraggedTab((current) =>
        current?.dragId === drop.dragId ? null : current,
      );
      const nextControlPanelSide = resolveControlPanelSideAfterDrop(
        drop.tab,
        placement,
        controlPanelSide,
      );
      const didCommit = commitWorkspaceDrop(
        (current) =>
          placeExternalTab(
            current,
            drop.tab,
            targetPaneId,
            placement,
            tabIndex,
            transferredTabId,
          ),
        {
          structuralPlacementIsCommitEvidence: true,
          tabId: transferredTabId,
          targetPaneId,
        },
        nextControlPanelSide,
      );
      if (didCommit) {
        broadcastTabDragMessage({
          type: "drop-commit",
          dragId: drop.dragId,
          sourceWindowId: drop.sourceWindowId,
          sourcePaneId: drop.sourcePaneId,
          tabId: drop.tabId,
          targetWindowId: windowId,
        });
      }
      broadcastTabDragMessage({
        type: "drag-end",
        dragId: drop.dragId,
        sourceWindowId: drop.sourceWindowId,
      });
    },
    [
      broadcastTabDragMessage,
      commitWorkspaceDrop,
      controlPanelSide,
      draggedTab,
      externalDraggedTab,
      launcherDraggedTab,
      windowId,
      workspace,
    ],
  );

  useEffect(() => {
    if (typeof BroadcastChannel === "undefined") {
      return;
    }

    const channel = new BroadcastChannel(TAB_DRAG_CHANNEL_NAME);
    dragChannelRef.current = channel;
    channel.onmessage = (event: MessageEvent<unknown>) => {
      const message = event.data;
      if (!isWorkspaceTabDragChannelMessage(message)) {
        return;
      }

      switch (message.type) {
        case "drag-start":
          if (message.payload.sourceWindowId !== windowId) {
            setExternalDraggedTab(message.payload);
          }
          break;
        case "drag-end":
          setExternalDraggedTab((current) =>
            current?.dragId === message.dragId ? null : current,
          );
          break;
        case "drop-commit":
          if (message.sourceWindowId !== windowId) {
            break;
          }

          if (draggedTabRef.current?.dragId === message.dragId) {
            draggedTabRef.current = null;
          }
          setDraggedTab((current) =>
            current?.dragId === message.dragId ? null : current,
          );
          setWorkspace((current) =>
            applyControlPanelLayoutRef.current(
              closeWorkspaceTab(current, message.sourcePaneId, message.tabId),
            ),
          );
          break;
      }
    };

    return () => {
      channel.close();
      if (dragChannelRef.current === channel) {
        dragChannelRef.current = null;
      }
    };
  }, [setWorkspace, windowId]);

  useEffect(() => {
    function handlePointerMove(event: PointerEvent) {
      const resizeState = resizeStateRef.current;
      if (!resizeState) {
        return;
      }

      const delta =
        resizeState.direction === "row"
          ? event.clientX - resizeState.startX
          : event.clientY - resizeState.startY;
      const nextRatio = clamp(
        resizeState.startRatio + delta / Math.max(resizeState.size, 1),
        resizeState.minRatio,
        resizeState.maxRatio,
      );
      if (
        workspaceLayoutLoadPendingRef.current &&
        nextRatio !== resizeState.startRatio
      ) {
        ignoreFetchedWorkspaceLayoutRef.current = true;
      }

      setWorkspace((current) =>
        updateSplitRatio(current, resizeState.splitId, nextRatio),
      );
    }

    function handlePointerUp() {
      resizeStateRef.current = null;
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
  }, [
    ignoreFetchedWorkspaceLayoutRef,
    setWorkspace,
    workspaceLayoutLoadPendingRef,
  ]);

  useEffect(() => {
    if (!draggedTab && !launcherDraggedTab && !externalDraggedTab) {
      return;
    }

    const handleWindowBlur = () => {
      clearStaleTabDragState();
    };
    const handlePageHide = () => {
      clearStaleTabDragState();
    };
    const handleVisibilityChange = () => {
      if (document.visibilityState === "hidden") {
        clearStaleTabDragState();
      }
    };
    const timeoutId = window.setTimeout(() => {
      clearStaleTabDragState();
    }, TAB_DRAG_STALE_TIMEOUT_MS);

    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("pagehide", handlePageHide);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      window.clearTimeout(timeoutId);
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("pagehide", handlePageHide);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [
    clearStaleTabDragState,
    draggedTab,
    externalDraggedTab,
    launcherDraggedTab,
  ]);

  return {
    activeDraggedTab: draggedTab ?? launcherDraggedTab ?? externalDraggedTab,
    getKnownWorkspaceTabDrag,
    handleSplitResizeStart,
    handleTabDragStart,
    handleTabDragEnd,
    handleControlPanelLauncherDragStart,
    handleControlPanelLauncherDragEnd,
    handleTabDrop,
  };
}
