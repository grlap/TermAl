// app-workspace-layout.ts
//
// Owns: the workspace-layout persistence lifecycle plus the
// workspace-switcher refresh/delete control flow that used to
// live inline in App.tsx. That includes the workspace-summary
// request-token guards (`beginWorkspaceSummariesRequest`,
// `isLatestWorkspaceSummariesRequest`), the workspace-summaries
// state + ref, the switcher loading/error state, the
// deleting-workspace-ids state + ref, the pending-layout-save
// refs and their flush helpers (`clearPendingWorkspaceLayoutSave
// Timeout`, `persistPendingWorkspaceLayoutSave`,
// `flushPendingWorkspaceLayoutSave`, `flushWorkspaceLayoutSave
// Ref`), the fetch-layout effect that flips
// `isWorkspaceLayoutReady` (including the workspace-restart
// recovery notice via `workspaceLayoutRestartErrorMessageRef`),
// the persist-layout effect, and the single `pagehide` listener
// that keeps the pending layout save alive across unloads.
//
// Does not own: the switcher open/closed UI state
// (`isWorkspaceSwitcherOpen` / `setIsWorkspaceSwitcherOpen` stay
// in App.tsx), the JSX that renders the switcher or the
// restart-required notice, the generic backend-connection
// state, or the workspace/session/projects/orchestrator state
// the effects merely read. The outside-click and
// refresh-on-switcher-open effects also stay in App.tsx because
// they couple to `isWorkspaceSwitcherOpen`.
//
// Split out of: ui/src/App.tsx (Slice 12 of the App-split plan,
// see docs/app-split-plan.md).

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import {
  deleteWorkspaceLayout,
  fetchWorkspaceLayout,
  fetchWorkspaceLayouts,
  isBackendUnavailableError,
  saveWorkspaceLayout,
  type WorkspaceLayoutSummary,
} from "./api";
import { hydrateControlPanelLayout } from "./control-panel-layout";
import type { BackendConnectionState } from "./backend-connection";
import { resolveRecoveredWorkspaceLayoutRequestError } from "./state-adoption";
import {
  stripDiffPreviewDocumentContentFromWorkspaceState,
  stripLoadingGitDiffPreviewTabsFromWorkspaceState,
  type WorkspaceState,
} from "./workspace";
import {
  createWorkspaceViewId,
  deleteStoredWorkspaceLayout,
  parseStoredWorkspaceLayout,
  persistWorkspaceLayout,
  type ControlPanelSide,
  WORKSPACE_VIEW_QUERY_PARAM,
} from "./workspace-storage";
import type {
  DiagramLook,
  DiagramPalette,
  DiagramThemeOverrideMode,
  MarkdownStyleId,
  MarkdownThemeId,
  StyleId,
  ThemeId,
} from "./themes";
import { getErrorMessage } from "./app-utils";
import {
  WORKSPACE_LAYOUT_PERSIST_DELAY_MS,
  type PendingWorkspaceLayoutSave,
  type WorkspaceLayoutPersistencePayload,
} from "./app-shell-internals";

