// app-workspace-layout.ts
//
// Owns: the workspace-layout persistence lifecycle plus the
// workspaces-list refresh/delete control flow that used to
// live inline in App.tsx. That includes the workspace-summary
// request-token guards (`beginWorkspaceSummariesRequest`,
// `isLatestWorkspaceSummariesRequest`), in-flight GET reuse, the
// workspace-summaries state + ref, the list loading/error
// state, the deleting-workspace-ids state + ref, the
// pending-layout-save refs and their flush helpers
// (`clearPendingWorkspaceLayoutSaveTimeout`,
// `persistPendingWorkspaceLayoutSave`,
// `flushPendingWorkspaceLayoutSave`, `flushWorkspaceLayoutSave
// Ref`), the fetch-layout effect that flips
// `isWorkspaceLayoutReady` (including the workspace-restart
// recovery notice via `workspaceLayoutRestartErrorMessageRef`),
// the persist-layout effect, and the single `pagehide` listener
// that keeps the pending layout save alive across unloads.
// Storage pause/retry is independent of completed hydration: it
// gates saves, not read-only consumers of an already restored
// workspace. Server adoption stores before publishing either its
// tree or its appearance preferences.
//
// Refresh/delete invariants:
// - Loading is GET-in-flight only, tracked by a GET generation
//   separate from the apply token.
// - DELETE bumps the apply token so a stale GET cannot write the
//   list; that GET may still clear loading if no newer GET is in
//   flight. A superseded GET must not clear a newer GET.
// - In-flight GET reuse stays token-gated and only while the
//   captured list identity still matches.
// - Explicit Refresh/Reload clear the outward error immediately
//   and refuse a GET whose captured list identity is already
//   stale after rename/SSE. `forceFresh` always starts a new GET.
// - List and delete errors share the outward string | null, but
//   keep provenance internally. A successful current GET clears a
//   recovered list-GET error and does not clear a delete error. A
//   preserveError list-GET failure keeps only a current delete
//   error; otherwise it records the latest list error. A stale GET
//   cannot write the list or erase a newer error.
// - After the last pending DELETE settles, automatic resync
//   starts a fresh GET with `preserveError`, even if that delete
//   no longer owns the latest request token. Failed automatic
//   resync keeps a visible delete error; a successful automatic
//   GET also keeps that delete error.
// - Outstanding DELETEs are tracked separately so the header can
//   say Deleting versus Loading.
//
// Does not own: the Workspaces panel JSX, the restart-required
// notice, the generic backend-connection state, or the
// workspace/session/projects/orchestrator state the effects
// merely read.
//
// Split out of: ui/src/App.tsx (Slice 12 of the App-split plan,
// see docs/app-split-plan.md).

import {
  useCallback,
  useEffect,
  useMemo,
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
  renameWorkspaceLayout,
  saveWorkspaceLayout,
  type WorkspaceLayoutSummary,
} from "./api";
import { ApiRequestError, isBackendUnavailableError } from "./api-request";
import { hydrateControlPanelLayout } from "./control-panel-layout";
import { resolveRecoveredWorkspaceLayoutRequestError } from "./state-adoption";
import {
  collectWorkspaceSessionReferences,
  reconcileWorkspaceState,
  stripDiffPreviewDocumentContentFromWorkspaceState,
  stripLoadingGitDiffPreviewTabsFromWorkspaceState,
  workspaceHasDelegatedChildSessionReferences,
} from "./workspace";
import { type WorkspaceState } from "./workspace-types";
import type { RefreshWorkspaceSummaries } from "./workspace-summaries-refresh";
import { appTestHooks } from "./app-test-hooks";
import {
  createWorkspaceViewId,
  deleteStoredWorkspaceLayout,
  parseStoredWorkspaceLayout,
  persistWorkspaceLayout,
  acknowledgeWorkspaceLayoutSave,
  hasPendingWorkspaceLayout,
  WorkspaceLayoutStorageReadError,
  type StoredWorkspaceLayout,
  type ControlPanelSide,
  getWorkspaceViewHref,
} from "./workspace-storage";
import { getStoredThemePreferences } from "./themes";
import type {
  ThemePreferences,
  DiagramLook,
  DiagramPalette,
  DiagramThemeOverrideMode,
  MarkdownStyleId,
  MarkdownThemeId,
  StyleId,
  ThemeId,
  ThemeMode,
} from "./themes";
import { getErrorMessage } from "./app-utils";
import {
  WORKSPACE_LAYOUT_PERSIST_DELAY_MS,
  type PendingWorkspaceLayoutSave,
  type WorkspaceLayoutPersistencePayload,
} from "./app-shell-internals";
import type { Session } from "./types";
import type { WorkspaceDeleteRequest } from "./workspace-delete-request";

export type UseAppWorkspaceLayoutParams = {
  workspaceViewId: string;
  workspace: WorkspaceState;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  sessions: Session[];
  sessionsRef: MutableRefObject<Session[]>;
  isSessionStateReady: boolean;
  controlPanelSide: ControlPanelSide;
  setControlPanelSide: Dispatch<SetStateAction<ControlPanelSide>>;
  preferences: {
    lightThemeId: ThemeId;
    darkThemeId: ThemeId;
    themeMode: ThemeMode;
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
    setLightThemeId: Dispatch<SetStateAction<ThemeId>>;
    setDarkThemeId: Dispatch<SetStateAction<ThemeId>>;
    setThemeMode: Dispatch<SetStateAction<ThemeMode>>;
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
  setRequestError: Dispatch<SetStateAction<string | null>>;
  isMountedRef: MutableRefObject<boolean>;
  reportRequestError: (error: unknown, options?: { message?: string }) => void;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: ControlPanelSide,
  ) => WorkspaceState;
};

