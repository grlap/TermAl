// Owns: SourcePanel file loading, saving, dirty-state, and tab decorations for
// SessionPaneView.
// Does not own: SourcePanel rendering, workspace tab selection, or transcript
// scrolling.
// Split from: ui/src/SessionPaneView.tsx.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { fetchFile, saveFile } from "./api";
import { getErrorMessage } from "./app-utils";
import type { PaneTabDecoration } from "./panels/PaneTabs";
import type {
  SourceFileState,
  SourceSaveOptions,
} from "./panels/SourcePanel";
import {
  isSourceFileMissingError,
  sourceFileStateFromResponse,
} from "./source-file-state";
import type { WorkspaceFilesChangedEvent } from "./types";
import type { PaneViewMode, WorkspaceSourceTab } from "./workspace";
import { workspaceFilesChangedEventChangeForPath } from "./workspace-file-events";

const EMPTY_SOURCE_FILE_STATE: SourceFileState = {
  status: "idle",
  path: "",
  content: "",
  contentHash: null,
  mtimeMs: null,
  sizeBytes: null,
  staleOnDisk: false,
  externalChangeKind: null,
  externalContentHash: null,
  externalMtimeMs: null,
  externalSizeBytes: null,
  error: null,
  language: null,
};

type UseSessionPaneSourceFileStateParams = {
  activeSourceOriginProjectId: string | null;
  activeSourceOriginSessionId: string | null;
  activeSourceTab: WorkspaceSourceTab | null;
  activeSourceWorkspaceRoot: string | null;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  paneId: string;
  paneSourcePath: string | null | undefined;
  paneViewMode: PaneViewMode;
  sourceCandidatePaths: string[];
  workspaceFilesChangedEvent: WorkspaceFilesChangedEvent | null;
};