export type UseAppWorkspaceLayoutParams = {
  workspaceViewId: string;
  workspace: WorkspaceState;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  controlPanelSide: ControlPanelSide;
  setControlPanelSide: Dispatch<SetStateAction<ControlPanelSide>>;
  preferences: {
    themeId: ThemeId;
    styleId: StyleId;
    markdownThemeId: MarkdownThemeId;
    markdownStyleId: MarkdownStyleId;
    diagramThemeOverrideMode: DiagramThemeOverrideMode;
    diagramLook: DiagramLook;
    diagramPalette: DiagramPalette;
    fontSizePx: number;
    editorFontSizePx: number;
    densityPercent: number;
  };
  setPreferences: {
    setThemeId: Dispatch<SetStateAction<ThemeId>>;
    setStyleId: Dispatch<SetStateAction<StyleId>>;
    setMarkdownThemeId: Dispatch<SetStateAction<MarkdownThemeId>>;
    setMarkdownStyleId: Dispatch<SetStateAction<MarkdownStyleId>>;
    setDiagramThemeOverrideMode: Dispatch<
      SetStateAction<DiagramThemeOverrideMode>
    >;
    setDiagramLook: Dispatch<SetStateAction<DiagramLook>>;
    setDiagramPalette: Dispatch<SetStateAction<DiagramPalette>>;
    setFontSizePx: Dispatch<SetStateAction<number>>;
    setEditorFontSizePx: Dispatch<SetStateAction<number>>;
    setDensityPercent: Dispatch<SetStateAction<number>>;
  };
  setIsWorkspaceSwitcherOpen: Dispatch<SetStateAction<boolean>>;
  setRequestError: Dispatch<SetStateAction<string | null>>;
  isMountedRef: MutableRefObject<boolean>;
  clearRecoveredBackendRequestError: () => void;
  setBackendConnectionState: (state: BackendConnectionState) => void;
  reportRequestError: (
    error: unknown,
    options?: { message?: string },
  ) => void;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: ControlPanelSide,
  ) => WorkspaceState;
};

export type UseAppWorkspaceLayoutReturn = {
  isWorkspaceLayoutReady: boolean;
  workspaceSummaries: WorkspaceLayoutSummary[];
  workspaceSummariesRef: MutableRefObject<WorkspaceLayoutSummary[]>;
  setWorkspaceSummaries: Dispatch<SetStateAction<WorkspaceLayoutSummary[]>>;
  isWorkspaceSwitcherLoading: boolean;
  workspaceSwitcherError: string | null;
  deletingWorkspaceIds: string[];
  ignoreFetchedWorkspaceLayoutRef: MutableRefObject<boolean>;
  workspaceLayoutLoadPendingRef: MutableRefObject<boolean>;
  pendingWorkspaceLayoutSaveRef: MutableRefObject<PendingWorkspaceLayoutSave | null>;
  flushWorkspaceLayoutSaveRef: MutableRefObject<
    (options?: { keepalive?: boolean }) => void
  >;
  refreshWorkspaceSummaries: () => Promise<void>;
  flushPendingWorkspaceLayoutSave: (options?: { keepalive?: boolean }) => void;
  navigateToWorkspace: (nextWorkspaceViewId: string) => void;
  handleWorkspaceSwitcherToggle: () => void;
  handleOpenWorkspaceHere: (nextWorkspaceViewId: string) => void;
  handleOpenNewWorkspaceHere: () => void;
  handleOpenNewWorkspaceWindow: () => void;
  handleDeleteWorkspace: (workspaceId: string) => Promise<void>;
};

