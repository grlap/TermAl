import { Suspense, lazy, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import {
  fetchFile,
  fetchReviewDocument,
  saveReviewDocument,
  type FileResponse,
  type GitDiffSection,
  type ReviewAnchor,
  type ReviewComment,
  type ReviewDocument,
  type ReviewThread,
} from "../api";
import { copyTextToClipboard } from "../clipboard";
import { FileTabIcon } from "../file-tab-icon";
import type { MonacoCodeEditorStatus } from "../MonacoCodeEditor";
import type { MonacoDiffEditorHandle, MonacoDiffEditorStatus } from "../MonacoDiffEditor";
import { buildDiffPreviewModel } from "../diff-preview";
import { resolveMonacoLanguage, type MonacoAppearance } from "../monaco";
import { normalizeDisplayPath, relativizePathToWorkspace } from "../path-display";
import type { DiffMessage, WorkspaceFilesChangedEvent } from "../types";
import { rebaseContentOntoDisk, type SourceSaveOptions } from "./SourcePanel";
import { StructuredDiffView } from "./StructuredDiffView";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);
const MonacoDiffEditor = lazy(() =>
  import("../MonacoDiffEditor").then(({ MonacoDiffEditor }) => ({ default: MonacoDiffEditor })),
);

type DiffViewMode = "all" | "changes" | "edit" | "raw";

type LatestFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  contentHash?: string | null;
  error: string | null;
  language: string | null;
};

type ReviewState = {
  status: "idle" | "loading" | "ready" | "error";
  review: ReviewDocument | null;
  reviewFilePath: string | null;
  error: string | null;
};

type ReviewOriginContext = {
  agentName: string | null;
  messageId: string;
  sessionId: string | null;
  workdir: string | null;
};

const DEFAULT_EDITOR_STATUS: MonacoCodeEditorStatus = {
  line: 1,
  column: 1,
  tabSize: 2,
  insertSpaces: true,
  endOfLine: "LF",
};

const DEFAULT_DIFF_EDITOR_STATUS: MonacoDiffEditorStatus = {
  ...DEFAULT_EDITOR_STATUS,
  changeCount: 0,
  currentChange: 0,
};

const LANGUAGE_LABELS: Record<string, string> = {
  bash: "Shell Script",
  css: "CSS",
  dockerfile: "Dockerfile",
  go: "Go",
  html: "HTML",
  ini: "INI",
  javascript: "JavaScript",
  json: "JSON",
  markdown: "Markdown",
  plaintext: "Plain Text",
  powershell: "PowerShell",
  python: "Python",
  rust: "Rust",
  shell: "Shell Script",
  sql: "SQL",
  typescript: "TypeScript",
  xml: "XML",
  yaml: "YAML",
};

