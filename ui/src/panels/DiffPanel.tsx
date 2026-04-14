import { Suspense, lazy, useEffect, useMemo, useRef, useState } from "react";
import type { FormEvent, KeyboardEvent, ReactNode } from "react";
import {
  fetchFile,
  fetchReviewDocument,
  saveReviewDocument,
  type FileResponse,
  type GitDiffDocumentContent,
  type GitDiffDocumentSide,
  type GitDiffSection,
  type ReviewAnchor,
  type ReviewComment,
  type ReviewDocument,
  type ReviewThread,
} from "../api";
import { copyTextToClipboard } from "../clipboard";
import { FileTabIcon } from "../file-tab-icon";
import { MarkdownContent, type MarkdownFileLinkTarget } from "../message-cards";
import type { MonacoCodeEditorStatus } from "../MonacoCodeEditor";
import type { MonacoDiffEditorHandle, MonacoDiffEditorStatus } from "../MonacoDiffEditor";
import { buildDiffPreviewModel } from "../diff-preview";
import { resolveMonacoLanguage, type MonacoAppearance } from "../monaco";
import { normalizeDisplayPath, relativizePathToWorkspace } from "../path-display";
import type { DiffMessage, WorkspaceFilesChangedEvent } from "../types";
import { workspaceFilesChangedEventChangeForPath } from "../workspace-file-events";
import { rebaseContentOntoDisk, type SourceSaveOptions } from "./SourcePanel";
import { StructuredDiffView } from "./StructuredDiffView";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);
const MonacoDiffEditor = lazy(() =>
  import("../MonacoDiffEditor").then(({ MonacoDiffEditor }) => ({ default: MonacoDiffEditor })),
);