type FetchedWorkspaceThemeFields = Pick<
  StoredWorkspaceLayout,
  "lightThemeId" | "darkThemeId" | "themeMode"
>;

const WORKSPACE_STORAGE_RETRY_TIMEOUT_MS = 15_000;

// Only failures at the browser write boundary enter storage recovery; parser
// or reconciliation exceptions must not be disguised as storage failures.
class WorkspaceLayoutStorageWriteError extends Error {}

function storeWorkspaceLayout(
  workspaceId: string,
  layout: StoredWorkspaceLayout,
  pendingSaveId?: string,
) {
  try {
    persistWorkspaceLayout(workspaceId, layout, pendingSaveId);
  } catch (error) {
    throw new WorkspaceLayoutStorageWriteError(
      `Workspace save is paused. Could not preserve the workspace layout in this browser: ${getErrorMessage(error)} Keep this tab open and retry after restoring storage access.`,
    );
  }
}

export function resolveFetchedWorkspaceThemePreferences(
  layout: FetchedWorkspaceThemeFields,
): ThemePreferences | null {
  if (
    layout.lightThemeId === undefined &&
    layout.darkThemeId === undefined &&
    layout.themeMode === undefined
  ) {
    return null;
  }

  // Live hydration uses the same current preference slots as cold start.
  return getStoredThemePreferences(layout);
}

export type UseAppWorkspaceLayoutReturn = {
  isWorkspaceLayoutReady: boolean;
  workspaceLayoutStorageError: string | null;
  isWorkspaceLayoutRetrying: boolean;
  retryWorkspaceLayoutStorage: () => void;
  workspaceSummaries: WorkspaceLayoutSummary[];
  workspaceSummariesRef: MutableRefObject<WorkspaceLayoutSummary[]>;
  setWorkspaceSummaries: Dispatch<SetStateAction<WorkspaceLayoutSummary[]>>;
  isWorkspacesListLoading: boolean;
  workspacesListError: string | null;
  deletingWorkspaceIds: string[];
  ignoreFetchedWorkspaceLayoutRef: MutableRefObject<boolean>;
  workspaceLayoutLoadPendingRef: MutableRefObject<boolean>;
  pendingWorkspaceLayoutSaveRef: MutableRefObject<PendingWorkspaceLayoutSave | null>;
  flushWorkspaceLayoutSaveRef: MutableRefObject<
    (options?: { keepalive?: boolean }) => void
  >;
  refreshWorkspaceSummaries: RefreshWorkspaceSummaries;
  flushPendingWorkspaceLayoutSave: (options?: { keepalive?: boolean }) => void;
  navigateToWorkspace: (nextWorkspaceViewId: string) => void;
  handleOpenWorkspaceHere: (nextWorkspaceViewId: string) => void;
  handleOpenNewWorkspaceHere: () => void;
  handleOpenNewWorkspaceWindow: () => void;
  handleDeleteWorkspace: (workspaceId: string) => WorkspaceDeleteRequest;
  handleRenameWorkspace: (workspaceId: string, label: string) => Promise<void>;
};

function workspaceHasSessionReferences(workspace: WorkspaceState) {
  return workspace.panes.some((pane) => {
    if (pane.activeSessionId) {
      return true;
    }

    return pane.tabs.some((tab) => {
      if (tab.kind === "session") {
        return true;
      }
      if (tab.kind === "canvas" && tab.cards.length > 0) {
        return true;
      }
      return "originSessionId" in tab && !!tab.originSessionId;
    });
  });
}

async function waitForWorkspaceLayoutLoadCommitTestBarrier() {
  // Production never installs this hook. Tests use it as a data-free,
  // cancellation-safe boundary immediately before either fetch outcome
  // commits state, so renderApp can deterministically own that commit.
  const beforeCommit = appTestHooks?.beforeWorkspaceLayoutLoadCommit;
  if (beforeCommit) {
    await beforeCommit();
  }
}