export function DiffPanel({
  appearance,
  changeType,
  changeSetId = null,
  fontSizePx,
  diff,
  diffMessageId,
  filePath,
  gitSectionId = null,
  language,
  sessionId,
  projectId = null,
  originAgentName = null,
  workspaceRoot = null,
  workspaceFilesChangedEvent = null,
  onOpenPath,
  onInsertReviewIntoPrompt,
  onOpenConversation,
  onSaveFile,
  summary,
}: {
  appearance: MonacoAppearance;
  changeType: DiffMessage["changeType"];
  changeSetId?: string | null;
  fontSizePx: number;
  diff: string;
  diffMessageId: string;
  filePath: string | null;
  gitSectionId?: GitDiffSection | null;
  language?: string | null;
  sessionId: string | null;
  projectId?: string | null;
  originAgentName?: string | null;
  workspaceRoot?: string | null;
  workspaceFilesChangedEvent?: WorkspaceFilesChangedEvent | null;
  onOpenPath: (path: string) => void;
  onInsertReviewIntoPrompt?: (reviewFilePath: string, prompt: string) => void;
  onOpenConversation?: () => void;
  onSaveFile: (path: string, content: string, options?: SourceSaveOptions) => Promise<FileResponse | void>;
  summary: string;
}) {
  const [latestFile, setLatestFile] = useState<LatestFileState>(() => createInitialLatestFileState(filePath));
  const [reviewState, setReviewState] = useState<ReviewState>({
    status: "idle",
    review: null,
    reviewFilePath: null,
    error: null,
  });
  const normalizedSessionId = sessionId?.trim() ?? "";
  const normalizedProjectId = projectId?.trim() ?? "";
  const normalizedChangeSetId = changeSetId?.trim() ?? "";
  const hasScope = Boolean(normalizedSessionId || normalizedProjectId);
  const [editValue, setEditValue] = useState("");
  const [visualBaseContent, setVisualBaseContent] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [isRebasingFile, setIsRebasingFile] = useState(false);
  const [isReloadingFile, setIsReloadingFile] = useState(false);
  const [diffEditConflictOnDisk, setDiffEditConflictOnDisk] = useState(false);
  const [reviewSaveError, setReviewSaveError] = useState<string | null>(null);
  const [isSavingReview, setIsSavingReview] = useState(false);
  const [externalFileNotice, setExternalFileNotice] = useState<string | null>(null);
  const [editEditorStatus, setEditEditorStatus] = useState<MonacoCodeEditorStatus>(DEFAULT_EDITOR_STATUS);
  const [visualEditorStatus, setVisualEditorStatus] = useState<MonacoDiffEditorStatus>(DEFAULT_DIFF_EDITOR_STATUS);
  const [copiedPath, setCopiedPath] = useState(false);
  const [copiedReviewPath, setCopiedReviewPath] = useState(false);
  const diffEditorRef = useRef<MonacoDiffEditorHandle | null>(null);
  const pendingEditValueRef = useRef<string | null>(null);

  const previewSourceContent = visualBaseContent ?? (latestFile.status === "ready" ? latestFile.content : null);
  const preview = useMemo(
    () => buildDiffPreviewModel(diff, changeType, previewSourceContent),
    [changeType, diff, previewSourceContent],
  );
  const [viewMode, setViewMode] = useState<DiffViewMode>(() =>
    defaultDiffViewMode(buildDiffPreviewModel(diff, changeType).hasStructuredPreview, Boolean(filePath)),
  );

  useEffect(() => {
    setViewMode(defaultDiffViewMode(preview.hasStructuredPreview, Boolean(filePath)));
    setEditEditorStatus(DEFAULT_EDITOR_STATUS);
    setVisualEditorStatus(DEFAULT_DIFF_EDITOR_STATUS);
    setSaveError(null);
    setExternalFileNotice(null);
    setDiffEditConflictOnDisk(false);
    setReviewSaveError(null);
    setIsSaving(false);
    setIsRebasingFile(false);
    setIsReloadingFile(false);
    setIsSavingReview(false);
  }, [diffMessageId, filePath, preview.hasStructuredPreview]);

  useEffect(() => {
    let cancelled = false;

    if (!filePath) {
      setLatestFile(createInitialLatestFileState(null));
      setVisualBaseContent(null);
      setExternalFileNotice(null);
      setDiffEditConflictOnDisk(false);
      return;
    }

    if (!hasScope) {
      setVisualBaseContent(null);
      setExternalFileNotice(null);
      setDiffEditConflictOnDisk(false);
      setLatestFile({
        status: "error",
        path: filePath,
        content: "",
        error: "This diff preview is no longer associated with a live session or project.",
        language: language ?? null,
      });
      return;
    }

    setVisualBaseContent(null);
    setExternalFileNotice(null);
    setDiffEditConflictOnDisk(false);
    setLatestFile({
      status: "loading",
      path: filePath,
      content: "",
      error: null,
      language: language ?? null,
    });

    void fetchFile(filePath, {
      sessionId: normalizedSessionId || null,
      projectId: normalizedProjectId || null,
    })
      .then((response) => {
        if (cancelled) {
          return;
        }

        setVisualBaseContent(response.content);
        setExternalFileNotice(null);
        setDiffEditConflictOnDisk(false);
        setLatestFile(toLatestFileState(response));
      })
      .catch((error) => {
        if (cancelled) {
          return;
        }

        setLatestFile({
          status: "error",
          path: filePath,
          content: "",
          error: getErrorMessage(error),
          language: language ?? null,
        });
      });

    return () => {
      cancelled = true;
    };
  }, [filePath, hasScope, language, normalizedProjectId, normalizedSessionId]);

  useEffect(() => {
    let cancelled = false;

    if (!normalizedChangeSetId) {
      setReviewState({
        status: "idle",
        review: null,
        reviewFilePath: null,
        error: null,
      });
      return;
    }

    if (!hasScope) {
      setReviewState({
        status: "error",
        review: null,
        reviewFilePath: null,
        error: "This diff preview is no longer associated with a live session or project.",
      });
      return;
    }

    setReviewState({
      status: "loading",
      review: null,
      reviewFilePath: null,
      error: null,
    });

    void fetchReviewDocument(normalizedChangeSetId, {
      sessionId: normalizedSessionId || null,
      projectId: normalizedProjectId || null,
    })
      .then((response) => {
        if (cancelled) {
          return;
        }

        setReviewState({
          status: "ready",
          review: response.review,
          reviewFilePath: response.reviewFilePath,
          error: null,
        });
      })
      .catch((error) => {
        if (cancelled) {
          return;
        }

        setReviewState({
          status: "error",
          review: null,
          reviewFilePath: null,
          error: getErrorMessage(error),
        });
      });

    return () => {
      cancelled = true;
    };
  }, [hasScope, normalizedChangeSetId, normalizedProjectId, normalizedSessionId]);

  useEffect(() => {
    if (latestFile.status === "ready") {
      const pendingEditValue = pendingEditValueRef.current;
      pendingEditValueRef.current = null;
      setEditValue(pendingEditValue ?? latestFile.content);
      setSaveError(null);
      setEditEditorStatus(createEditorStatusSnapshot(pendingEditValue ?? latestFile.content));
      return;
    }

    if (latestFile.status !== "loading") {
      pendingEditValueRef.current = null;
      setEditValue("");
      setEditEditorStatus(DEFAULT_EDITOR_STATUS);
    }
  }, [latestFile.content, latestFile.path, latestFile.status]);

  const visualLanguage = formatLanguageLabel(language, filePath);
  const editLanguage = latestFile.status === "ready"
    ? formatLanguageLabel(latestFile.language ?? language ?? null, latestFile.path)
    : formatLanguageLabel(language, filePath);
  const canEditVisualDiff =
    preview.hasStructuredPreview && latestFile.status === "ready" && Boolean(filePath);
  const hasVisualNavigation = viewMode === "all" && visualEditorStatus.changeCount > 0;
  const isDirty = latestFile.status === "ready" && editValue !== latestFile.content;
  const saveStateLabel = saveError ? "Save failed" : isSaving ? "Saving..." : isDirty ? "Unsaved changes" : null;
  const gitSectionLabel =
    gitSectionId === "staged" ? "Staged" : gitSectionId === "unstaged" ? "Unstaged" : null;
  const visibleSummary = gitSectionLabel ? null : summary;
  const displayFilePath = useMemo(() => {
    if (!filePath) {
      return null;
    }

    return normalizeDisplayPath(relativizePathToWorkspace(filePath, workspaceRoot));
  }, [filePath, workspaceRoot]);
  const filePathTitle = filePath ? normalizeDisplayPath(filePath) : null;
  const copyablePath = displayFilePath ?? filePathTitle;
  const hasReviewScope = normalizedChangeSetId.length > 0 && hasScope;
  const canEditReview = hasReviewScope && reviewState.status === "ready";
  const reviewThreads = useMemo<ReviewThread[]>(
    () => reviewState.review?.threads ?? [],
    [reviewState.review],
  );
  const openReviewThreadCount = useMemo(
    () => reviewThreads.filter((thread) => thread.status === "open").length,
    [reviewThreads],
  );
  const reviewOriginContext = useMemo<ReviewOriginContext>(
    () => ({
      agentName: originAgentName,
      messageId: diffMessageId,
      sessionId: normalizedSessionId || null,
      workdir: workspaceRoot ?? null,
    }),
    [diffMessageId, normalizedSessionId, originAgentName, workspaceRoot],
  );

  useEffect(() => {
    if (!workspaceFilesChangedEvent || !filePath || !hasScope) {
      return;
    }

    const change = workspaceFilesChangedEventChangeForPath(workspaceFilesChangedEvent, filePath);
    if (!change) {
      return;
    }

    const wasDirty = latestFile.status === "ready" && editValue !== latestFile.content;
    if (change.kind === "deleted") {
      if (wasDirty) {
        setExternalFileNotice("The file was deleted on disk. Your diff edit buffer is preserved.");
        setDiffEditConflictOnDisk(true);
        return;
      }

      setVisualBaseContent(null);
      setDiffEditConflictOnDisk(false);
      setLatestFile({
        status: "error",
        path: filePath,
        content: "",
        error: "File was deleted on disk.",
        language: language ?? null,
      });
      setEditValue("");
      setExternalFileNotice("The file was deleted on disk.");
      return;
    }

    let cancelled = false;
    void fetchFile(filePath, {
      sessionId: normalizedSessionId || null,
      projectId: normalizedProjectId || null,
    })
      .then((response) => {
        if (cancelled) {
          return;
        }

        if (wasDirty && latestFile.status === "ready") {
          const rebaseResult = rebaseContentOntoDisk(
            latestFile.content,
            editValue,
            response.content,
          );
          if (rebaseResult.status === "conflict") {
            setExternalFileNotice(rebaseResult.reason);
            setDiffEditConflictOnDisk(true);
            return;
          }

          pendingEditValueRef.current = rebaseResult.content;
          setVisualBaseContent(response.content);
          setLatestFile(toLatestFileState(response));
          setEditValue(rebaseResult.content);
          setExternalFileNotice("File changed on disk; your diff edits were applied on top.");
          setDiffEditConflictOnDisk(false);
          return;
        }

        setVisualBaseContent(response.content);
        setLatestFile(toLatestFileState(response));
        setExternalFileNotice("File refreshed from disk.");
        setDiffEditConflictOnDisk(false);
      })
      .catch((error) => {
        if (cancelled) {
          return;
        }

        setExternalFileNotice(`File changed on disk, but refresh failed: ${getErrorMessage(error)}`);
        setDiffEditConflictOnDisk(wasDirty);
      });

    return () => {
      cancelled = true;
    };
  }, [
    workspaceFilesChangedEvent,
    filePath,
    hasScope,
    language,
    normalizedProjectId,
    normalizedSessionId,
  ]);

  useEffect(() => {
    if (!copiedPath) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedPath(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedPath]);

  useEffect(() => {
    if (!copiedReviewPath) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedReviewPath(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedReviewPath]);

  async function handleSave(options?: SourceSaveOptions) {
    if (latestFile.status !== "ready" || !isDirty || isSaving) {
      return;
    }

    setIsSaving(true);
    setSaveError(null);
    try {
      const response = await onSaveFile(latestFile.path, editValue, {
        baseHash: latestFile.contentHash ?? null,
        overwrite: options?.overwrite,
      });
      if (response) {
        setVisualBaseContent(response.content);
        setLatestFile(toLatestFileState(response));
      } else {
        setLatestFile((current) => {
          if (current.status !== "ready") {
            return current;
          }

          return {
            ...current,
            content: editValue,
            contentHash: null,
          };
        });
      }
      setExternalFileNotice(null);
      setDiffEditConflictOnDisk(false);
    } catch (error) {
      const message = getErrorMessage(error);
      setSaveError(message);
      if (isStaleFileSaveError(message)) {
        setExternalFileNotice(
          "The file changed on disk before save. Apply your edits to the disk version, reload from disk, or save anyway.",
        );
        setDiffEditConflictOnDisk(true);
      }
    } finally {
      setIsSaving(false);
    }
  }

  async function handleApplyDiffEditsToDiskVersion() {
    if (
      latestFile.status !== "ready" ||
      !isDirty ||
      !hasScope ||
      isRebasingFile
    ) {
      return;
    }

    setIsRebasingFile(true);
    setSaveError(null);
    try {
      const response = await fetchFile(latestFile.path, {
        sessionId: normalizedSessionId || null,
        projectId: normalizedProjectId || null,
      });
      const rebaseResult = rebaseContentOntoDisk(
        latestFile.content,
        editValue,
        response.content,
      );
      if (rebaseResult.status === "conflict") {
        setExternalFileNotice(rebaseResult.reason);
        setDiffEditConflictOnDisk(true);
        return;
      }

      pendingEditValueRef.current = rebaseResult.content;
      setVisualBaseContent(response.content);
      setLatestFile(toLatestFileState(response));
      setEditValue(rebaseResult.content);
      setExternalFileNotice("Your diff edits were applied on top of the disk version.");
      setDiffEditConflictOnDisk(false);
    } catch (error) {
      setExternalFileNotice(`Could not load the disk version: ${getErrorMessage(error)}`);
      setDiffEditConflictOnDisk(true);
    } finally {
      setIsRebasingFile(false);
    }
  }

  async function handleReloadDiffFileFromDisk() {
    if (latestFile.status !== "ready" || !hasScope || isReloadingFile) {
      return;
    }

    setIsReloadingFile(true);
    setSaveError(null);
    try {
      const response = await fetchFile(latestFile.path, {
        sessionId: normalizedSessionId || null,
        projectId: normalizedProjectId || null,
      });
      pendingEditValueRef.current = null;
      setVisualBaseContent(response.content);
      setLatestFile(toLatestFileState(response));
      setEditValue(response.content);
      setExternalFileNotice("File reloaded from disk.");
      setDiffEditConflictOnDisk(false);
    } catch (error) {
      setExternalFileNotice(`Could not reload from disk: ${getErrorMessage(error)}`);
      setDiffEditConflictOnDisk(true);
    } finally {
      setIsReloadingFile(false);
    }
  }

  async function handleCopyPath() {
    if (!copyablePath) {
      return;
    }

    try {
      await copyTextToClipboard(copyablePath);
      setCopiedPath(true);
    } catch {
      setCopiedPath(false);
    }
  }

  async function handleCopyReviewPath() {
    if (!reviewState.reviewFilePath) {
      return;
    }

    try {
      await copyTextToClipboard(reviewState.reviewFilePath);
      setCopiedReviewPath(true);
    } catch {
      setCopiedReviewPath(false);
    }
  }

  async function mutateReviewDocument(mutator: (current: ReviewDocument) => ReviewDocument) {
    if (!normalizedChangeSetId) {
      throw new Error("This diff preview does not have a stable change set id yet.");
    }
    if (!hasScope) {
      throw new Error("This diff preview is no longer associated with a live session or project.");
    }
    if (reviewState.status !== "ready") {
      throw new Error("Review threads must load successfully before they can be edited.");
    }

    setIsSavingReview(true);
    setReviewSaveError(null);
    try {
      const nextReview = mutator(
        ensureReviewDocument(reviewState.review, normalizedChangeSetId, {
          changeType,
          filePath,
          origin: reviewOriginContext,
        }),
      );
      const response = await saveReviewDocument(normalizedChangeSetId, nextReview, {
        sessionId: normalizedSessionId || null,
        projectId: normalizedProjectId || null,
      });
      setReviewState({
        status: "ready",
        review: response.review,
        reviewFilePath: response.reviewFilePath,
        error: null,
      });
    } catch (error) {
      setReviewSaveError(getErrorMessage(error));
      throw error;
    } finally {
      setIsSavingReview(false);
    }
  }

  async function handleCreateThread(anchor: ReviewAnchor, body: string) {
    await mutateReviewDocument((current) => ({
      ...current,
      files: ensureReviewFiles(current.files ?? [], filePath, changeType),
      threads: [
        ...(current.threads ?? []),
        {
          id: `thread-${crypto.randomUUID()}`,
          anchor,
          status: "open",
          comments: [createReviewComment(body)],
        },
      ],
    }));
  }

  async function handleReplyToThread(threadId: string, body: string) {
    await mutateReviewDocument((current) => ({
      ...current,
      threads: (current.threads ?? []).map((thread) =>
        thread.id === threadId
          ? {
              ...thread,
              comments: [...thread.comments, createReviewComment(body)],
            }
          : thread,
      ),
    }));
  }

  async function handleUpdateThreadStatus(
    threadId: string,
    status: ReviewThread["status"],
  ) {
    await mutateReviewDocument((current) => ({
      ...current,
      threads: (current.threads ?? []).map((thread) =>
        thread.id === threadId
          ? {
              ...thread,
              status,
            }
          : thread,
      ),
    }));
  }

  function handleInsertIntoPrompt() {
    if (!reviewState.reviewFilePath || !onInsertReviewIntoPrompt) {
      return;
    }

    onInsertReviewIntoPrompt(
      reviewState.reviewFilePath,
      buildReviewHandoffPrompt(reviewState.reviewFilePath, reviewThreads),
    );
  }

  return (
    <div className="source-pane diff-preview-panel has-editor">
      <div className="source-toolbar">
        <div className="source-editor-toolbar">
          <div className="source-editor-status">
            {gitSectionLabel ? <span className="chip">{gitSectionLabel}</span> : null}
            {!gitSectionLabel && changeType === "create" ? <span className="chip">New file</span> : null}
            {preview.changeSummary.changedLineCount > 0 ? (
              <span
                className="diff-preview-stat diff-preview-stat-changed"
                aria-label={`Changed lines: ${preview.changeSummary.changedLineCount}`}
                title={`Changed lines: ${preview.changeSummary.changedLineCount}`}
              >
                {preview.changeSummary.changedLineCount}
              </span>
            ) : null}
            {preview.changeSummary.addedLineCount > 0 ? (
              <span
                className="diff-preview-stat diff-preview-stat-added"
                aria-label={`Added lines: ${preview.changeSummary.addedLineCount}`}
                title={`Added lines: ${preview.changeSummary.addedLineCount}`}
              >
                +{preview.changeSummary.addedLineCount}
              </span>
            ) : null}
            {preview.changeSummary.removedLineCount > 0 ? (
              <span
                className="diff-preview-stat diff-preview-stat-removed"
                aria-label={`Removed lines: ${preview.changeSummary.removedLineCount}`}
                title={`Removed lines: ${preview.changeSummary.removedLineCount}`}
              >
                -{preview.changeSummary.removedLineCount}
              </span>
            ) : null}
            {filePath || language ? (
              <div className="diff-preview-file-meta" title={filePathTitle ?? undefined}>
                <FileTabIcon className="diff-preview-file-icon" language={language ?? null} path={filePath} />
                {displayFilePath ? <span className="diff-preview-file-path">{displayFilePath}</span> : null}
                {copyablePath ? (
                  <button
                    className={`command-icon-button diff-preview-copy-button${copiedPath ? " copied" : ""}`}
                    type="button"
                    onMouseDown={(event) => {
                      event.preventDefault();
                      event.stopPropagation();
                    }}
                    onClick={() => void handleCopyPath()}
                    aria-label={copiedPath ? "Path copied" : "Copy path"}
                    title={copiedPath ? "Copied" : "Copy path"}
                  >
                    {copiedPath ? <CheckIcon /> : <CopyIcon />}
                  </button>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="source-editor-actions diff-preview-actions">
            {preview.hasStructuredPreview ? (
              <DiffPreviewToggleButton
                selected={viewMode === "all"}
                label="All lines"
                onClick={() => setViewMode("all")}
              >
                <AllLinesIcon />
              </DiffPreviewToggleButton>
            ) : null}
            {preview.hasStructuredPreview ? (
              <DiffPreviewToggleButton
                selected={viewMode === "changes"}
                label="Changed only"
                onClick={() => setViewMode("changes")}
              >
                <ChangedOnlyIcon />
              </DiffPreviewToggleButton>
            ) : null}
            {filePath ? (
              <DiffPreviewToggleButton
                selected={viewMode === "edit"}
                label="Edit mode"
                onClick={() => setViewMode("edit")}
              >
                <EditModeIcon />
              </DiffPreviewToggleButton>
            ) : null}
            <DiffPreviewToggleButton
              selected={viewMode === "raw"}
              label="Raw patch"
              onClick={() => setViewMode("raw")}
            >
              <RawPatchIcon />
            </DiffPreviewToggleButton>
            {filePath ? (
              <button className="ghost-button" type="button" onClick={() => onOpenPath(filePath)}>
                Open file
              </button>
            ) : null}
            {onOpenConversation ? (
              <button className="ghost-button" type="button" onClick={onOpenConversation}>
                Back to conversation
              </button>
            ) : null}
            {reviewState.reviewFilePath ? (
              <button className="ghost-button" type="button" onClick={() => void handleCopyReviewPath()}>
                {copiedReviewPath ? "Review path copied" : "Copy review path"}
              </button>
            ) : null}
            {reviewState.reviewFilePath && onInsertReviewIntoPrompt ? (
              <button
                className="ghost-button"
                type="button"
                onClick={handleInsertIntoPrompt}
                disabled={openReviewThreadCount === 0}
                title={
                  openReviewThreadCount === 0
                    ? "No open review threads to insert."
                    : "Insert the review handoff into the session draft."
                }
              >
                Insert review into prompt
              </button>
            ) : null}
          </div>
        </div>
        {visibleSummary ? <p className="support-copy file-viewer-summary diff-preview-summary">{visibleSummary}</p> : null}
        {reviewState.status === "error" ? (
          <p className="support-copy diff-preview-note">{`Review threads unavailable: ${reviewState.error}`}</p>
        ) : null}
        {reviewState.reviewFilePath ? (
          <div className="diff-review-summary" aria-label="Change-set review threads">
            <span className="chip">{`${reviewThreads.length} review thread${reviewThreads.length === 1 ? "" : "s"}`}</span>
            <span className="chip">{`${openReviewThreadCount} open`}</span>
            <span className="support-copy diff-review-summary-path">{reviewState.reviewFilePath}</span>
            {isSavingReview ? <span className="support-copy">Saving review...</span> : null}
          </div>
        ) : null}
        {reviewSaveError ? (
          <p className="support-copy diff-preview-note">{`Review update failed: ${reviewSaveError}`}</p>
        ) : null}
        {externalFileNotice ? (
          <p className="support-copy diff-preview-note">{externalFileNotice}</p>
        ) : null}
        {diffEditConflictOnDisk && latestFile.status === "ready" ? (
          <div className="source-file-change-actions diff-preview-file-change-actions">
            <button
              className="ghost-button"
              type="button"
              disabled={!isDirty || isRebasingFile}
              onClick={() => void handleApplyDiffEditsToDiskVersion()}
            >
              {isRebasingFile ? "Applying..." : "Apply my edits to disk version"}
            </button>
            <button
              className="ghost-button"
              type="button"
              disabled={isSaving || !isDirty}
              onClick={() => void handleSave({ overwrite: true })}
            >
              {isSaving ? "Saving..." : "Save anyway"}
            </button>
            <button
              className="ghost-button"
              type="button"
              disabled={isReloadingFile}
              onClick={() => void handleReloadDiffFileFromDisk()}
            >
              {isReloadingFile ? "Reloading..." : "Reload from disk"}
            </button>
          </div>
        ) : null}
        {!normalizedChangeSetId && preview.hasStructuredPreview ? (
          <p className="support-copy diff-preview-note">
            Review comments are unavailable for this diff because it does not have a stable change set id yet.
          </p>
        ) : null}
      </div>

      <div className="source-editor-region diff-preview-region">
        {viewMode === "edit" ? (
          <>
            {renderEditFileView({
              appearance,
              editValue,
              fontSizePx,
              filePath,
              language,
              latestFile,
              onChange: setEditValue,
              onSave: handleSave,
              onStatusChange: setEditEditorStatus,
            })}
            {latestFile.status === "ready" ? (
              <footer className="source-editor-statusbar diff-preview-statusbar" aria-label="Edit mode status">
                <div className="source-editor-statusbar-group">
                  {saveStateLabel ? <span className="source-editor-statusbar-item source-editor-statusbar-state">{saveStateLabel}</span> : null}
                </div>
                <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
                  <span className="source-editor-statusbar-item">{`Ln ${editEditorStatus.line}, Col ${editEditorStatus.column}`}</span>
                  <span className="source-editor-statusbar-item">{formatIndentationLabel(editEditorStatus)}</span>
                  <span className="source-editor-statusbar-item">UTF-8</span>
                  <span className="source-editor-statusbar-item">{editEditorStatus.endOfLine}</span>
                  <span className="source-editor-statusbar-item">{editLanguage}</span>
                </div>
              </footer>
            ) : null}
          </>
        ) : null}

        {viewMode === "all" && preview.hasStructuredPreview ? (
          <div className="source-editor-shell source-editor-shell-with-statusbar">
            <div className="diff-editor-shell">
              <Suspense fallback={<div className="source-editor-loading">Loading diff editor...</div>}>
                <MonacoDiffEditor
                  ref={diffEditorRef}
                  appearance={appearance}
                  fontSizePx={fontSizePx}
                  ariaLabel={filePath ? `Diff preview for ${filePath}` : "Diff preview"}
                  language={latestFile.status === "ready" ? latestFile.language ?? language ?? null : language}
                  onChange={canEditVisualDiff ? setEditValue : undefined}
                  onSave={canEditVisualDiff ? () => void handleSave() : undefined}
                  onStatusChange={setVisualEditorStatus}
                  path={filePath}
                  readOnly={!canEditVisualDiff}
                  modifiedValue={canEditVisualDiff ? editValue : preview.modifiedText}
                  originalValue={preview.originalText}
                />
              </Suspense>
            </div>
            <footer className="source-editor-statusbar diff-preview-statusbar" aria-label="Diff status">
              <div className="source-editor-statusbar-group">
                <div className="diff-preview-change-nav" aria-label="Change navigation">
                  <button
                    className="diff-preview-nav-button"
                    type="button"
                    onClick={() => diffEditorRef.current?.goToPreviousChange()}
                    disabled={!hasVisualNavigation}
                    aria-label="Previous change"
                    title="Previous change"
                  >
                    <DiffNavArrow direction="up" />
                  </button>
                  <button
                    className="diff-preview-nav-button"
                    type="button"
                    onClick={() => diffEditorRef.current?.goToNextChange()}
                    disabled={!hasVisualNavigation}
                    aria-label="Next change"
                    title="Next change"
                  >
                    <DiffNavArrow direction="down" />
                  </button>
                </div>
                <span className="source-editor-statusbar-item source-editor-statusbar-state">
                  {formatChangeNavigationLabel(visualEditorStatus)}
                </span>
                {canEditVisualDiff && saveStateLabel ? (
                  <span className="source-editor-statusbar-item source-editor-statusbar-state">{saveStateLabel}</span>
                ) : null}
              </div>
              <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
                <span className="source-editor-statusbar-item">{`Ln ${visualEditorStatus.line}, Col ${visualEditorStatus.column}`}</span>
                <span className="source-editor-statusbar-item">{formatIndentationLabel(visualEditorStatus)}</span>
                <span className="source-editor-statusbar-item">UTF-8</span>
                <span className="source-editor-statusbar-item">{visualEditorStatus.endOfLine}</span>
                <span className="source-editor-statusbar-item">{visualLanguage}</span>
              </div>
            </footer>
          </div>
        ) : null}

        {viewMode === "changes" && preview.hasStructuredPreview ? (
          <StructuredDiffView
            filePath={filePath}
            preview={preview}
            threads={reviewThreads}
            isSavingReview={isSavingReview || reviewState.status === "loading"}
            onCreateThread={canEditReview ? (anchor, body) => handleCreateThread(anchor, body) : undefined}
            onReplyToThread={canEditReview ? (threadId, body) => handleReplyToThread(threadId, body) : undefined}
            onUpdateThreadStatus={
              canEditReview
                ? (threadId, status) => handleUpdateThreadStatus(threadId, status)
                : undefined
            }
          />
        ) : null}

        {viewMode === "raw" || (viewMode === "all" && !preview.hasStructuredPreview) ? (
          <RawPatchView diff={diff} />
        ) : null}

        {viewMode === "raw" ? (
          <footer className="source-editor-statusbar diff-preview-statusbar" aria-label="Raw patch status">
            <div className="source-editor-statusbar-group">
              <span className="source-editor-statusbar-item source-editor-statusbar-state">Raw patch</span>
            </div>
            <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
              <span className="source-editor-statusbar-item">{`${diff.split("\n").length} lines`}</span>
              <span className="source-editor-statusbar-item">UTF-8</span>
              <span className="source-editor-statusbar-item">Patch</span>
            </div>
          </footer>
        ) : null}

        {viewMode !== "edit" && (preview.note || (reviewThreads.length > 0 && viewMode !== "changes")) ? (
          <p className="support-copy diff-preview-note">
            {preview.note ?? ""}
            {preview.note && reviewThreads.length > 0 && viewMode !== "changes" ? " " : ""}
            {reviewThreads.length > 0 && viewMode !== "changes"
              ? "Review threads render inline in Changed only view."
              : ""}
          </p>
        ) : null}
      </div>
    </div>
  );
}

function DiffPreviewToggleButton({
  children,
  label,
  onClick,
  selected,
}: {
  children: ReactNode;
  label: string;
  onClick: () => void;
  selected: boolean;
}) {
  return (
    <button
      className={`ghost-button diff-preview-toggle diff-preview-toggle-icon ${selected ? "selected" : ""}`}
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function AllLinesIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M2.25 3h11.5v1.75H2.25Zm0 4.13h11.5v1.75H2.25Zm0 4.12h11.5V13H2.25Z" fill="currentColor" />
    </svg>
  );
}

function ChangedOnlyIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M2.25 3.25H6.5V5H2.25Zm7.25 0h4.25V5H9.5ZM2.25 11h4.25v1.75H2.25ZM9.5 11h4.25v1.75H9.5Z" fill="currentColor" />
      <path d="m6.45 8 1.6-1.6 1.48 1.48L8 9.41 6.47 7.88Z" fill="currentColor" />
    </svg>
  );
}

function EditModeIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="m11.77 1.88 2.35 2.35-7.4 7.4-3 .65.65-3Zm-6.28 7.87 4.83-4.83-.75-.75-4.83 4.83-.3 1.35Zm6-6 .76.75.88-.88-.76-.75Z" fill="currentColor" />
    </svg>
  );
}

function RawPatchIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M4.9 2 2 8l2.9 6h1.72L3.72 8l2.9-6Zm6.2 0L14 8l-2.9 6H9.38l2.9-6-2.9-6Z" fill="currentColor" />
    </svg>
  );
}

function CopyIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M5 2.5h6.5A1.5 1.5 0 0 1 13 4v7.5A1.5 1.5 0 0 1 11.5 13H5A1.5 1.5 0 0 1 3.5 11.5V4A1.5 1.5 0 0 1 5 2.5Zm0 1a.5.5 0 0 0-.5.5v7.5a.5.5 0 0 0 .5.5h6.5a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H5Z"
        fill="currentColor"
      />
      <path
        d="M2.5 5.5a.5.5 0 0 1 .5.5v6A1.5 1.5 0 0 0 4.5 13.5h5a.5.5 0 0 1 0 1h-5A2.5 2.5 0 0 1 2 12V6a.5.5 0 0 1 .5-.5Z"
        fill="currentColor"
      />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M13.35 4.65a.5.5 0 0 1 0 .7l-6 6a.5.5 0 0 1-.7 0l-3-3a.5.5 0 1 1 .7-.7L7 10.29l5.65-5.64a.5.5 0 0 1 .7 0Z"
        fill="currentColor"
      />
    </svg>
  );
}

function renderEditFileView({
  appearance,
  editValue,
  fontSizePx,
  filePath,
  language,
  latestFile,
  onChange,
  onSave,
  onStatusChange,
}: {
  appearance: MonacoAppearance;
  editValue: string;
  fontSizePx: number;
  filePath: string | null;
  language?: string | null;
  latestFile: LatestFileState;
  onChange: (value: string) => void;
  onSave: () => Promise<void>;
  onStatusChange: (status: MonacoCodeEditorStatus) => void;
}) {
  if (!filePath) {
    return (
      <article className="thread-notice">
        <div className="card-label">Edit mode</div>
        <p>This diff does not include a file path, so there is no file to edit.</p>
      </article>
    );
  }

  if (latestFile.status === "loading" || latestFile.status === "idle") {
    return <div className="source-editor-loading">Loading latest file...</div>;
  }

  if (latestFile.status === "error") {
    return (
      <article className="thread-notice">
        <div className="card-label">Edit mode</div>
        <p>{latestFile.error}</p>
      </article>
    );
  }

  return (
    <div className="source-editor-shell source-editor-shell-with-statusbar">
      <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
        <MonacoCodeEditor
          appearance={appearance}
          ariaLabel={`Edit mode for ${latestFile.path}`}
          fontSizePx={fontSizePx}
          language={latestFile.language ?? language ?? null}
          path={latestFile.path}
          value={editValue}
          onChange={onChange}
          onSave={() => void onSave()}
          onStatusChange={onStatusChange}
        />
      </Suspense>
    </div>
  );
}