export function useAppWorkspaceLayout(
  params: UseAppWorkspaceLayoutParams,
): UseAppWorkspaceLayoutReturn {
  const {
    workspaceViewId,
    workspace,
    setWorkspace,
    controlPanelSide,
    setControlPanelSide,
    preferences,
    setPreferences,
    setIsWorkspaceSwitcherOpen,
    setRequestError,
    isMountedRef,
    clearRecoveredBackendRequestError,
    setBackendConnectionState,
    reportRequestError,
    applyControlPanelLayout,
  } = params;
  const {
    themeId,
    styleId,
    markdownThemeId,
    markdownStyleId,
    diagramThemeOverrideMode,
    diagramLook,
    diagramPalette,
    fontSizePx,
    editorFontSizePx,
    densityPercent,
  } = preferences;
  const {
    setThemeId,
    setStyleId,
    setMarkdownThemeId,
    setMarkdownStyleId,
    setDiagramThemeOverrideMode,
    setDiagramLook,
    setDiagramPalette,
    setFontSizePx,
    setEditorFontSizePx,
    setDensityPercent,
  } = setPreferences;

  const [isWorkspaceLayoutReady, setIsWorkspaceLayoutReady] = useState(false);
  const [workspaceSummaries, setWorkspaceSummaries] = useState<
    WorkspaceLayoutSummary[]
  >([]);
  const [isWorkspaceSwitcherLoading, setIsWorkspaceSwitcherLoading] =
    useState(false);
  const [workspaceSwitcherError, setWorkspaceSwitcherError] = useState<
    string | null
  >(null);
  const [deletingWorkspaceIds, setDeletingWorkspaceIds] = useState<string[]>(
    [],
  );

  const ignoreFetchedWorkspaceLayoutRef = useRef(false);
  const workspaceLayoutRestartErrorMessageRef = useRef<string | null>(null);
  const workspaceLayoutLoadPendingRef = useRef(false);
  const workspaceSummariesRequestTokenRef = useRef(0);
  const deletingWorkspaceIdsRef = useRef<Set<string>>(new Set());
  const pendingWorkspaceLayoutSaveRef =
    useRef<PendingWorkspaceLayoutSave | null>(null);
  const pendingWorkspaceLayoutSaveTimeoutRef = useRef<number | null>(null);
  const flushWorkspaceLayoutSaveRef = useRef<
    (options?: { keepalive?: boolean }) => void
  >(() => {});
  const workspaceSummariesRef = useRef(workspaceSummaries);

  useEffect(() => {
    workspaceSummariesRef.current = workspaceSummaries;
  }, [workspaceSummaries]);

  function beginWorkspaceSummariesRequest() {
    workspaceSummariesRequestTokenRef.current += 1;
    return workspaceSummariesRequestTokenRef.current;
  }

  function isLatestWorkspaceSummariesRequest(requestToken: number) {
    return workspaceSummariesRequestTokenRef.current === requestToken;
  }

  function finishDeletingWorkspace(workspaceId: string) {
    const nextDeletingWorkspaceIds = new Set(deletingWorkspaceIdsRef.current);
    nextDeletingWorkspaceIds.delete(workspaceId);
    deletingWorkspaceIdsRef.current = nextDeletingWorkspaceIds;
    if (isMountedRef.current) {
      setDeletingWorkspaceIds([...nextDeletingWorkspaceIds]);
    }
  }

  const refreshWorkspaceSummaries = useCallback(async () => {
    const requestToken = beginWorkspaceSummariesRequest();
    const workspacesAtRequest = workspaceSummariesRef.current;
    setIsWorkspaceSwitcherLoading(true);
    setWorkspaceSwitcherError(null);
    try {
      const response = await fetchWorkspaceLayouts();
      if (
        !isMountedRef.current ||
        !isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        return;
      }
      // Only apply the refresh result when the workspace list has not been
      // updated by another source (SSE-delivered workspace data, a delete
      // handler, etc.) during the fetch. This avoids overwriting a more
      // authoritative SSE-delivered list with a stale /api/workspaces
      // snapshot, while still applying the result when only unrelated
      // session/orchestrator events arrived.
      if (workspaceSummariesRef.current === workspacesAtRequest) {
        workspaceSummariesRef.current = response.workspaces;
        setWorkspaceSummaries(response.workspaces);
      }
    } catch (error) {
      if (
        !isMountedRef.current ||
        !isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        return;
      }
      setWorkspaceSwitcherError(getErrorMessage(error));
    } finally {
      if (
        isMountedRef.current &&
        isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        setIsWorkspaceSwitcherLoading(false);
      }
    }
    // All dependencies are stable callbacks or refs, so re-subscribing only
    // happens if the browser-recovery handler itself changes.
  }, [clearRecoveredBackendRequestError, setBackendConnectionState]);

  function clearPendingWorkspaceLayoutSaveTimeout() {
    if (
      pendingWorkspaceLayoutSaveTimeoutRef.current === null ||
      typeof window === "undefined"
    ) {
      return;
    }

    window.clearTimeout(pendingWorkspaceLayoutSaveTimeoutRef.current);
    pendingWorkspaceLayoutSaveTimeoutRef.current = null;
  }

  function persistPendingWorkspaceLayoutSave(
    pendingSave: PendingWorkspaceLayoutSave,
    options?: { keepalive?: boolean },
  ) {
    void saveWorkspaceLayout(
      pendingSave.workspaceId,
      pendingSave.layout,
      options?.keepalive ? { keepalive: true } : undefined,
    ).catch((error) => {
      console.warn(
        "workspace layout warning> failed to save server workspace layout:",
        error,
      );
    });
  }

  function flushPendingWorkspaceLayoutSave(options?: { keepalive?: boolean }) {
    clearPendingWorkspaceLayoutSaveTimeout();
    const pendingSave = pendingWorkspaceLayoutSaveRef.current;
    if (!pendingSave) {
      return;
    }

    pendingWorkspaceLayoutSaveRef.current = null;
    persistPendingWorkspaceLayoutSave(pendingSave, options);
  }

  flushWorkspaceLayoutSaveRef.current = flushPendingWorkspaceLayoutSave;

  function navigateToWorkspace(nextWorkspaceViewId: string) {
    if (typeof window === "undefined") {
      return;
    }

    flushPendingWorkspaceLayoutSave({ keepalive: true });
    const url = new URL(window.location.href);
    url.searchParams.set(WORKSPACE_VIEW_QUERY_PARAM, nextWorkspaceViewId);
    window.location.assign(url.toString());
  }

  function handleWorkspaceSwitcherToggle() {
    setIsWorkspaceSwitcherOpen((current) => !current);
  }

  function handleOpenWorkspaceHere(nextWorkspaceViewId: string) {
    setIsWorkspaceSwitcherOpen(false);
    if (nextWorkspaceViewId === workspaceViewId) {
      return;
    }
    navigateToWorkspace(nextWorkspaceViewId);
  }

  function handleOpenNewWorkspaceHere() {
    handleOpenWorkspaceHere(createWorkspaceViewId());
  }

  function handleOpenNewWorkspaceWindow() {
    if (typeof window === "undefined") {
      return;
    }

    const nextWorkspaceViewId = createWorkspaceViewId();
    flushPendingWorkspaceLayoutSave({ keepalive: true });
    const url = new URL(window.location.href);
    url.searchParams.set(WORKSPACE_VIEW_QUERY_PARAM, nextWorkspaceViewId);
    window.open(url.toString(), "_blank", "noopener");
    setIsWorkspaceSwitcherOpen(false);
  }

  async function handleDeleteWorkspace(workspaceId: string) {
    if (
      workspaceId === workspaceViewId ||
      deletingWorkspaceIdsRef.current.has(workspaceId)
    ) {
      return;
    }

    const nextDeletingWorkspaceIds = new Set(deletingWorkspaceIdsRef.current);
    nextDeletingWorkspaceIds.add(workspaceId);
    deletingWorkspaceIdsRef.current = nextDeletingWorkspaceIds;
    setDeletingWorkspaceIds([...nextDeletingWorkspaceIds]);
    setWorkspaceSwitcherError(null);

    const requestToken = beginWorkspaceSummariesRequest();
    const workspacesAtRequest = workspaceSummariesRef.current;
    setIsWorkspaceSwitcherLoading(true);
    try {
      const deleteResponse = await deleteWorkspaceLayout(workspaceId);
      deleteStoredWorkspaceLayout(workspaceId);
      if (isMountedRef.current) {
        if (
          isLatestWorkspaceSummariesRequest(requestToken) &&
          workspaceSummariesRef.current === workspacesAtRequest
        ) {
          // This is the latest workspace request and the workspace list
          // has not been updated by another source (SSE, another delete,
          // a refresh) during the flight: the server's post-delete list is
          // the most up-to-date view and safely reflects concurrent
          // cross-tab operations.
          workspaceSummariesRef.current = deleteResponse.workspaces;
          setWorkspaceSummaries(deleteResponse.workspaces);
        } else {
          // Either a newer workspace request was initiated (e.g. a refresh)
          // or the workspace list was updated by SSE / another handler
          // during the delete. Don't replace the entire list (the newer
          // source is more authoritative), but ensure the confirmed-deleted
          // workspace is removed locally.
          setWorkspaceSummaries((current) => {
            const next = current.filter((w) => w.id !== workspaceId);
            workspaceSummariesRef.current = next;
            return next;
          });
        }
      }
    } catch (error) {
      if (
        isMountedRef.current &&
        isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        setWorkspaceSwitcherError(getErrorMessage(error));
      }
    } finally {
      finishDeletingWorkspace(workspaceId);
      if (
        isMountedRef.current &&
        isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        setIsWorkspaceSwitcherLoading(false);
      }
    }
  }

  useEffect(() => {
    let cancelled = false;
    workspaceLayoutLoadPendingRef.current = true;
    ignoreFetchedWorkspaceLayoutRef.current = false;
    setIsWorkspaceLayoutReady(false);

    void fetchWorkspaceLayout(workspaceViewId)
      .then((response) => {
        if (cancelled) {
          return;
        }

        const nextLayout = response
          ? parseStoredWorkspaceLayout(
              JSON.stringify({
                controlPanelSide: response.layout.controlPanelSide,
                themeId: response.layout.themeId,
                styleId: response.layout.styleId,
                markdownThemeId: response.layout.markdownThemeId,
                markdownStyleId: response.layout.markdownStyleId,
                diagramThemeOverrideMode: response.layout.diagramThemeOverrideMode,
                diagramLook: response.layout.diagramLook,
                diagramPalette: response.layout.diagramPalette,
                fontSizePx: response.layout.fontSizePx,
                editorFontSizePx: response.layout.editorFontSizePx,
                densityPercent: response.layout.densityPercent,
                workspace: response.layout.workspace,
              }),
            )
          : null;

        if (nextLayout) {
          const shouldApplyFetchedWorkspaceLayout =
            !ignoreFetchedWorkspaceLayoutRef.current;
          // A manual layout change during hydration claims the workspace tree
          // and dock side locally, but still allows the server-stored visual
          // preferences to merge in once the fetch resolves.
          if (shouldApplyFetchedWorkspaceLayout) {
            setControlPanelSide(nextLayout.controlPanelSide);
          }
          if (nextLayout.themeId) {
            setThemeId(nextLayout.themeId);
          }
          if (nextLayout.styleId) {
            setStyleId(nextLayout.styleId);
          }
          if (nextLayout.markdownThemeId) {
            setMarkdownThemeId(nextLayout.markdownThemeId);
          }
          if (nextLayout.markdownStyleId) {
            setMarkdownStyleId(nextLayout.markdownStyleId);
          }
          if (nextLayout.diagramThemeOverrideMode) {
            setDiagramThemeOverrideMode(nextLayout.diagramThemeOverrideMode);
          }
          if (nextLayout.diagramLook) {
            setDiagramLook(nextLayout.diagramLook);
          }
          if (nextLayout.diagramPalette) {
            setDiagramPalette(nextLayout.diagramPalette);
          }
          if (nextLayout.fontSizePx !== undefined) {
            setFontSizePx(nextLayout.fontSizePx);
          }
          if (nextLayout.editorFontSizePx !== undefined) {
            setEditorFontSizePx(nextLayout.editorFontSizePx);
          }
          if (nextLayout.densityPercent !== undefined) {
            setDensityPercent(nextLayout.densityPercent);
          }
          if (shouldApplyFetchedWorkspaceLayout) {
            setWorkspace(
              hydrateControlPanelLayout(
                nextLayout.workspace,
                nextLayout.controlPanelSide,
              ),
            );
            persistWorkspaceLayout(workspaceViewId, nextLayout);
          }
        }

        // A successful layout fetch proves the route that restart-required
        // errors report as broken is now functional. Clear the stale toast
        // only if the current requestError is the exact message we set.
        const staleRestartMessage =
          workspaceLayoutRestartErrorMessageRef.current;
        if (staleRestartMessage !== null) {
          workspaceLayoutRestartErrorMessageRef.current = null;
          setRequestError((current) =>
            resolveRecoveredWorkspaceLayoutRequestError(
              current,
              staleRestartMessage,
            ),
          );
        }
        workspaceLayoutLoadPendingRef.current = false;
        setIsWorkspaceLayoutReady(true);
      })
      .catch((error) => {
        console.warn(
          "workspace layout warning> failed to load server workspace layout:",
          error,
        );
        if (!cancelled) {
          // Restart-required errors indicate an incompatible backend; surface
          // the restart instruction to the user instead of silently degrading.
          if (isBackendUnavailableError(error) && error.restartRequired) {
            const message = getErrorMessage(error);
            workspaceLayoutRestartErrorMessageRef.current = message;
            reportRequestError(error);
          }
          workspaceLayoutLoadPendingRef.current = false;
          setIsWorkspaceLayoutReady(true);
        }
      });

    return () => {
      cancelled = true;
      workspaceLayoutLoadPendingRef.current = false;
    };
  }, [workspaceViewId]);

  useEffect(() => {
    if (!isWorkspaceLayoutReady) {
      return;
    }

    const persistedWorkspace =
      stripDiffPreviewDocumentContentFromWorkspaceState(
        stripLoadingGitDiffPreviewTabsFromWorkspaceState(
          applyControlPanelLayout(workspace, controlPanelSide),
        ),
      );
    const layout: WorkspaceLayoutPersistencePayload = {
      controlPanelSide,
      themeId,
      styleId,
      markdownThemeId,
      markdownStyleId,
      diagramThemeOverrideMode,
      diagramLook,
      diagramPalette,
      fontSizePx,
      editorFontSizePx,
      densityPercent,
      workspace: persistedWorkspace,
    };
    persistWorkspaceLayout(workspaceViewId, layout);
    pendingWorkspaceLayoutSaveRef.current = {
      workspaceId: workspaceViewId,
      layout,
    };

    clearPendingWorkspaceLayoutSaveTimeout();
    const persistTimeout = window.setTimeout(() => {
      flushPendingWorkspaceLayoutSave();
    }, WORKSPACE_LAYOUT_PERSIST_DELAY_MS);
    pendingWorkspaceLayoutSaveTimeoutRef.current = persistTimeout;

    return () => {
      if (pendingWorkspaceLayoutSaveTimeoutRef.current === persistTimeout) {
        clearPendingWorkspaceLayoutSaveTimeout();
      }
    };
  }, [
    controlPanelSide,
    densityPercent,
    diagramLook,
    diagramPalette,
    diagramThemeOverrideMode,
    editorFontSizePx,
    fontSizePx,
    isWorkspaceLayoutReady,
    markdownStyleId,
    markdownThemeId,
    styleId,
    themeId,
    workspace,
    workspaceViewId,
  ]);

  useEffect(() => {
    function handlePageHide() {
      flushWorkspaceLayoutSaveRef.current({ keepalive: true });
    }

    window.addEventListener("pagehide", handlePageHide);
    return () => {
      window.removeEventListener("pagehide", handlePageHide);
    };
  }, []);

  return {
    isWorkspaceLayoutReady,
    workspaceSummaries,
    workspaceSummariesRef,
    setWorkspaceSummaries,
    isWorkspaceSwitcherLoading,
    workspaceSwitcherError,
    deletingWorkspaceIds,
    ignoreFetchedWorkspaceLayoutRef,
    workspaceLayoutLoadPendingRef,
    pendingWorkspaceLayoutSaveRef,
    flushWorkspaceLayoutSaveRef,
    refreshWorkspaceSummaries,
    flushPendingWorkspaceLayoutSave,
    navigateToWorkspace,
    handleWorkspaceSwitcherToggle,
    handleOpenWorkspaceHere,
    handleOpenNewWorkspaceHere,
    handleOpenNewWorkspaceWindow,
    handleDeleteWorkspace,
  };
}