export function useAppWorkspaceLayout(
  params: UseAppWorkspaceLayoutParams,
): UseAppWorkspaceLayoutReturn {
  const {
    workspaceViewId,
    workspace,
    setWorkspace,
    sessions,
    sessionsRef,
    isSessionStateReady,
    controlPanelSide,
    setControlPanelSide,
    preferences,
    setPreferences,
    setRequestError,
    isMountedRef,
    reportRequestError,
    applyControlPanelLayout,
  } = params;
  const {
    lightThemeId,
    darkThemeId,
    themeMode,
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
    setLightThemeId,
    setDarkThemeId,
    setThemeMode,
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
  const [workspaceLayoutStorageError, setWorkspaceLayoutStorageError] = useState<string | null>(null);
  const [isWorkspaceLayoutRetrying, setIsWorkspaceLayoutRetrying] = useState(false);
  const [workspaceLayoutRetryEpoch, setWorkspaceLayoutRetryEpoch] = useState(0);
  const storagePauseRef = useRef<{ workspaceId: string; serialized: string; preserveLocal: boolean } | null>(null);
  const currentLayoutRef = useRef<{ serialized: string; layout: WorkspaceLayoutPersistencePayload } | null>(null);
  const storageRetryInFlightRef = useRef(false);
  const cancelStorageRetryRef = useRef<(() => void) | null>(null);
  const initialPruneWorkspaceIdRef = useRef<string | null>(null);
  const [workspaceSummaries, setWorkspaceSummaries] = useState<
    WorkspaceLayoutSummary[]
  >([]);
  const [isWorkspacesListLoading, setIsWorkspacesListLoading] =
    useState(false);
  const [workspacesListErrorRecord, setWorkspacesListErrorRecord] = useState<{
    source: "list" | "delete";
    message: string;
  } | null>(null);
  const [deletingWorkspaceIds, setDeletingWorkspaceIds] = useState<string[]>(
    [],
  );

  const ignoreFetchedWorkspaceLayoutRef = useRef(false);
  const workspaceLayoutRestartErrorMessageRef = useRef<string | null>(null);
  const workspaceLayoutLoadPendingRef = useRef(false);
  const workspaceSummariesRequestTokenRef = useRef(0);
  const workspaceSummariesGetIdRef = useRef(0);
  const workspaceSummariesInFlightRef = useRef<{
    token: number;
    promise: Promise<void>;
    snapshot: WorkspaceLayoutSummary[];
  } | null>(null);
  const deletingWorkspaceIdsRef = useRef<Set<string>>(new Set());
  const pendingWorkspaceLayoutSaveRef =
    useRef<PendingWorkspaceLayoutSave | null>(null);
  const pendingWorkspaceLayoutSaveTimeoutRef = useRef<number | null>(null);
  const latestWorkspaceLayoutSaveRef =
    useRef<PendingWorkspaceLayoutSave | null>(null);
  const workspaceLayoutSaveRetryDelayRef = useRef(1000);
  const workspaceLayoutSaveErrorRef = useRef<string | null>(null);
  const workspaceLayoutPersistenceMountedRef = useRef(true);
  const flushWorkspaceLayoutSaveRef = useRef<
    (options?: { keepalive?: boolean }) => void
  >(() => {});
  const workspaceSummariesRef = useRef(workspaceSummaries);
  const pendingFetchedWorkspaceLayoutRef = useRef<StoredWorkspaceLayout | null>(
    null,
  );
  const hasPrunedInitialDelegatedChildWorkspaceTabsRef = useRef(false);
  const initialDelegatedChildPruneSessionIdsRef = useRef<Set<string>>(
    new Set(),
  );
  const initialDelegatedChildPruneMetadataSignatureRef = useRef("");
  const workspaceRef = useRef(workspace);
  const isSessionStateReadyRef = useRef(isSessionStateReady);
  const hasSessions = sessions.length > 0;
  workspaceRef.current = workspace;
  isSessionStateReadyRef.current = isSessionStateReady;

  useEffect(() => {
    workspaceSummariesRef.current = workspaceSummaries;
  }, [workspaceSummaries]);

  // Preserve the first paused content baseline across failed retry attempts.
  // Unlike requestError, this notice cannot be cleared by unrelated actions.
  function pauseWorkspaceLayoutStorage(message: string, preserveLocal = false) {
    cancelStorageRetryRef.current?.();
    const current = currentLayoutRef.current!;
    storagePauseRef.current ??= {
      workspaceId: workspaceViewId,
      serialized: current.serialized,
      preserveLocal: preserveLocal || ignoreFetchedWorkspaceLayoutRef.current,
    };
    if (preserveLocal) storagePauseRef.current.preserveLocal = true;
    clearPendingWorkspaceLayoutSaveTimeout();
    pendingWorkspaceLayoutSaveRef.current = null;
    latestWorkspaceLayoutSaveRef.current = null;
    storageRetryInFlightRef.current = false;
    setIsWorkspaceLayoutRetrying(false);
    setWorkspaceLayoutStorageError(message);
    workspaceLayoutLoadPendingRef.current = false;
  }

  function finishWorkspaceLayoutStorageRecovery() {
    cancelStorageRetryRef.current?.();
    storagePauseRef.current = null;
    storageRetryInFlightRef.current = false;
    setIsWorkspaceLayoutRetrying(false);
    setWorkspaceLayoutStorageError(null);
  }

  // One authority check spans the whole pause/retry lifetime, not just the
  // click. Both immediate and deferred adoption must check the latest content.
  const preservePausedWorkspaceLayout = useCallback(() => {
    const pending = hasPendingWorkspaceLayout(workspaceViewId);
    const paused = storagePauseRef.current;
    const current = currentLayoutRef.current!;
    if (paused?.workspaceId === workspaceViewId &&
        (paused.preserveLocal || paused.serialized !== current.serialized)) {
      paused.preserveLocal = true;
      storeWorkspaceLayout(workspaceViewId, current.layout, crypto.randomUUID());
      return true;
    }
    return pending;
  }, [workspaceViewId]);

  function retryWorkspaceLayoutStorage() {
    const paused = storagePauseRef.current;
    if (!paused || paused.workspaceId !== workspaceViewId || storageRetryInFlightRef.current) return;
    try {
      // Unknown is not absent. Probe before writing or allowing server adoption.
      preservePausedWorkspaceLayout();
      storageRetryInFlightRef.current = true;
      setIsWorkspaceLayoutRetrying(true);
      setWorkspaceLayoutRetryEpoch((value) => value + 1);
    } catch (error) {
      pauseWorkspaceLayoutStorage(error instanceof WorkspaceLayoutStorageWriteError
        ? error.message
        : `Workspace save is paused. ${getErrorMessage(error)} Keep this tab open and retry after restoring storage access.`);
    }
  }

  const resetInitialDelegatedChildWorkspacePruneScope = useCallback(
    (nextWorkspace: WorkspaceState) => {
      initialDelegatedChildPruneSessionIdsRef.current =
        collectWorkspaceSessionReferences(nextWorkspace);
      initialDelegatedChildPruneMetadataSignatureRef.current = "";
      hasPrunedInitialDelegatedChildWorkspaceTabsRef.current = false;
    },
    [],
  );

  const applyFetchedPreferences = useCallback((nextLayout: StoredWorkspaceLayout, applySide = true) => {
    if (applySide) setControlPanelSide(nextLayout.controlPanelSide);
    const theme = resolveFetchedWorkspaceThemePreferences(nextLayout);
    if (theme) {
      setLightThemeId(theme.lightThemeId);
      setDarkThemeId(theme.darkThemeId);
      setThemeMode(theme.themeMode);
    }
    if (nextLayout.styleId) setStyleId(nextLayout.styleId);
    if (nextLayout.markdownThemeId) setMarkdownThemeId(nextLayout.markdownThemeId);
    if (nextLayout.markdownStyleId) setMarkdownStyleId(nextLayout.markdownStyleId);
    if (nextLayout.diagramThemeOverrideMode) setDiagramThemeOverrideMode(nextLayout.diagramThemeOverrideMode);
    if (nextLayout.diagramLook) setDiagramLook(nextLayout.diagramLook);
    if (nextLayout.diagramPalette) setDiagramPalette(nextLayout.diagramPalette);
    if (nextLayout.fontSizePx !== undefined) setFontSizePx(nextLayout.fontSizePx);
    if (nextLayout.editorFontSizePx !== undefined) setEditorFontSizePx(nextLayout.editorFontSizePx);
    if (nextLayout.densityPercent !== undefined) setDensityPercent(nextLayout.densityPercent);
  }, [setControlPanelSide, setLightThemeId, setDarkThemeId, setThemeMode, setStyleId,
    setMarkdownThemeId, setMarkdownStyleId, setDiagramThemeOverrideMode, setDiagramLook,
    setDiagramPalette, setFontSizePx, setEditorFontSizePx, setDensityPercent]);

  const applyFetchedWorkspaceLayout = useCallback(
    (nextLayout: StoredWorkspaceLayout) => {
      // Unsaved local work must not silently lose to a server copy, including
      // a response deferred until sessions load. The local pending layout wins.
      if (preservePausedWorkspaceLayout()) return false;
      const restoredWorkspace = reconcileWorkspaceState(
        nextLayout.workspace,
        sessionsRef.current,
        { pruneDelegatedChildSessionTabs: true },
      );
      // Store before publishing ANY server state. Publishing preferences first
      // can mix server appearance with a local tree after a failed write, and
      // makes content comparisons unable to distinguish server/user changes.
      storeWorkspaceLayout(workspaceViewId, {
        ...nextLayout,
        workspace: restoredWorkspace,
      });
      // Only a successful adoption establishes restored provenance. A retry
      // that keeps pending local work never reaches this reset, so live child
      // tabs remain protected; server-only refs still catch late parent metadata.
      resetInitialDelegatedChildWorkspacePruneScope(nextLayout.workspace);
      setWorkspace(
        hydrateControlPanelLayout(
          restoredWorkspace,
          nextLayout.controlPanelSide,
        ),
      );
      applyFetchedPreferences(nextLayout);
      return true;
    },
    [applyFetchedPreferences, preservePausedWorkspaceLayout,
      resetInitialDelegatedChildWorkspacePruneScope, sessionsRef, setWorkspace, workspaceViewId],
  );

  const initialDelegatedChildPruneMetadataSignature = useCallback(
    (currentSessions: readonly Session[]) => {
      const initialSessionIds = initialDelegatedChildPruneSessionIdsRef.current;
      if (initialSessionIds.size === 0) {
        return "";
      }
      return currentSessions
        .flatMap((session) =>
          initialSessionIds.has(session.id)
            ? [`${session.id}:${session.parentDelegationId ?? ""}`]
            : [],
        )
        .sort()
        .join("|");
    },
    [],
  );

  const delegatedChildSessionIdsOutsideInitialPruneScope = useCallback(
    (currentSessions: readonly Session[]) => {
      const initialSessionIds = initialDelegatedChildPruneSessionIdsRef.current;
      return currentSessions.flatMap((session) =>
        session.parentDelegationId && !initialSessionIds.has(session.id)
          ? [session.id]
          : [],
      );
    },
    [],
  );

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

  const refreshWorkspaceSummaries = useCallback<RefreshWorkspaceSummaries>((options) => {
    if (!options?.preserveError) {
      setWorkspacesListErrorRecord(null);
    }
    const inflight = workspaceSummariesInFlightRef.current;
    if (
      inflight
      && isLatestWorkspaceSummariesRequest(inflight.token)
      && !options?.forceFresh
      && workspaceSummariesRef.current === inflight.snapshot
    ) {
      return inflight.promise;
    }
    const requestToken = beginWorkspaceSummariesRequest();
    const getId = ++workspaceSummariesGetIdRef.current;
    const workspacesAtRequest = workspaceSummariesRef.current;
    setIsWorkspacesListLoading(true);
    const request = (async () => {
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
        setWorkspacesListErrorRecord((current) =>
          current?.source === "list" ? null : current,
        );
      } catch (error) {
        if (
          !isMountedRef.current ||
          !isLatestWorkspaceSummariesRequest(requestToken)
        ) {
          return;
        }
        const nextError = getErrorMessage(error);
        if (options?.preserveError) {
          setWorkspacesListErrorRecord((current) =>
            current?.source === "delete"
              ? current
              : { source: "list", message: nextError },
          );
        } else {
          setWorkspacesListErrorRecord({ source: "list", message: nextError });
        }
      } finally {
        if (
          isMountedRef.current &&
          workspaceSummariesGetIdRef.current === getId
        ) {
          setIsWorkspacesListLoading(false);
        }
      }
    })();
    workspaceSummariesInFlightRef.current = {
      token: requestToken,
      promise: request,
      snapshot: workspacesAtRequest,
    };
    void request.finally(() => {
      if (workspaceSummariesInFlightRef.current?.promise === request) {
        workspaceSummariesInFlightRef.current = null;
      }
    });
    return request;
    // Actual dependencies: stable setState identities plus the mount
    // ref. Request tokens, GET generation, and the list snapshot are
    // read through refs, so the callback stays current without listing
    // unused backend-connection handlers.
  }, [
    isMountedRef,
    setIsWorkspacesListLoading,
    setWorkspaceSummaries,
    setWorkspacesListErrorRecord,
  ]);

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
    ).then(() => {
      try {
        acknowledgeWorkspaceLayoutSave(pendingSave.workspaceId, pendingSave.localSaveId);
      } catch (error) {
        // The server accepted the save. A local acknowledgement failure must
        // not be reported as a failed PUT; retain the conservative pending mark.
        console.warn("workspace layout warning> failed to acknowledge local save:", error);
      }
      if (latestWorkspaceLayoutSaveRef.current === pendingSave) {
        workspaceLayoutSaveRetryDelayRef.current = 1000;
        const previousError = workspaceLayoutSaveErrorRef.current;
        if (workspaceLayoutPersistenceMountedRef.current && isMountedRef.current && previousError) {
          setRequestError((current) => current === previousError ? null : current);
          workspaceLayoutSaveErrorRef.current = null;
        }
      }
    }).catch((error) => {
      console.warn(
        "workspace layout warning> failed to save server workspace layout:",
        error,
      );
      // Retry only the latest edit; an older failure must not replace its
      // content or report an error for a save that has already been superseded.
      if (
        !workspaceLayoutPersistenceMountedRef.current ||
        !isMountedRef.current ||
        latestWorkspaceLayoutSaveRef.current !== pendingSave
      ) {
        return;
      }
      clearPendingWorkspaceLayoutSaveTimeout();
      const retryable = error instanceof ApiRequestError && (
        error.status === 408 || error.status === 429 ||
        (error.status !== null && error.status >= 500 && error.status <= 599) ||
        (isBackendUnavailableError(error) && !error.restartRequired &&
          (error.status === null || (error.status >= 200 && error.status < 300)))
      );
      if (!retryable) {
        // Local storage already holds this layout. Do not resend a rejected
        // payload on a timer or pagehide; a subsequent edit can save normally.
        pendingWorkspaceLayoutSaveRef.current = null;
        const message = `Workspace layout was not saved to the server: ${getErrorMessage(error)} Your unsaved layout is kept in this browser. A subsequent layout edit will try again.`;
        workspaceLayoutSaveErrorRef.current = message;
        setRequestError(message);
        return;
      }
      pendingWorkspaceLayoutSaveRef.current = pendingSave;
      const delay = workspaceLayoutSaveRetryDelayRef.current;
      workspaceLayoutSaveRetryDelayRef.current = Math.min(delay * 2, 30_000);
      pendingWorkspaceLayoutSaveTimeoutRef.current = window.setTimeout(() => {
        flushWorkspaceLayoutSaveRef.current();
      }, delay);
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
    const href = getWorkspaceViewHref(nextWorkspaceViewId);
    if (href !== undefined) {
      window.location.assign(href);
    }
  }

  function handleOpenWorkspaceHere(nextWorkspaceViewId: string) {
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
    const href = getWorkspaceViewHref(nextWorkspaceViewId);
    if (href !== undefined) {
      window.open(href, "_blank", "noopener");
    }
  }

  async function handleRenameWorkspace(workspaceId: string, label: string) {
    const { layout } = await renameWorkspaceLayout(workspaceId, label);
    if (!isMountedRef.current) {
      return;
    }
    // Merge only this label response, preserving a newer SSE revision and
    // never restoring a workspace concurrently removed from the list.
    const next = workspaceSummariesRef.current.map((summary) =>
      summary.id === workspaceId && summary.revision <= layout.revision
        ? { ...summary, label: layout.label, revision: layout.revision, updatedAt: layout.updatedAt }
        : summary,
    );
    workspaceSummariesRef.current = next;
    setWorkspaceSummaries(next);
  }

  function handleDeleteWorkspace(workspaceId: string): WorkspaceDeleteRequest {
    if (
      workspaceId === workspaceViewId ||
      deletingWorkspaceIdsRef.current.has(workspaceId)
    ) {
      return { started: false, completed: Promise.resolve() };
    }

    const nextDeletingWorkspaceIds = new Set(deletingWorkspaceIdsRef.current);
    nextDeletingWorkspaceIds.add(workspaceId);
    deletingWorkspaceIdsRef.current = nextDeletingWorkspaceIds;
    setDeletingWorkspaceIds([...nextDeletingWorkspaceIds]);
    setWorkspacesListErrorRecord(null);

    const requestToken = beginWorkspaceSummariesRequest();
    const workspacesAtRequest = workspaceSummariesRef.current;
    const completed = (async () => {
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
            const next = workspaceSummariesRef.current.filter((w) => w.id !== workspaceId);
            workspaceSummariesRef.current = next;
            setWorkspaceSummaries(next);
          }
        }
      } catch (error) {
        if (isMountedRef.current) {
          setWorkspacesListErrorRecord({
            source: "delete",
            message: getErrorMessage(error),
          });
        }
      } finally {
        finishDeletingWorkspace(workspaceId);
        if (isMountedRef.current && deletingWorkspaceIdsRef.current.size === 0) {
          void refreshWorkspaceSummaries({ preserveError: true, forceFresh: true });
        }
      }
    })();

    return { started: true, completed };
  }

  useEffect(() => {
    let cancelled = false;
    const isStorageRetry = storagePauseRef.current?.workspaceId === workspaceViewId;
    const controller = new AbortController();
    let retryDeadline: number | undefined;
    const cancelRetry = () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(retryDeadline);
      pendingFetchedWorkspaceLayoutRef.current = null;
      if (cancelStorageRetryRef.current === cancelRetry) cancelStorageRetryRef.current = null;
    };
    if (storagePauseRef.current?.workspaceId !== workspaceViewId) {
      finishWorkspaceLayoutStorageRecovery();
    }
    workspaceLayoutLoadPendingRef.current = true;
    ignoreFetchedWorkspaceLayoutRef.current = false;
    pendingFetchedWorkspaceLayoutRef.current = null;
    // Retry is not a new restore scope: tabs opened in this UI session must
    // remain outside the restore-only delegated-child pruning set.
    if (initialPruneWorkspaceIdRef.current !== workspaceViewId) {
      initialPruneWorkspaceIdRef.current = workspaceViewId;
      resetInitialDelegatedChildWorkspacePruneScope(workspaceRef.current);
    }
    // A storage retry does not un-hydrate the current workspace. Independent
    // consumers (Git diff content, restored-child pruning) must keep running.
    if (!isStorageRetry) setIsWorkspaceLayoutReady(false);

    if (isStorageRetry) {
      cancelStorageRetryRef.current = cancelRetry;
      // The deadline covers the GET and any wait for session metadata. On
      // expiry abandon this attempt, fence late results and allow another try.
      retryDeadline = window.setTimeout(() => {
        pauseWorkspaceLayoutStorage("Workspace recovery timed out. Keep this tab open and choose Retry workspace save to try again.");
      }, WORKSPACE_STORAGE_RETRY_TIMEOUT_MS);
    }

    const layoutRequest = isStorageRetry
      ? fetchWorkspaceLayout(workspaceViewId, { signal: controller.signal })
      : fetchWorkspaceLayout(workspaceViewId);
    void layoutRequest
      .then(async (response) => {
        if (import.meta.env.MODE === "test") {
          await waitForWorkspaceLayoutLoadCommitTestBarrier();
        }
        if (cancelled) {
          return;
        }

        // Backend response types are compile-time only. Reuse the persisted
        // layout parser here as the runtime boundary so unknown theme ids,
        // modes, and other hand-edited/stale values never reach DOM setters.
        // A pending local layout survives both failed and in-flight saves
        // across reload. Do not merge older server preferences into it either.
        const pendingLocalLayout = preservePausedWorkspaceLayout();
        const nextLayout = response && !pendingLocalLayout
          ? parseStoredWorkspaceLayout(
              JSON.stringify({
                controlPanelSide: response.layout.controlPanelSide,
                lightThemeId: response.layout.lightThemeId,
                darkThemeId: response.layout.darkThemeId,
                themeMode: response.layout.themeMode,
                styleId: response.layout.styleId,
                markdownThemeId: response.layout.markdownThemeId,
                markdownStyleId: response.layout.markdownStyleId,
                diagramThemeOverrideMode:
                  response.layout.diagramThemeOverrideMode,
                diagramLook: response.layout.diagramLook,
                diagramPalette: response.layout.diagramPalette,
                fontSizePx: response.layout.fontSizePx,
                editorFontSizePx: response.layout.editorFontSizePx,
                densityPercent: response.layout.densityPercent,
                workspace: response.layout.workspace,
              }),
            )
          : null;

        let isFetchedWorkspaceLayoutWaitingForSessions = false;
        if (nextLayout) {
          const shouldApplyFetchedWorkspaceLayout =
            !ignoreFetchedWorkspaceLayoutRef.current;
          // A manual layout change during hydration claims the workspace tree
          // and dock side locally, but still allows the server-stored visual
          // preferences to merge in once the fetch resolves.
          // This initial manual-claim policy is distinct from pending local
          // recovery, which rejects the whole server payload above.
          if (!shouldApplyFetchedWorkspaceLayout && !isStorageRetry) {
            applyFetchedPreferences(nextLayout, false);
          }
          if (shouldApplyFetchedWorkspaceLayout) {
            if (
              !isSessionStateReadyRef.current &&
              sessionsRef.current.length === 0 &&
              workspaceHasSessionReferences(nextLayout.workspace)
            ) {
              pendingFetchedWorkspaceLayoutRef.current = nextLayout;
              isFetchedWorkspaceLayoutWaitingForSessions = true;
            } else {
              pendingFetchedWorkspaceLayoutRef.current = null;
              applyFetchedWorkspaceLayout(nextLayout);
            }
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
        workspaceLayoutLoadPendingRef.current =
          isFetchedWorkspaceLayoutWaitingForSessions;
        setIsWorkspaceLayoutReady((ready) => ready || !isFetchedWorkspaceLayoutWaitingForSessions);
        if (!isFetchedWorkspaceLayoutWaitingForSessions) finishWorkspaceLayoutStorageRecovery();
      })
      .catch(async (error) => {
        if (import.meta.env.MODE === "test") {
          await waitForWorkspaceLayoutLoadCommitTestBarrier();
        }
        console.warn(
          "workspace layout warning> failed to load server workspace layout:",
          error,
        );
        if (!cancelled) {
          if (error instanceof WorkspaceLayoutStorageReadError || error instanceof WorkspaceLayoutStorageWriteError) {
            pauseWorkspaceLayoutStorage(error.message);
            return;
          }
          // Restart-required errors indicate an incompatible backend; surface
          // the restart instruction to the user instead of silently degrading.
          if (isBackendUnavailableError(error) && error.restartRequired) {
            const message = getErrorMessage(error);
            workspaceLayoutRestartErrorMessageRef.current = message;
            reportRequestError(error);
          }
          if (isStorageRetry) {
            // Working browser storage does not prove server recovery succeeded.
            // Keep both the save pause and the previous hydration readiness:
            // otherwise autosave can publish the older bootstrap layout.
            pauseWorkspaceLayoutStorage(
              `Workspace recovery failed: ${getErrorMessage(error)} Workspace save remains paused. Keep this tab open and choose Retry workspace save to try again.`,
            );
            return;
          }
          workspaceLayoutLoadPendingRef.current = false;
          setIsWorkspaceLayoutReady(true);
          finishWorkspaceLayoutStorageRecovery();
        }
      });

    return () => {
      cancelRetry();
      workspaceLayoutLoadPendingRef.current = false;
      pendingFetchedWorkspaceLayoutRef.current = null;
    };
  }, [
    applyFetchedPreferences,
    applyFetchedWorkspaceLayout,
    preservePausedWorkspaceLayout,
    resetInitialDelegatedChildWorkspacePruneScope,
    sessionsRef,
    workspaceViewId,
    workspaceLayoutRetryEpoch,
  ]);

  useEffect(() => {
    const pendingFetchedWorkspaceLayout =
      pendingFetchedWorkspaceLayoutRef.current;
    if (
      !pendingFetchedWorkspaceLayout ||
      (!isSessionStateReady && !hasSessions)
    ) {
      return;
    }

    pendingFetchedWorkspaceLayoutRef.current = null;
    if (ignoreFetchedWorkspaceLayoutRef.current) {
      if (!storagePauseRef.current) applyFetchedPreferences(pendingFetchedWorkspaceLayout, false);
      workspaceLayoutLoadPendingRef.current = false;
      setIsWorkspaceLayoutReady(true);
      finishWorkspaceLayoutStorageRecovery();
      return;
    }

    try {
      applyFetchedWorkspaceLayout(pendingFetchedWorkspaceLayout);
    } catch (error) {
      if (!(error instanceof WorkspaceLayoutStorageReadError) && !(error instanceof WorkspaceLayoutStorageWriteError)) throw error;
      pauseWorkspaceLayoutStorage(error.message);
      return;
    }
    workspaceLayoutLoadPendingRef.current = false;
    setIsWorkspaceLayoutReady(true);
    finishWorkspaceLayoutStorageRecovery();
  }, [applyFetchedPreferences, applyFetchedWorkspaceLayout, hasSessions, isSessionStateReady]);

  useEffect(() => {
    if (!isWorkspaceLayoutReady || !isSessionStateReady) {
      return;
    }

    const metadataSignature =
      initialDelegatedChildPruneMetadataSignature(sessions);
    if (
      metadataSignature !==
      initialDelegatedChildPruneMetadataSignatureRef.current
    ) {
      initialDelegatedChildPruneMetadataSignatureRef.current =
        metadataSignature;
      hasPrunedInitialDelegatedChildWorkspaceTabsRef.current = false;
    }

    if (hasPrunedInitialDelegatedChildWorkspaceTabsRef.current) {
      return;
    }

    const initialPruneSessionIds =
      initialDelegatedChildPruneSessionIdsRef.current;
    if (initialPruneSessionIds.size === 0) {
      hasPrunedInitialDelegatedChildWorkspaceTabsRef.current = true;
      return;
    }

    if (sessions.length === 0 && workspaceHasSessionReferences(workspace)) {
      return;
    }

    const preserveSessionIds =
      delegatedChildSessionIdsOutsideInitialPruneScope(sessions);
    if (
      !workspaceHasDelegatedChildSessionReferences(
        workspace,
        sessions,
        preserveSessionIds,
      )
    ) {
      // Session metadata is ready and none of the restored references are
      // delegated children, so the restore-only prune has no remaining work.
      hasPrunedInitialDelegatedChildWorkspaceTabsRef.current = true;
      return;
    }

    // The updater below may be invoked more than once by React. Mark the
    // restore-only prune as consumed here so the updater remains pure.
    hasPrunedInitialDelegatedChildWorkspaceTabsRef.current = true;
    setWorkspace((current) => {
      const currentSessions = sessionsRef.current;
      const currentPreserveSessionIds =
        delegatedChildSessionIdsOutsideInitialPruneScope(currentSessions);
      if (
        !workspaceHasDelegatedChildSessionReferences(
          current,
          currentSessions,
          currentPreserveSessionIds,
        )
      ) {
        return current;
      }

      // This one-shot prune is only for session references restored from local
      // or server workspace layouts; current-session child tabs are preserved.
      return applyControlPanelLayout(
        reconcileWorkspaceState(current, currentSessions, {
          pruneDelegatedChildSessionTabs: true,
          preserveSessionIds: currentPreserveSessionIds,
        }),
        controlPanelSide,
      );
    });
  }, [
    applyControlPanelLayout,
    controlPanelSide,
    delegatedChildSessionIdsOutsideInitialPruneScope,
    initialDelegatedChildPruneMetadataSignature,
    isSessionStateReady,
    isWorkspaceLayoutReady,
    sessions,
    sessionsRef,
    setWorkspace,
    workspace,
  ]);

  const currentWorkspaceLayout = useMemo(() => {
    const persistedWorkspace =
      stripDiffPreviewDocumentContentFromWorkspaceState(
        stripLoadingGitDiffPreviewTabsFromWorkspaceState(
          applyControlPanelLayout(workspace, controlPanelSide),
        ),
      );
    const layout: WorkspaceLayoutPersistencePayload = {
      controlPanelSide,
      lightThemeId,
      darkThemeId,
      themeMode,
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
    return { serialized: JSON.stringify(layout), layout };
  }, [
    applyControlPanelLayout,
    controlPanelSide,
    densityPercent,
    diagramLook,
    diagramPalette,
    diagramThemeOverrideMode,
    editorFontSizePx,
    fontSizePx,
    markdownStyleId,
    markdownThemeId,
    styleId,
    lightThemeId,
    darkThemeId,
    themeMode,
    workspace,
  ]);
  currentLayoutRef.current = currentWorkspaceLayout;
  const serializedWorkspaceLayout = currentWorkspaceLayout.serialized;

  useEffect(() => {
    if (!isWorkspaceLayoutReady || workspaceLayoutStorageError !== null || storagePauseRef.current) {
      return;
    }

    // Session reconciliation can produce a new workspace object without a
    // layout edit. Debounce changes to the persisted content so those renders
    // neither postpone an initial save forever nor trigger a save/SSE loop.
    const layout = currentLayoutRef.current!.layout;
    const localSaveId = crypto.randomUUID();
    try {
      persistWorkspaceLayout(workspaceViewId, layout, localSaveId);
    } catch (error) {
      pauseWorkspaceLayoutStorage(`Workspace save is paused. Could not preserve the workspace layout in this browser: ${getErrorMessage(error)} Keep this tab open and retry after restoring storage access.`, true);
      return;
    }
    pendingWorkspaceLayoutSaveRef.current = {
      workspaceId: workspaceViewId,
      layout,
      localSaveId,
    };
    latestWorkspaceLayoutSaveRef.current = pendingWorkspaceLayoutSaveRef.current;
    workspaceLayoutSaveRetryDelayRef.current = 1000;

    clearPendingWorkspaceLayoutSaveTimeout();
    const persistTimeout = window.setTimeout(() => {
      flushWorkspaceLayoutSaveRef.current();
    }, WORKSPACE_LAYOUT_PERSIST_DELAY_MS);
    pendingWorkspaceLayoutSaveTimeoutRef.current = persistTimeout;

    return () => {
      if (pendingWorkspaceLayoutSaveTimeoutRef.current === persistTimeout) {
        clearPendingWorkspaceLayoutSaveTimeout();
      }
    };
    // clearPendingWorkspaceLayoutSaveTimeout only accesses refs; the flush ref
    // supplies the current save/error callbacks without restarting the debounce.
  }, [isWorkspaceLayoutReady, workspaceLayoutStorageError, serializedWorkspaceLayout, workspaceViewId]);

  useEffect(() => {
    workspaceLayoutPersistenceMountedRef.current = true;
    function handlePageHide() {
      flushWorkspaceLayoutSaveRef.current({ keepalive: true });
    }

    window.addEventListener("pagehide", handlePageHide);
    return () => {
      workspaceLayoutPersistenceMountedRef.current = false;
      clearPendingWorkspaceLayoutSaveTimeout();
      window.removeEventListener("pagehide", handlePageHide);
    };
  }, []);

  return {
    isWorkspaceLayoutReady,
    workspaceLayoutStorageError,
    isWorkspaceLayoutRetrying,
    retryWorkspaceLayoutStorage,
    workspaceSummaries,
    workspaceSummariesRef,
    setWorkspaceSummaries,
    isWorkspacesListLoading,
    workspacesListError: workspacesListErrorRecord?.message ?? null,
    deletingWorkspaceIds,
    ignoreFetchedWorkspaceLayoutRef,
    workspaceLayoutLoadPendingRef,
    pendingWorkspaceLayoutSaveRef,
    flushWorkspaceLayoutSaveRef,
    refreshWorkspaceSummaries,
    flushPendingWorkspaceLayoutSave,
    navigateToWorkspace,
    handleOpenWorkspaceHere,
    handleOpenNewWorkspaceHere,
    handleOpenNewWorkspaceWindow,
    handleDeleteWorkspace,
    handleRenameWorkspace,
  };
}
