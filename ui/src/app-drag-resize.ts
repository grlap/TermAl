import {
  startTransition,
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

type UseAppDragResizeArgs = {
  windowId: string;
  workspace: WorkspaceState;
  paneLookup: Map<string, WorkspacePane>;
  controlPanelSide: ControlPanelSide;
  setControlPanelSide: Dispatch<SetStateAction<ControlPanelSide>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: "left" | "right",
  ) => WorkspaceState;
  workspaceLayoutLoadPendingRef: MutableRefObject<boolean>;
  ignoreFetchedWorkspaceLayoutRef: MutableRefObject<boolean>;
  markSessionTabsForBottomAfterWorkspaceRebuild: (
    workspaceState: WorkspaceState,
    options?: {
      sessionIds?: string[];
      tabs?: WorkspaceTab[];
    },
  ) => void;
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
  markSessionTabsForBottomAfterWorkspaceRebuild,
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

  const handleTabDrop = useCallback(
    (
      targetPaneId: string,
      placement: TabDropPlacement,
      tabIndex?: number,
      dataTransfer?: DataTransfer | null,
    ) => {
      const droppedSession = readSessionDragData(dataTransfer ?? null);
      if (droppedSession) {
        markSessionTabsForBottomAfterWorkspaceRebuild(workspace, {
          sessionIds: [droppedSession.sessionId],
        });
        startTransition(() => {
          setWorkspace((current) => {
            const nextWorkspace = placeSessionDropInWorkspaceState(
              current,
              droppedSession.sessionId,
              targetPaneId,
              placement,
              tabIndex,
            );
            return applyControlPanelLayout(nextWorkspace, controlPanelSide);
          });
        });
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
        markSessionTabsForBottomAfterWorkspaceRebuild(workspace, {
          tabs: [drop.tab],
        });
        draggedTabRef.current = null;
        setDraggedTab(null);
        const nextControlPanelSide =
          drop.tab.kind === "controlPanel" &&
          (placement === "left" || placement === "right")
            ? placement
            : controlPanelSide;
        if (nextControlPanelSide !== controlPanelSide) {
          setControlPanelSide(nextControlPanelSide);
        }
        startTransition(() => {
          setWorkspace((current) =>
            applyControlPanelLayout(
              placeDraggedTab(
                current,
                drop.sourcePaneId,
                drop.tabId,
                targetPaneId,
                placement,
                tabIndex,
              ),
              nextControlPanelSide,
            ),
          );
        });
        return;
      }

      if (currentLauncherDraggedTab) {
        const drop = currentLauncherDraggedTab;
        markSessionTabsForBottomAfterWorkspaceRebuild(workspace, {
          tabs: [drop.tab],
        });
        launcherDraggedTabRef.current = null;
        setLauncherDraggedTab(null);
        flushSync(() => {
          setWorkspace((current) =>
            applyControlPanelLayout(
              placeExternalTab(
                current,
                drop.tab,
                targetPaneId,
                placement,
                tabIndex,
              ),
            ),
          );
        });
        return;
      }

      if (!currentExternalDraggedTab) {
        return;
      }

      const drop = currentExternalDraggedTab;
      markSessionTabsForBottomAfterWorkspaceRebuild(workspace, {
        tabs: [drop.tab],
      });
      setExternalDraggedTab((current) =>
        current?.dragId === drop.dragId ? null : current,
      );
      const nextControlPanelSide =
        drop.tab.kind === "controlPanel" &&
        (placement === "left" || placement === "right")
          ? placement
          : controlPanelSide;
      if (nextControlPanelSide !== controlPanelSide) {
        setControlPanelSide(nextControlPanelSide);
      }
      flushSync(() => {
        setWorkspace((current) =>
          applyControlPanelLayout(
            placeExternalTab(
              current,
              drop.tab,
              targetPaneId,
              placement,
              tabIndex,
            ),
            nextControlPanelSide,
          ),
        );
      });
      broadcastTabDragMessage({
        type: "drop-commit",
        dragId: drop.dragId,
        sourceWindowId: drop.sourceWindowId,
        sourcePaneId: drop.sourcePaneId,
        tabId: drop.tabId,
        targetWindowId: windowId,
      });
      broadcastTabDragMessage({
        type: "drag-end",
        dragId: drop.dragId,
        sourceWindowId: drop.sourceWindowId,
      });
    },
    [
      applyControlPanelLayout,
      broadcastTabDragMessage,
      controlPanelSide,
      draggedTab,
      externalDraggedTab,
      launcherDraggedTab,
      markSessionTabsForBottomAfterWorkspaceRebuild,
      setControlPanelSide,
      setWorkspace,
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
            applyControlPanelLayout(
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
  }, [applyControlPanelLayout, setWorkspace, windowId]);

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