export function useSessionPaneSourceFileState({
  activeSourceOriginProjectId,
  activeSourceOriginSessionId,
  activeSourceTab,
  activeSourceWorkspaceRoot,
  onPaneSourcePathChange,
  paneId,
  paneSourcePath,
  paneViewMode,
  sourceCandidatePaths,
  workspaceFilesChangedEvent,
}: UseSessionPaneSourceFileStateParams) {
  const [fileState, setFileState] = useState<SourceFileState>(
    EMPTY_SOURCE_FILE_STATE,
  );
  const [sourceEditorDirty, setSourceEditorDirty] = useState(false);
  const fileStateRef = useRef(fileState);
  const sourceEditorDirtyRef = useRef(false);

  useEffect(() => {
    fileStateRef.current = fileState;
  }, [fileState]);

  async function handleSourceFileSave(
    path: string,
    content: string,
    sessionId: string | null,
    projectId: string | null,
    options?: SourceSaveOptions,
  ) {
    if (!sessionId && !projectId) {
      throw new Error(
        "This file view is no longer associated with a live session or project.",
      );
    }

    const response = await saveFile(path, content, {
      sessionId,
      projectId,
      baseHash:
        options?.baseHash !== undefined
          ? options.baseHash
          : fileState.status === "ready" && fileState.path === path
            ? fileState.contentHash
            : null,
      overwrite: options?.overwrite,
    });
    return response;
  }

  async function handleSourceFileFetchLatest(
    path: string,
    sessionId: string | null,
    projectId: string | null,
  ) {
    if (!sessionId && !projectId) {
      throw new Error(
        "This file view is no longer associated with a live session or project.",
      );
    }

    const response = await fetchFile(path, {
      sessionId,
      projectId,
    });
    return sourceFileStateFromResponse(response);
  }

  async function handleSourceFileReload(
    path: string,
    sessionId: string | null,
    projectId: string | null,
  ) {
    const nextFileState = await handleSourceFileFetchLatest(
      path,
      sessionId,
      projectId,
    );
    return nextFileState;
  }

  function handleSourceFileAdopt(nextFileState: SourceFileState) {
    setFileState(nextFileState);
  }

  const handleSourceEditorDirtyChange = useCallback((isDirty: boolean) => {
    sourceEditorDirtyRef.current = isDirty;
    setSourceEditorDirty(isDirty);
  }, []);

  useEffect(() => {
    if (paneViewMode !== "source") {
      return;
    }

    if (!paneSourcePath && sourceCandidatePaths[0]) {
      onPaneSourcePathChange(paneId, sourceCandidatePaths[0]);
    }
  }, [
    onPaneSourcePathChange,
    paneId,
    paneSourcePath,
    paneViewMode,
    sourceCandidatePaths,
  ]);

  useEffect(() => {
    let cancelled = false;

    async function loadFile(path: string) {
      if (!activeSourceOriginSessionId && !activeSourceOriginProjectId) {
        setFileState({
          ...EMPTY_SOURCE_FILE_STATE,
          status: "error",
          path,
          error:
            "This file view is no longer associated with a live session or project.",
        });
        sourceEditorDirtyRef.current = false;
        setSourceEditorDirty(false);
        return;
      }

      setFileState({
        ...EMPTY_SOURCE_FILE_STATE,
        status: "loading",
        path,
      });
      sourceEditorDirtyRef.current = false;
      setSourceEditorDirty(false);

      try {
        const response = await fetchFile(path, {
          sessionId: activeSourceOriginSessionId,
          projectId: activeSourceOriginProjectId,
        });
        if (cancelled) {
          return;
        }

        sourceEditorDirtyRef.current = false;
        setSourceEditorDirty(false);
        setFileState(sourceFileStateFromResponse(response));
      } catch (error) {
        if (cancelled) {
          return;
        }

        setFileState({
          ...EMPTY_SOURCE_FILE_STATE,
          status: "error",
          path,
          error: getErrorMessage(error),
        });
        sourceEditorDirtyRef.current = false;
        setSourceEditorDirty(false);
      }
    }

    if (paneViewMode === "source" && paneSourcePath) {
      void loadFile(paneSourcePath);
    } else if (paneViewMode === "source") {
      setFileState(EMPTY_SOURCE_FILE_STATE);
      sourceEditorDirtyRef.current = false;
      setSourceEditorDirty(false);
    }

    return () => {
      cancelled = true;
    };
  }, [
    activeSourceOriginProjectId,
    activeSourceOriginSessionId,
    paneSourcePath,
    paneViewMode,
  ]);

  useEffect(() => {
    if (
      !workspaceFilesChangedEvent ||
      paneViewMode !== "source" ||
      !paneSourcePath ||
      (!activeSourceOriginSessionId && !activeSourceOriginProjectId)
    ) {
      return;
    }

    const fileChangeEvent = workspaceFilesChangedEvent;
    let cancelled = false;

    async function checkOpenSourceFile() {
      const current = fileStateRef.current;
      if (
        current.status !== "ready" ||
        current.path !== paneSourcePath ||
        !current.contentHash
      ) {
        return;
      }

      const fileChange = workspaceFilesChangedEventChangeForPath(
        fileChangeEvent,
        current.path,
        {
          rootPath: activeSourceWorkspaceRoot,
          sessionId: activeSourceOriginSessionId,
        },
      );
      if (!fileChange) {
        return;
      }

      if (fileChange.kind === "deleted") {
        setFileState((latest) =>
          latest.status === "ready" &&
          latest.path === current.path &&
          latest.contentHash === current.contentHash
            ? {
                ...latest,
                staleOnDisk: true,
                externalChangeKind: "deleted",
                externalContentHash: null,
                externalMtimeMs: fileChange.mtimeMs ?? null,
                externalSizeBytes: fileChange.sizeBytes ?? null,
              }
            : latest,
        );
        return;
      }

      try {
        const response = await fetchFile(current.path, {
          sessionId: activeSourceOriginSessionId,
          projectId: activeSourceOriginProjectId,
        });
        if (cancelled) {
          return;
        }

        const nextHash = response.contentHash ?? null;
        if (!nextHash || nextHash === current.contentHash) {
          if (current.staleOnDisk) {
            setFileState((latest) =>
              latest.status === "ready" &&
              latest.path === current.path &&
              latest.contentHash === current.contentHash
                ? {
                    ...latest,
                    staleOnDisk: false,
                    externalChangeKind: null,
                    externalContentHash: null,
                    externalMtimeMs: null,
                    externalSizeBytes: null,
                  }
                : latest,
            );
          }
          return;
        }

        if (sourceEditorDirtyRef.current) {
          setFileState((latest) =>
            latest.status === "ready" &&
            latest.path === current.path &&
            latest.contentHash === current.contentHash
              ? {
                  ...latest,
                  staleOnDisk: true,
                  externalChangeKind: fileChange.kind,
                  externalContentHash: nextHash,
                  externalMtimeMs: response.mtimeMs ?? null,
                  externalSizeBytes: response.sizeBytes ?? null,
                }
              : latest,
          );
          return;
        }

        setFileState(sourceFileStateFromResponse(response));
      } catch (error) {
        if (!cancelled && isSourceFileMissingError(error)) {
          setFileState((latest) =>
            latest.status === "ready" &&
            latest.path === current.path &&
            latest.contentHash === current.contentHash
              ? {
                  ...latest,
                  staleOnDisk: true,
                  externalChangeKind: "deleted",
                  externalContentHash: null,
                  externalMtimeMs: fileChange.mtimeMs ?? null,
                  externalSizeBytes: fileChange.sizeBytes ?? null,
                }
              : latest,
          );
        }
      }
    }

    void checkOpenSourceFile();

    return () => {
      cancelled = true;
    };
  }, [
    activeSourceOriginProjectId,
    activeSourceOriginSessionId,
    activeSourceWorkspaceRoot,
    paneSourcePath,
    paneViewMode,
    workspaceFilesChangedEvent,
  ]);

  const tabDecorations = useMemo<Record<string, PaneTabDecoration>>(() => {
    if (!activeSourceTab || fileState.status !== "ready") {
      return {};
    }

    let decoration: PaneTabDecoration | null = null;
    if (fileState.staleOnDisk && sourceEditorDirty) {
      decoration = {
        label: "Conflict",
        tone: "danger",
        title: "This file changed on disk while you have unsaved edits.",
      };
    } else if (fileState.staleOnDisk) {
      decoration = {
        label: "Changed",
        tone: "info",
        title: "This file changed on disk.",
      };
    } else if (sourceEditorDirty) {
      decoration = {
        label: "Unsaved",
        tone: "warning",
        title: "This file has unsaved editor changes.",
      };
    }

    return decoration ? { [activeSourceTab.id]: decoration } : {};
  }, [activeSourceTab, fileState, sourceEditorDirty]);

  return {
    fileState,
    handleSourceEditorDirtyChange,
    handleSourceFileAdopt,
    handleSourceFileFetchLatest,
    handleSourceFileReload,
    handleSourceFileSave,
    tabDecorations,
  };
}