function DiffNavArrow({ direction }: { direction: "up" | "down" }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d={direction === "up" ? "M8 3.5 13 8.5l-1.15 1.15L8.8 6.61V13h-1.6V6.61L4.15 9.65 3 8.5l5-5Z" : "M8 12.5 3 7.5l1.15-1.15L7.2 9.39V3h1.6v6.39l3.05-3.04L13 7.5l-5 5Z"}
        fill="currentColor"
      />
    </svg>
  );
}

function RawPatchView({ diff }: { diff: string }) {
  const lines = diff.split("\n");

  return (
    <div className="diff-editor-shell diff-preview-raw-shell">
      <div className="diff-preview-raw" role="table" aria-label="Raw patch preview">
        {lines.map((line, index) => (
          <div
            key={`${index}:${line}`}
            className={`diff-preview-raw-line ${rawDiffLineClassName(line)}`}
            role="row"
          >
            <span className="diff-preview-raw-line-number" aria-hidden="true">
              {index + 1}
            </span>
            <span className="diff-preview-raw-line-content" role="cell">
              {line || " "}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function rawDiffLineClassName(line: string) {
  if (line.startsWith("@@")) {
    return "diff-preview-raw-line-hunk";
  }

  if (
    line.startsWith("diff --git ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ")
  ) {
    return "diff-preview-raw-line-meta";
  }

  if (line === "\\ No newline at end of file") {
    return "diff-preview-raw-line-note";
  }

  if (line.startsWith("+") && !line.startsWith("+++ ")) {
    return "diff-preview-raw-line-added";
  }

  if (line.startsWith("-") && !line.startsWith("--- ")) {
    return "diff-preview-raw-line-removed";
  }

  return "diff-preview-raw-line-context";
}

function defaultDiffViewMode(hasStructuredPreview: boolean, hasFilePath: boolean): DiffViewMode {
  if (hasStructuredPreview) {
    return "all";
  }

  return hasFilePath ? "edit" : "raw";
}

function ensureReviewDocument(
  review: ReviewDocument | null,
  changeSetId: string,
  context: {
    changeType: DiffMessage["changeType"];
    filePath: string | null;
    origin: ReviewOriginContext;
  },
): ReviewDocument {
  if (review) {
    return {
      ...review,
      files: ensureReviewFiles(review.files ?? [], context.filePath, context.changeType),
      threads: review.threads ?? [],
    };
  }

  return {
    version: 1,
    revision: 0,
    changeSetId,
    origin:
      context.origin.sessionId &&
      context.origin.workdir &&
      context.origin.agentName
        ? {
            sessionId: context.origin.sessionId,
            messageId: context.origin.messageId,
            agent: context.origin.agentName,
            workdir: context.origin.workdir,
            createdAt: new Date().toISOString(),
          }
        : null,
    files: ensureReviewFiles([], context.filePath, context.changeType),
    threads: [],
  };
}

function ensureReviewFiles(
  files: NonNullable<ReviewDocument["files"]>,
  filePath: string | null,
  changeType: DiffMessage["changeType"],
) {
  if (!filePath || files.some((file) => file.filePath === filePath)) {
    return files;
  }

  return [...files, { filePath, changeType }];
}

function createReviewComment(body: string): ReviewComment {
  const timestamp = new Date().toISOString();
  return {
    id: `comment-${crypto.randomUUID()}`,
    author: "user",
    body: body.trim(),
    createdAt: timestamp,
    updatedAt: timestamp,
  };
}

function buildReviewHandoffPrompt(reviewFilePath: string, threads: ReviewThread[]) {
  const openThreads = threads.filter((thread) => thread.status === "open").length;
  return openThreads > 0
    ? `Please address the ${openThreads} open review thread${openThreads === 1 ? "" : "s"} in ${reviewFilePath}. Reply in each thread and resolve threads you have handled.`
    : `Review file: ${reviewFilePath}`;
}

function createInitialLatestFileState(filePath: string | null): LatestFileState {
  if (!filePath) {
    return {
      status: "idle",
      path: "",
      content: "",
      error: null,
      language: null,
    };
  }

  return {
    status: "loading",
    path: filePath,
    content: "",
    error: null,
    language: null,
  };
}

function toLatestFileState(response: FileResponse): LatestFileState {
  return {
    status: "ready",
    path: response.path,
    content: response.content,
    contentHash: response.contentHash ?? null,
    error: null,
    language: response.language ?? null,
  };
}

function normalizeWorkspaceFileEventPath(path: string) {
  const normalized = path.trim().replace(/\\+/g, "/").replace(/\/+/g, "/");
  if (!normalized) {
    return "";
  }

  const withoutTrailingSlash =
    normalized.length > 1 && !/^[a-z]:\/$/i.test(normalized)
      ? normalized.replace(/\/+$/g, "")
      : normalized;

  return /^[a-z]:\//i.test(withoutTrailingSlash)
    ? withoutTrailingSlash.toLowerCase()
    : withoutTrailingSlash;
}

function workspaceFilesChangedEventChangeForPath(
  event: WorkspaceFilesChangedEvent,
  targetPath: string,
) {
  const normalizedTargetPath = normalizeWorkspaceFileEventPath(targetPath);
  if (!normalizedTargetPath) {
    return null;
  }

  return event.changes.find(
    (change) =>
      normalizeWorkspaceFileEventPath(change.path) === normalizedTargetPath,
  ) ?? null;
}

function isStaleFileSaveError(message: string) {
  return message.toLowerCase().includes("file changed on disk before save");
}

function createEditorStatusSnapshot(content: string): MonacoCodeEditorStatus {
  return {
    ...DEFAULT_EDITOR_STATUS,
    endOfLine: content.includes("\r\n") ? "CRLF" : "LF",
  };
}

function formatChangeNavigationLabel(status: MonacoDiffEditorStatus) {
  if (status.changeCount === 0) {
    return "No changes";
  }

  return `Change ${Math.max(status.currentChange, 1)} of ${status.changeCount}`;
}

function formatIndentationLabel(status: MonacoCodeEditorStatus) {
  return status.insertSpaces ? `Spaces: ${status.tabSize}` : `Tab Size: ${status.tabSize}`;
}

function formatLanguageLabel(language: string | null | undefined, path: string | null | undefined) {
  const resolved = resolveMonacoLanguage(language ?? null, path ?? null);
  const normalizedPath = path?.trim().toLowerCase() ?? "";
  if (resolved === "typescript" && normalizedPath.endsWith(".tsx")) {
    return "TypeScript JSX";
  }
  if (resolved === "javascript" && normalizedPath.endsWith(".jsx")) {
    return "JavaScript JSX";
  }

  return LANGUAGE_LABELS[resolved] ?? resolved.replace(/[-_]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