type DiffViewMode = "all" | "changes" | "markdown" | "edit" | "raw";
type MarkdownDocumentCompleteness = "full" | "patch";
const MAX_RENDERED_MARKDOWN_UNDO_DEPTH = 100;

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
  documentContent = null,
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
  documentContent?: GitDiffDocumentContent | null;
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
  const [markdownEditContent, setMarkdownEditContent] = useState<string | null>(null);
  const [markdownEditRevision, setMarkdownEditRevision] = useState(0);
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
  const latestFileRef = useRef(latestFile);
  const editValueRef = useRef(editValue);
  const markdownEditContentRef = useRef<string | null>(markdownEditContent);
  const markdownUndoStackRef = useRef<string[]>([]);
  const markdownRedoStackRef = useRef<string[]>([]);
  const setLatestFileState = (
    next:
      | LatestFileState
      | ((current: LatestFileState) => LatestFileState),
  ) => {
    setLatestFile((current) => {
      const resolved = typeof next === "function" ? next(current) : next;
      latestFileRef.current = resolved;
      return resolved;
    });
  };
  const setEditValueState = (nextValue: string) => {
    editValueRef.current = nextValue;
    setEditValue(nextValue);
  };
  const setMarkdownEditContentState = (nextValue: string | null) => {
    markdownEditContentRef.current = nextValue;
    setMarkdownEditContent(nextValue);
  };
  const clearRenderedMarkdownEditHistory = () => {
    markdownUndoStackRef.current = [];
    markdownRedoStackRef.current = [];
  };
  const bumpRenderedMarkdownEditRevision = () => {
    setMarkdownEditRevision((current) => current + 1);
  };

  useEffect(() => {
    latestFileRef.current = latestFile;
  }, [latestFile]);

  useEffect(() => {
    editValueRef.current = editValue;
  }, [editValue]);

  useEffect(() => {
    markdownEditContentRef.current = markdownEditContent;
  }, [markdownEditContent]);

  const isMarkdownTarget = isMarkdownDocument(language, filePath);
  const previewSourceContent =
    documentContent?.after.content ?? visualBaseContent ?? (latestFile.status === "ready" ? latestFile.content : null);
  const preview = useMemo(
    () => buildDiffPreviewModel(diff, changeType, previewSourceContent),
    [changeType, diff, previewSourceContent],
  );
  const markdownPreview = useMemo(
    () => buildMarkdownDiffPreview(documentContent, preview, changeType),
    [changeType, documentContent, preview],
  );
  const canShowMarkdownView = isMarkdownTarget && markdownPreview !== null;
  const markdownDisplayPreview = useMemo<MarkdownDiffPreviewModel | null>(() => {
    if (!markdownPreview) {
      return null;
    }

    return {
      ...markdownPreview,
      after: {
        ...markdownPreview.after,
        content: markdownEditContent ?? markdownPreview.after.content,
      },
    };
  }, [markdownEditContent, markdownPreview]);
  const preferMarkdownView =
    canShowMarkdownView && Boolean(gitSectionId && documentContent?.isCompleteDocument);
  const [viewMode, setViewMode] = useState<DiffViewMode>(() =>
    defaultDiffViewMode(
      buildDiffPreviewModel(diff, changeType).hasStructuredPreview,
      Boolean(filePath),
      preferMarkdownView,
    ),
  );

  useEffect(() => {
    setViewMode(defaultDiffViewMode(preview.hasStructuredPreview, Boolean(filePath), preferMarkdownView));
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
    setMarkdownEditContentState(null);
    clearRenderedMarkdownEditHistory();
    bumpRenderedMarkdownEditRevision();
  }, [diffMessageId, filePath, preferMarkdownView, preview.hasStructuredPreview]);

  useEffect(() => {
    let cancelled = false;

    if (!filePath) {
      setLatestFileState(createInitialLatestFileState(null));
      setVisualBaseContent(null);
      setExternalFileNotice(null);
      setDiffEditConflictOnDisk(false);
      return;
    }

    if (!hasScope) {
      setVisualBaseContent(null);
      setExternalFileNotice(null);
      setDiffEditConflictOnDisk(false);
      setLatestFileState({
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
    setLatestFileState({
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
        setLatestFileState(toLatestFileState(response));
      })
      .catch((error) => {
        if (cancelled) {
          return;
        }

        setLatestFileState({
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
      setEditValueState(pendingEditValue ?? latestFile.content);
      setSaveError(null);
      setEditEditorStatus(createEditorStatusSnapshot(pendingEditValue ?? latestFile.content));
      return;
    }

    if (latestFile.status !== "loading") {
      pendingEditValueRef.current = null;
      setEditValueState("");
      setEditEditorStatus(DEFAULT_EDITOR_STATUS);
    }
  }, [latestFile.content, latestFile.path, latestFile.status]);

  const visualLanguage = formatLanguageLabel(language, filePath);
  const editLanguage = latestFile.status === "ready"
    ? formatLanguageLabel(latestFile.language ?? language ?? null, latestFile.path)
    : formatLanguageLabel(language, filePath);
  const canEditVisualDiff =
    !documentContent && preview.hasStructuredPreview && latestFile.status === "ready" && Boolean(filePath);
  const renderedMarkdownEditBlockedReason = documentContent?.editBlockedReason ?? null;
  const canEditRenderedMarkdown =
    Boolean(filePath) &&
    latestFile.status === "ready" &&
    markdownDisplayPreview?.after.completeness === "full" &&
    (documentContent?.canEdit ?? true);
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

    const change = workspaceFilesChangedEventChangeForPath(
      workspaceFilesChangedEvent,
      filePath,
      {
        rootPath: workspaceRoot,
        sessionId: normalizedSessionId || null,
      },
    );
    if (!change) {
      return;
    }

    const currentFile = latestFileRef.current;
    const currentEditValue = editValueRef.current;
    const wasDirty =
      currentFile.status === "ready" && currentEditValue !== currentFile.content;
    if (change.kind === "deleted") {
      if (wasDirty) {
        setExternalFileNotice("The file was deleted on disk. Your diff edit buffer is preserved.");
        setDiffEditConflictOnDisk(true);
        return;
      }

      setVisualBaseContent(null);
      setDiffEditConflictOnDisk(false);
      setLatestFileState({
        status: "error",
        path: filePath,
        content: "",
        error: "File was deleted on disk.",
        language: language ?? null,
      });
      setEditValueState("");
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

        const latestSnapshot = latestFileRef.current;
        const latestEditValue = editValueRef.current;
        const isCurrentlyDirty =
          latestSnapshot.status === "ready" &&
          latestSnapshot.path === filePath &&
          latestEditValue !== latestSnapshot.content;
        if (isCurrentlyDirty) {
          const rebaseResult = rebaseContentOntoDisk(
            latestSnapshot.content,
            latestEditValue,
            response.content,
          );
          if (rebaseResult.status === "conflict") {
            setExternalFileNotice(rebaseResult.reason);
            setDiffEditConflictOnDisk(true);
            return;
          }

          pendingEditValueRef.current = rebaseResult.content;
          setVisualBaseContent(response.content);
          setLatestFileState(toLatestFileState(response));
          setEditValueState(rebaseResult.content);
          setExternalFileNotice("File changed on disk; your diff edits were applied on top.");
          setDiffEditConflictOnDisk(false);
          return;
        }

        setVisualBaseContent(response.content);
        setLatestFileState(toLatestFileState(response));
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
    workspaceRoot,
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
    const currentFile = latestFileRef.current;
    const currentEditValue = editValueRef.current;
    const isCurrentDirty =
      currentFile.status === "ready" && currentEditValue !== currentFile.content;
    if (currentFile.status !== "ready" || !isCurrentDirty || isSaving) {
      return;
    }

    setIsSaving(true);
    setSaveError(null);
    try {
      const response = await onSaveFile(currentFile.path, currentEditValue, {
        baseHash: currentFile.contentHash ?? null,
        overwrite: options?.overwrite,
      });
      const savedContent = response?.content ?? currentEditValue;
      if (response) {
        setVisualBaseContent(response.content);
        setLatestFileState(toLatestFileState(response));
      } else {
        setLatestFileState((current) => {
          if (current.status !== "ready") {
            return current;
          }

          return {
            ...current,
            content: currentEditValue,
            contentHash: null,
          };
        });
      }
      if (markdownEditContent !== null || viewMode === "markdown") {
        setMarkdownEditContentState(savedContent);
        clearRenderedMarkdownEditHistory();
        bumpRenderedMarkdownEditRevision();
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

  function handleRenderedMarkdownSectionChange(
    segment: MarkdownDiffDocumentSegment,
    nextMarkdown: string,
  ) {
    if (!canEditRenderedMarkdown || !markdownPreview || latestFileRef.current.status !== "ready") {
      return;
    }

    const displayContent = markdownEditContentRef.current ?? markdownPreview.after.content;
    const nextDocumentContent = replaceMarkdownDocumentRange(
      displayContent,
      segment.afterStartOffset,
      segment.afterEndOffset,
      normalizeEditedMarkdownSection(nextMarkdown, segment.markdown),
    );
    if (nextDocumentContent === displayContent) {
      return;
    }

    pushRenderedMarkdownUndoSnapshot(displayContent);
    markdownRedoStackRef.current = [];
    setMarkdownEditContentState(nextDocumentContent);
    setEditValueState(nextDocumentContent);
    setSaveError(null);
    setDiffEditConflictOnDisk(false);
    if (markdownPreview.after.source !== "worktree") {
      setExternalFileNotice("Rendered Markdown edits will save this document to the worktree file.");
    } else {
      setExternalFileNotice(null);
    }
  }

  function pushRenderedMarkdownUndoSnapshot(content: string) {
    const stack = markdownUndoStackRef.current;
    if (stack[stack.length - 1] === content) {
      return;
    }

    stack.push(content);
    if (stack.length > MAX_RENDERED_MARKDOWN_UNDO_DEPTH) {
      stack.splice(0, stack.length - MAX_RENDERED_MARKDOWN_UNDO_DEPTH);
    }
  }

  function applyRenderedMarkdownContentFromHistory(content: string) {
    setMarkdownEditContentState(content);
    bumpRenderedMarkdownEditRevision();
    setEditValueState(content);
    setSaveError(null);
    setDiffEditConflictOnDisk(false);
    if (markdownPreview?.after.source && markdownPreview.after.source !== "worktree") {
      setExternalFileNotice("Rendered Markdown edits will save this document to the worktree file.");
    } else {
      setExternalFileNotice(null);
    }
  }

  function handleRenderedMarkdownUndo() {
    if (!canEditRenderedMarkdown || !markdownPreview || latestFileRef.current.status !== "ready") {
      return false;
    }

    const previousContent = markdownUndoStackRef.current.pop();
    if (previousContent == null) {
      return false;
    }

    const currentContent = markdownEditContentRef.current ?? markdownPreview.after.content;
    markdownRedoStackRef.current.push(currentContent);
    applyRenderedMarkdownContentFromHistory(previousContent);
    return true;
  }

  function handleRenderedMarkdownRedo() {
    if (!canEditRenderedMarkdown || !markdownPreview || latestFileRef.current.status !== "ready") {
      return false;
    }

    const nextContent = markdownRedoStackRef.current.pop();
    if (nextContent == null) {
      return false;
    }

    const currentContent = markdownEditContentRef.current ?? markdownPreview.after.content;
    pushRenderedMarkdownUndoSnapshot(currentContent);
    applyRenderedMarkdownContentFromHistory(nextContent);
    return true;
  }

  async function handleApplyDiffEditsToDiskVersion() {
    const currentFile = latestFileRef.current;
    const currentEditValue = editValueRef.current;
    if (
      currentFile.status !== "ready" ||
      currentEditValue === currentFile.content ||
      !hasScope ||
      isRebasingFile
    ) {
      return;
    }

    setIsRebasingFile(true);
    setSaveError(null);
    try {
      const response = await fetchFile(currentFile.path, {
        sessionId: normalizedSessionId || null,
        projectId: normalizedProjectId || null,
      });
      const latestSnapshot = latestFileRef.current;
      const latestEditValue = editValueRef.current;
      if (
        latestSnapshot.status !== "ready" ||
        latestSnapshot.path !== currentFile.path
      ) {
        return;
      }

      const rebaseResult = rebaseContentOntoDisk(
        latestSnapshot.content,
        latestEditValue,
        response.content,
      );
      if (rebaseResult.status === "conflict") {
        setExternalFileNotice(rebaseResult.reason);
        setDiffEditConflictOnDisk(true);
        return;
      }

      pendingEditValueRef.current = rebaseResult.content;
      setVisualBaseContent(response.content);
      setLatestFileState(toLatestFileState(response));
      setEditValueState(rebaseResult.content);
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
      setLatestFileState(toLatestFileState(response));
      setEditValueState(response.content);
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

  function handleOpenMarkdownSourceLink(target: MarkdownFileLinkTarget) {
    onOpenPath(target.path);
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
            {canShowMarkdownView ? (
              <DiffPreviewToggleButton
                selected={viewMode === "markdown"}
                label="Rendered Markdown"
                onClick={() => setViewMode("markdown")}
              >
                <MarkdownModeIcon />
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
              onChange: setEditValueState,
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
                  onChange={canEditVisualDiff ? setEditValueState : undefined}
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

        {viewMode === "markdown" && markdownDisplayPreview ? (
          <MarkdownDiffView
            canEdit={canEditRenderedMarkdown}
            documentPath={filePath}
            editBlockedReason={renderedMarkdownEditBlockedReason}
            gitSectionId={gitSectionId}
            isDirty={isDirty}
            isSaving={isSaving}
            markdownPreview={markdownDisplayPreview}
            markdownEditRevision={markdownEditRevision}
            onOpenSourceLink={handleOpenMarkdownSourceLink}
            onRedo={handleRenderedMarkdownRedo}
            onRenderedMarkdownSectionChange={handleRenderedMarkdownSectionChange}
            onSave={() => void handleSave()}
            onUndo={handleRenderedMarkdownUndo}
            preview={preview}
            saveStateLabel={saveStateLabel}
            workspaceRoot={workspaceRoot}
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

        {viewMode !== "edit" && viewMode !== "markdown" && (preview.note || (reviewThreads.length > 0 && viewMode !== "changes")) ? (
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

function MarkdownModeIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M2.5 2.25h11A1.5 1.5 0 0 1 15 3.75v8.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 1 12.25v-8.5a1.5 1.5 0 0 1 1.5-1.5Zm0 1.5v8.5h11v-8.5Zm1.15 6.8V5.45h1.52l1.18 1.72 1.18-1.72h1.52v5.1H7.72V7.42L6.35 9.35 4.98 7.42v3.13Zm7.65 0L9.35 8.6h1.2V5.45h1.5V8.6h1.2Z"
        fill="currentColor"
      />
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

type MarkdownDiffPreviewSide = GitDiffDocumentSide & {
  completeness: MarkdownDocumentCompleteness;
  note: string | null;
};

type MarkdownDiffPreviewModel = {
  after: MarkdownDiffPreviewSide;
  before: MarkdownDiffPreviewSide;
};

function buildMarkdownDiffPreview(
  documentContent: GitDiffDocumentContent | null | undefined,
  preview: ReturnType<typeof buildDiffPreviewModel>,
  changeType: DiffMessage["changeType"],
): MarkdownDiffPreviewModel | null {
  if (documentContent) {
    const completeness: MarkdownDocumentCompleteness = documentContent.isCompleteDocument ? "full" : "patch";
    return {
      before: {
        ...documentContent.before,
        completeness,
        note: documentContent.note ?? null,
      },
      after: {
        ...documentContent.after,
        completeness,
        note: documentContent.note ?? null,
      },
    };
  }

  if (!preview.hasStructuredPreview) {
    return null;
  }

  const completeness: MarkdownDocumentCompleteness = preview.note ? "patch" : "full";
  return {
    before: {
      content: changeType === "create" ? "" : preview.originalText,
      source: "patch",
      completeness,
      note: preview.note ?? null,
    },
    after: {
      content: preview.modifiedText,
      source: "patch",
      completeness,
      note: preview.note ?? null,
    },
  };
}

function MarkdownDiffView({
  canEdit,
  documentPath,
  editBlockedReason,
  gitSectionId,
  isDirty,
  isSaving,
  markdownEditRevision,
  markdownPreview,
  onRenderedMarkdownSectionChange,
  onOpenSourceLink,
  onRedo,
  onSave,
  onUndo,
  preview,
  saveStateLabel,
  workspaceRoot,
}: {
  canEdit: boolean;
  documentPath: string | null;
  editBlockedReason: string | null;
  gitSectionId: GitDiffSection | null;
  isDirty: boolean;
  isSaving: boolean;
  markdownEditRevision: number;
  markdownPreview: MarkdownDiffPreviewModel;
  onRenderedMarkdownSectionChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onRedo: () => boolean;
  onSave: () => void;
  onUndo: () => boolean;
  preview: ReturnType<typeof buildDiffPreviewModel>;
  saveStateLabel: string | null;
  workspaceRoot: string | null;
}) {
  const segments = useMemo(
    () => buildMarkdownDiffDocumentSegments(markdownPreview, preview),
    [markdownPreview, preview],
  );
  const gitSectionLabel =
    gitSectionId === "staged" ? "Staged" : gitSectionId === "unstaged" ? "Unstaged" : null;

  return (
    <div className="source-editor-shell source-editor-shell-with-statusbar markdown-diff-shell">
      <div className="markdown-diff-toolbar">
        <div className="source-editor-status">
          <span className="chip">Rendered Markdown</span>
          {gitSectionLabel ? <span className="chip">{gitSectionLabel}</span> : null}
          <span className="chip">One document</span>
          {canEdit ? <span className="chip">Editable</span> : null}
          <span className="chip">{formatMarkdownSideSource(markdownPreview.after.source)}</span>
        </div>
        {canEdit ? (
          <div className="source-editor-actions markdown-diff-edit-actions">
            {saveStateLabel ? <span className="support-copy markdown-diff-save-state">{saveStateLabel}</span> : null}
            <button className="ghost-button" type="button" disabled={!isDirty || isSaving} onClick={onSave}>
              {isSaving ? "Saving..." : isDirty ? "Save Markdown" : "Saved"}
            </button>
          </div>
        ) : null}
      </div>
      {!canEdit && editBlockedReason ? (
        <p className="support-copy markdown-document-note">{editBlockedReason}</p>
      ) : null}
      <MarkdownDiffDocument
        canEdit={canEdit}
        completeness={markdownPreview.after.completeness}
        documentPath={documentPath}
        editorRevision={markdownEditRevision}
        note={markdownPreview.after.note}
        onRenderedMarkdownSectionChange={onRenderedMarkdownSectionChange}
        onOpenSourceLink={onOpenSourceLink}
        onRedo={onRedo}
        onSave={onSave}
        onUndo={onUndo}
        segments={segments}
        workspaceRoot={workspaceRoot}
      />
      <footer className="source-editor-statusbar diff-preview-statusbar" aria-label="Markdown diff status">
        <div className="source-editor-statusbar-group">
          <span className="source-editor-statusbar-item source-editor-statusbar-state">
            {canEdit
              ? "Rendered Markdown edits update the file buffer"
              : editBlockedReason ?? "Rendered changes"}
          </span>
        </div>
        <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
          <span className="source-editor-statusbar-item">{`${segments.length} section${segments.length === 1 ? "" : "s"}`}</span>
          <span className="source-editor-statusbar-item">
            {markdownPreview.after.completeness === "full" ? "Full document" : "Patch preview"}
          </span>
        </div>
      </footer>
    </div>
  );
}

type MarkdownDiffDocumentSegment = {
  afterEndOffset: number;
  afterStartOffset: number;
  id: string;
  isInAfterDocument: boolean;
  kind: "added" | "normal" | "removed";
  markdown: string;
  newStart: number | null;
  oldStart: number | null;
};

function MarkdownDiffDocument({
  canEdit,
  completeness,
  documentPath,
  editorRevision,
  note,
  onRenderedMarkdownSectionChange,
  onOpenSourceLink,
  onRedo,
  onSave,
  onUndo,
  segments,
  workspaceRoot,
}: {
  canEdit: boolean;
  completeness: MarkdownDocumentCompleteness;
  documentPath: string | null;
  editorRevision: number;
  note: string | null;
  onRenderedMarkdownSectionChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onRedo: () => boolean;
  onSave: () => void;
  onUndo: () => boolean;
  segments: MarkdownDiffDocumentSegment[];
  workspaceRoot: string | null;
}) {
  const visibleNote =
    note ??
    (completeness === "patch"
      ? "Rendered from patch context only. Unchanged document sections outside the diff are omitted."
      : null);

  return (
    <div className="markdown-diff-change-view">
      <div className="markdown-document-header">
        <span className="chip">Rendered document</span>
        <span className="chip">{completeness === "full" ? "Full document" : "Patch preview"}</span>
      </div>
      {visibleNote ? <p className="support-copy markdown-document-note">{visibleNote}</p> : null}
      <div className="markdown-diff-change-scroll">
        {segments.length === 0 ? (
          <p className="support-copy markdown-document-empty">No rendered Markdown changes were found.</p>
        ) : (
          segments.map((segment) =>
            segment.kind === "normal" ? (
              <EditableRenderedMarkdownSection
                canEdit={canEdit && segment.isInAfterDocument}
                className="markdown-diff-normal-section"
                documentPath={documentPath}
                key={`${segment.id}:${editorRevision}`}
                onChange={(nextMarkdown) => onRenderedMarkdownSectionChange(segment, nextMarkdown)}
                onOpenSourceLink={onOpenSourceLink}
                onRedo={onRedo}
                onSave={onSave}
                onUndo={onUndo}
                segment={segment}
                workspaceRoot={workspaceRoot}
              />
            ) : (
              <section className="markdown-diff-change-block" key={`${segment.id}:${editorRevision}`}>
                <RenderedMarkdownChangeSection
                  canEdit={canEdit && segment.kind === "added" && segment.isInAfterDocument}
                  documentPath={documentPath}
                  onChange={(nextMarkdown) => onRenderedMarkdownSectionChange(segment, nextMarkdown)}
                  onOpenSourceLink={onOpenSourceLink}
                  onRedo={onRedo}
                  onSave={onSave}
                  onUndo={onUndo}
                  segment={segment}
                  tone={segment.kind}
                  workspaceRoot={workspaceRoot}
                />
              </section>
            ),
          )
        )}
      </div>
    </div>
  );
}

function RenderedMarkdownChangeSection({
  canEdit,
  documentPath,
  onChange,
  onOpenSourceLink,
  onRedo,
  onSave,
  onUndo,
  segment,
  tone,
  workspaceRoot,
}: {
  canEdit: boolean;
  documentPath: string | null;
  onChange: (nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onRedo: () => boolean;
  onSave: () => void;
  onUndo: () => boolean;
  segment: MarkdownDiffDocumentSegment;
  tone: "added" | "removed";
  workspaceRoot: string | null;
}) {
  return (
    <section className={`markdown-diff-rendered-section markdown-diff-rendered-section-${tone}`}>
      <EditableRenderedMarkdownSection
        canEdit={canEdit}
        className="markdown-diff-rendered-section-body"
        documentPath={documentPath}
        onChange={onChange}
        onOpenSourceLink={onOpenSourceLink}
        onRedo={onRedo}
        onSave={onSave}
        onUndo={onUndo}
        segment={segment}
        workspaceRoot={workspaceRoot}
      />
    </section>
  );
}

function EditableRenderedMarkdownSection({
  canEdit,
  className,
  documentPath,
  onChange,
  onOpenSourceLink,
  onRedo,
  onSave,
  onUndo,
  segment,
  workspaceRoot,
}: {
  canEdit: boolean;
  className: string;
  documentPath: string | null;
  onChange: (nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onRedo: () => boolean;
  onSave: () => void;
  onUndo: () => boolean;
  segment: MarkdownDiffDocumentSegment;
  workspaceRoot: string | null;
}) {
  const classNames = `${className}${canEdit ? " markdown-diff-editable-section" : ""}`;

  function handleInput(event: FormEvent<HTMLElement>) {
    if (!canEdit) {
      return;
    }

    onChange(serializeEditableMarkdownSection(event.currentTarget));
  }

  function handleKeyDown(event: KeyboardEvent<HTMLElement>) {
    if (!canEdit) {
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      onSave();
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z") {
      event.preventDefault();
      if (event.shiftKey) {
        onRedo();
      } else {
        onUndo();
      }
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "y") {
      event.preventDefault();
      onRedo();
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return;
    }

    if (
      event.key === "ArrowDown" &&
      isSelectionAtEditableSectionVisualBoundary(event.currentTarget, "end")
    ) {
      if (focusAdjacentEditableMarkdownSection(event.currentTarget, 1)) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowRight" &&
      isSelectionAtEditableSectionBoundary(event.currentTarget, "end")
    ) {
      if (focusAdjacentEditableMarkdownSection(event.currentTarget, 1)) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowUp" &&
      isSelectionAtEditableSectionVisualBoundary(event.currentTarget, "start")
    ) {
      if (focusAdjacentEditableMarkdownSection(event.currentTarget, -1)) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowLeft" &&
      isSelectionAtEditableSectionBoundary(event.currentTarget, "start")
    ) {
      if (focusAdjacentEditableMarkdownSection(event.currentTarget, -1)) {
        event.preventDefault();
      }
    }
  }

  return (
    <section
      className={classNames}
      contentEditable={canEdit}
      data-markdown-editable={canEdit ? "true" : undefined}
      onInput={handleInput}
      onKeyDown={handleKeyDown}
      suppressContentEditableWarning
      tabIndex={canEdit ? 0 : undefined}
    >
      <MarkdownContent
        documentPath={documentPath}
        markdown={segment.markdown}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot={workspaceRoot}
      />
    </section>
  );
}

function buildMarkdownDiffDocumentSegments(
  markdownPreview: MarkdownDiffPreviewModel,
  preview: ReturnType<typeof buildDiffPreviewModel>,
): MarkdownDiffDocumentSegment[] {
  if (markdownPreview.after.completeness === "full") {
    return buildFullMarkdownDiffDocumentSegments(
      markdownPreview.before.content,
      markdownPreview.after.content,
    );
  }

  return buildPatchMarkdownDiffDocumentSegments(preview);
}

function buildFullMarkdownDiffDocumentSegments(
  beforeContent: string,
  afterContent: string,
): MarkdownDiffDocumentSegment[] {
  const beforeLines = splitMarkdownDocumentLinesWithOffsets(beforeContent);
  const afterLines = splitMarkdownDocumentLinesWithOffsets(afterContent);
  const anchors = buildMarkdownLineDiffAnchors(beforeLines, afterLines);
  const segments: MarkdownDiffDocumentSegment[] = [];
  let beforeCursor = 0;
  let afterCursor = 0;

  const pushChangedRange = (beforeEnd: number, afterEnd: number) => {
    if (beforeCursor < beforeEnd) {
      const insertionOffset = afterLines[afterCursor]?.start ?? afterContent.length;
      pushMarkdownDiffSegment(segments, {
        afterEndOffset: insertionOffset,
        afterStartOffset: insertionOffset,
        id: `removed:${segments.length}:${beforeCursor}:${beforeEnd}:${afterCursor}`,
        isInAfterDocument: false,
        kind: "removed",
        markdown: beforeLines.slice(beforeCursor, beforeEnd).map((line) => line.text).join(""),
        newStart: afterCursor + 1,
        oldStart: beforeCursor + 1,
      });
    }

    if (afterCursor < afterEnd) {
      const startOffset = afterLines[afterCursor]?.start ?? afterContent.length;
      const endOffset = afterLines[afterEnd - 1]?.end ?? startOffset;
      pushMarkdownDiffSegment(segments, {
        afterEndOffset: endOffset,
        afterStartOffset: startOffset,
        id: `added:${segments.length}:${beforeCursor}:${afterCursor}:${afterEnd}`,
        isInAfterDocument: true,
        kind: "added",
        markdown: afterLines.slice(afterCursor, afterEnd).map((line) => line.text).join(""),
        newStart: afterCursor + 1,
        oldStart: beforeCursor + 1,
      });
    }
  };

  for (const anchor of anchors) {
    pushChangedRange(anchor.beforeIndex, anchor.afterIndex);

    const line = afterLines[anchor.afterIndex];
    if (line) {
      pushMarkdownDiffSegment(segments, {
        afterEndOffset: line.end,
        afterStartOffset: line.start,
        id: `normal:${segments.length}:${anchor.beforeIndex}:${anchor.afterIndex}`,
        isInAfterDocument: true,
        kind: "normal",
        markdown: line.text,
        newStart: anchor.afterIndex + 1,
        oldStart: anchor.beforeIndex + 1,
      });
    }

    beforeCursor = anchor.beforeIndex + 1;
    afterCursor = anchor.afterIndex + 1;
  }

  pushChangedRange(beforeLines.length, afterLines.length);

  return segments;
}

function buildPatchMarkdownDiffDocumentSegments(
  preview: ReturnType<typeof buildDiffPreviewModel>,
): MarkdownDiffDocumentSegment[] {
  const segments: MarkdownDiffDocumentSegment[] = [];

  for (const hunk of preview.hunks) {
    let removedLines: string[] = [];
    let addedLines: string[] = [];
    let oldStart: number | null = null;
    let newStart: number | null = null;

    const flush = () => {
      if (removedLines.length > 0) {
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `removed:${segments.length}:${oldStart ?? "none"}:${newStart ?? "none"}`,
          isInAfterDocument: false,
          kind: "removed",
          markdown: joinMarkdownDiffLines(removedLines),
          newStart,
          oldStart,
        });
      }

      if (addedLines.length > 0) {
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `added:${segments.length}:${oldStart ?? "none"}:${newStart ?? "none"}`,
          isInAfterDocument: true,
          kind: "added",
          markdown: joinMarkdownDiffLines(addedLines),
          newStart,
          oldStart,
        });
      }

      removedLines = [];
      addedLines = [];
      oldStart = null;
      newStart = null;
    };

    for (const row of hunk.rows) {
      if (row.kind === "context") {
        flush();
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `normal:${segments.length}:${row.right.lineNumber ?? "none"}`,
          isInAfterDocument: true,
          kind: "normal",
          markdown: joinMarkdownDiffLines([row.right.text]),
          newStart: row.right.lineNumber,
          oldStart: row.left.lineNumber,
        });
        continue;
      }

      if (row.kind === "omitted") {
        flush();
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `normal:${segments.length}:omitted`,
          isInAfterDocument: false,
          kind: "normal",
          markdown: "...\n",
          newStart: null,
          oldStart: null,
        });
        continue;
      }

      if (row.kind === "removed" || row.kind === "changed") {
        if (oldStart == null) {
          oldStart = row.left.lineNumber;
        }
        removedLines.push(row.left.text);
      }

      if (row.kind === "added" || row.kind === "changed") {
        if (newStart == null) {
          newStart = row.right.lineNumber;
        }
        addedLines.push(row.right.text);
      }
    }

    flush();
  }

  return segments;
}

function pushMarkdownDiffSegment(
  segments: MarkdownDiffDocumentSegment[],
  segment: MarkdownDiffDocumentSegment,
) {
  if (segment.markdown.trim().length === 0 && segment.kind !== "normal") {
    return;
  }

  const previous = segments[segments.length - 1];
  if (previous && previous.kind === segment.kind && previous.isInAfterDocument === segment.isInAfterDocument) {
    previous.markdown += segment.markdown;
    if (previous.isInAfterDocument) {
      previous.afterEndOffset = segment.afterEndOffset;
    }
    return;
  }

  segments.push(segment);
}

function serializeEditableMarkdownSection(section: HTMLElement) {
  const markdownRoot = section.querySelector<HTMLElement>(".markdown-copy") ?? section;
  const blocks = Array.from(markdownRoot.childNodes)
    .map((node) => serializeMarkdownBlockNode(node))
    .filter((markdown) => markdown.trim().length > 0);

  return blocks.join("\n\n");
}

function serializeMarkdownBlockNode(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) {
    return node.textContent?.trim() ?? "";
  }

  if (!(node instanceof HTMLElement) || shouldSkipMarkdownEditableNode(node)) {
    return "";
  }

  const tagName = node.tagName.toLowerCase();
  if (/^h[1-6]$/.test(tagName)) {
    const level = Number(tagName.slice(1));
    return `${"#".repeat(level)} ${serializeMarkdownInlineChildren(node).trim()}`;
  }
  if (tagName === "p") {
    return serializeMarkdownInlineChildren(node).trim();
  }
  if (tagName === "ul" || tagName === "ol") {
    return serializeMarkdownList(node, tagName === "ol");
  }
  if (tagName === "blockquote") {
    return serializeMarkdownBlockChildren(node)
      .split("\n")
      .map((line) => (line.length > 0 ? `> ${line}` : ">"))
      .join("\n");
  }
  if (tagName === "pre") {
    const codeElement = node.querySelector("code");
    const language = codeElement?.className.match(/language-([\w-]+)/)?.[1] ?? "";
    const code = codeElement?.textContent ?? node.textContent ?? "";
    return `\`\`\`${language}\n${code.replace(/\n$/, "")}\n\`\`\``;
  }
  if (tagName === "table") {
    return serializeMarkdownTable(node);
  }
  if (tagName === "div" && node.classList.contains("markdown-table-scroll")) {
    const table = node.querySelector("table");
    return table ? serializeMarkdownTable(table) : "";
  }
  if (tagName === "hr") {
    return "---";
  }
  if (tagName === "br") {
    return "\n";
  }

  return serializeMarkdownBlockChildren(node);
}

function serializeMarkdownBlockChildren(element: HTMLElement) {
  return Array.from(element.childNodes)
    .map((node) => serializeMarkdownBlockNode(node))
    .filter((markdown) => markdown.trim().length > 0)
    .join("\n\n");
}

function serializeMarkdownInlineChildren(element: HTMLElement) {
  return Array.from(element.childNodes).map((node) => serializeMarkdownInlineNode(node)).join("");
}

function serializeMarkdownInlineNode(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) {
    return node.textContent ?? "";
  }

  if (!(node instanceof HTMLElement) || shouldSkipMarkdownEditableNode(node)) {
    return "";
  }

  const tagName = node.tagName.toLowerCase();
  if (tagName === "ul" || tagName === "ol") {
    return "";
  }
  if (tagName === "br") {
    return "\n";
  }

  const content = serializeMarkdownInlineChildren(node);
  if (!content) {
    return "";
  }

  if (tagName === "strong" || tagName === "b") {
    return `**${content}**`;
  }
  if (tagName === "em" || tagName === "i") {
    return `*${content}*`;
  }
  if (tagName === "del" || tagName === "s") {
    return `~~${content}~~`;
  }
  if (tagName === "code") {
    return wrapInlineMarkdownCode(content);
  }
  if (tagName === "a") {
    if (node.classList.contains("inline-code-link")) {
      const code = node.querySelector("code")?.textContent ?? content;
      return wrapInlineMarkdownCode(code);
    }

    const href = node.getAttribute("href");
    return href ? `[${content}](${href})` : content;
  }

  return content;
}

function serializeMarkdownList(list: HTMLElement, ordered: boolean) {
  return Array.from(list.children)
    .filter((child): child is HTMLElement => child instanceof HTMLElement && child.tagName.toLowerCase() === "li")
    .map((item, index) => {
      const marker = ordered ? `${index + 1}.` : "-";
      const itemText = serializeMarkdownInlineChildren(item).trim();
      const nestedBlocks = Array.from(item.children)
        .filter((child) => child instanceof HTMLElement && ["ul", "ol"].includes(child.tagName.toLowerCase()))
        .map((child) =>
          serializeMarkdownBlockNode(child).split("\n").map((line) => `  ${line}`).join("\n"),
        )
        .filter((markdown) => markdown.trim().length > 0);
      return [`${marker} ${itemText}`, ...nestedBlocks].join("\n");
    })
    .join("\n");
}

function serializeMarkdownTable(table: HTMLElement) {
  const rows = Array.from(table.querySelectorAll("tr")).map((row) =>
    Array.from(row.children).map((cell) => serializeMarkdownInlineChildren(cell as HTMLElement).trim()),
  );
  if (rows.length === 0) {
    return "";
  }

  const header = rows[0];
  const separator = header.map(() => "---");
  const bodyRows = rows.slice(1);
  return [header, separator, ...bodyRows]
    .map((row) => `| ${row.join(" | ")} |`)
    .join("\n");
}

function wrapInlineMarkdownCode(content: string) {
  const fence = content.includes("`") ? "``" : "`";
  return `${fence}${content}${fence}`;
}

function shouldSkipMarkdownEditableNode(node: HTMLElement) {
  return node.tagName.toLowerCase() === "button" || node.getAttribute("aria-hidden") === "true";
}

function normalizeEditedMarkdownSection(nextMarkdown: string, originalMarkdown: string) {
  let normalized = nextMarkdown.replace(/\u00a0/g, " ");
  if (originalMarkdown.endsWith("\n") && !normalized.endsWith("\n")) {
    normalized += "\n";
  }
  if (!originalMarkdown.endsWith("\n")) {
    normalized = normalized.replace(/\n+$/g, "");
  }
  return normalized;
}

function isSelectionAtEditableSectionBoundary(
  section: HTMLElement,
  boundary: "end" | "start",
) {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return false;
  }

  const range = selection.getRangeAt(0);
  if (!section.contains(range.startContainer)) {
    return false;
  }

  const probe = range.cloneRange();
  probe.selectNodeContents(section);
  if (boundary === "start") {
    probe.setEnd(range.startContainer, range.startOffset);
  } else {
    probe.setStart(range.startContainer, range.startOffset);
  }

  return probe.toString().length === 0;
}

function isSelectionAtEditableSectionVisualBoundary(
  section: HTMLElement,
  boundary: "end" | "start",
) {
  if (isSelectionAtEditableSectionBoundary(section, boundary)) {
    return true;
  }

  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return false;
  }

  const range = selection.getRangeAt(0);
  if (!section.contains(range.startContainer)) {
    return false;
  }

  const caretRect = getSelectionCaretRect(range);
  if (!caretRect) {
    return false;
  }

  const sectionRect = section.getBoundingClientRect();
  const computedStyle = window.getComputedStyle(section);
  const lineHeight = Number.parseFloat(computedStyle.lineHeight);
  const threshold = Number.isFinite(lineHeight) ? lineHeight * 1.6 : 32;

  return boundary === "start"
    ? caretRect.top - sectionRect.top <= threshold
    : sectionRect.bottom - caretRect.bottom <= threshold;
}

function getSelectionCaretRect(range: Range) {
  const rects = range.getClientRects();
  if (rects.length > 0) {
    return rects[rects.length - 1] ?? null;
  }

  const container =
    range.startContainer instanceof HTMLElement
      ? range.startContainer
      : range.startContainer.parentElement;
  return container?.getBoundingClientRect() ?? null;
}

function focusAdjacentEditableMarkdownSection(currentSection: HTMLElement, direction: -1 | 1) {
  const scrollRegion = currentSection.closest(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return false;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-editable='true']"),
  );
  const currentIndex = editableSections.indexOf(currentSection);
  if (currentIndex < 0) {
    return false;
  }

  const nextSection = editableSections[currentIndex + direction];
  if (!nextSection) {
    return false;
  }

  placeCaretInEditableMarkdownSection(nextSection, direction > 0 ? "start" : "end");
  return true;
}

function placeCaretInEditableMarkdownSection(section: HTMLElement, boundary: "end" | "start") {
  section.focus();

  const selection = window.getSelection();
  if (!selection) {
    return;
  }

  const range = document.createRange();
  const textNode =
    boundary === "start"
      ? findEditableMarkdownTextNode(section, "first")
      : findEditableMarkdownTextNode(section, "last");
  if (textNode) {
    range.setStart(textNode, boundary === "start" ? 0 : textNode.textContent?.length ?? 0);
  } else {
    range.selectNodeContents(section);
    range.collapse(boundary === "start");
  }
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
}

function findEditableMarkdownTextNode(root: Node, position: "first" | "last"): Text | null {
  if (root.nodeType === Node.TEXT_NODE) {
    return root as Text;
  }

  const children = Array.from(root.childNodes);
  if (position === "last") {
    children.reverse();
  }

  for (const child of children) {
    const textNode = findEditableMarkdownTextNode(child, position);
    if (textNode) {
      return textNode;
    }
  }

  return null;
}

type MarkdownDocumentLine = {
  compareText: string;
  end: number;
  start: number;
  text: string;
};

function replaceMarkdownDocumentRange(
  content: string,
  startOffset: number,
  endOffset: number,
  replacement: string,
) {
  const start = Math.max(0, Math.min(startOffset, content.length));
  const end = Math.max(start, Math.min(endOffset, content.length));
  return `${content.slice(0, start)}${replacement}${content.slice(end)}`;
}

function splitMarkdownDocumentLinesWithOffsets(content: string): MarkdownDocumentLine[] {
  const matches = content.matchAll(/[^\n]*\n|[^\n]+/g);
  return Array.from(matches, (match) => {
    const start = match.index ?? 0;
    const text = match[0];
    return {
      compareText: normalizeMarkdownLineForDiff(text),
      end: start + text.length,
      start,
      text,
    };
  });
}

function buildMarkdownLineDiffAnchors(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
) {
  const rowCount = beforeLines.length + 1;
  const columnCount = afterLines.length + 1;
  if (rowCount * columnCount > 1_000_000) {
    return buildGreedyMarkdownLineAnchors(beforeLines, afterLines);
  }

  const lengths = new Uint32Array(rowCount * columnCount);
  const offset = (row: number, column: number) => row * columnCount + column;
  for (let row = beforeLines.length - 1; row >= 0; row -= 1) {
    for (let column = afterLines.length - 1; column >= 0; column -= 1) {
      lengths[offset(row, column)] =
        beforeLines[row]?.compareText === afterLines[column]?.compareText
          ? lengths[offset(row + 1, column + 1)] + 1
          : Math.max(lengths[offset(row + 1, column)], lengths[offset(row, column + 1)]);
    }
  }

  const anchors: Array<{ afterIndex: number; beforeIndex: number }> = [];
  let beforeIndex = 0;
  let afterIndex = 0;
  while (beforeIndex < beforeLines.length && afterIndex < afterLines.length) {
    if (beforeLines[beforeIndex]?.compareText === afterLines[afterIndex]?.compareText) {
      anchors.push({ beforeIndex, afterIndex });
      beforeIndex += 1;
      afterIndex += 1;
      continue;
    }

    if (lengths[offset(beforeIndex + 1, afterIndex)] >= lengths[offset(beforeIndex, afterIndex + 1)]) {
      beforeIndex += 1;
    } else {
      afterIndex += 1;
    }
  }

  return anchors;
}

function buildGreedyMarkdownLineAnchors(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
) {
  const anchors: Array<{ afterIndex: number; beforeIndex: number }> = [];
  let beforeCursor = 0;
  for (let afterIndex = 0; afterIndex < afterLines.length; afterIndex += 1) {
    const compareText = afterLines[afterIndex]?.compareText;
    if (compareText == null) {
      continue;
    }

    const beforeIndex = beforeLines.findIndex(
      (line, index) => index >= beforeCursor && line.compareText === compareText,
    );
    if (beforeIndex < 0) {
      continue;
    }

    anchors.push({ beforeIndex, afterIndex });
    beforeCursor = beforeIndex + 1;
  }

  return anchors;
}

function normalizeMarkdownLineForDiff(line: string) {
  const normalizedLineEndings = line.replace(/\r\n/g, "\n").replace(/\r$/g, "");
  const normalizedLinks = normalizedLineEndings
    .replace(/\[`([^`]+)`\]\([^)]+\)/g, "`$1`")
    .replace(/(?<!!)\[([^\]]+)\]\([^)]+\)/g, "$1");
  if (isMarkdownTableSeparatorLine(normalizedLinks)) {
    return normalizedLinks.replace(/:?-{3,}:?/g, "---").replace(/\s+/g, "");
  }
  return normalizedLinks;
}

function isMarkdownTableSeparatorLine(line: string) {
  return /^\s*\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|?\s*$/.test(line);
}

function joinMarkdownDiffLines(lines: string[]) {
  if (lines.length === 0) {
    return "";
  }

  return `${lines.join("\n")}\n`;
}

function formatMarkdownSideSource(source: GitDiffDocumentSide["source"]) {
  switch (source) {
    case "head":
      return "HEAD";
    case "index":
      return "Index";
    case "worktree":
      return "Worktree";
    case "empty":
      return "Empty";
    case "patch":
      return "Patch";
  }
}

function defaultDiffViewMode(
  hasStructuredPreview: boolean,
  hasFilePath: boolean,
  preferMarkdownView = false,
): DiffViewMode {
  if (preferMarkdownView) {
    return "markdown";
  }

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

function isMarkdownDocument(language: string | null | undefined, path: string | null | undefined) {
  return resolveMonacoLanguage(language ?? null, path ?? null) === "markdown";
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
