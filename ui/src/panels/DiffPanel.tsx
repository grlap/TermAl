import { Suspense, lazy, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ClipboardEvent, FormEvent, KeyboardEvent, ReactNode } from "react";
import { flushSync } from "react-dom";
import {
  fetchFile,
  fetchReviewDocument,
  saveReviewDocument,
  type FileResponse,
  type GitDiffDocumentContent,
  type GitDiffSection,
  type OpenPathOptions,
  type ReviewAnchor,
  type ReviewComment,
  type ReviewDocument,
  type ReviewThread,
} from "../api";
import { copyTextToClipboard } from "../clipboard";
import { FileTabIcon } from "../file-tab-icon";
import { MarkdownContent, type MarkdownFileLinkTarget } from "../message-cards";
import type { MonacoCodeEditorHandle, MonacoCodeEditorStatus } from "../MonacoCodeEditor";
import type { MonacoDiffEditorHandle, MonacoDiffEditorStatus } from "../MonacoDiffEditor";
import { buildDiffPreviewModel } from "../diff-preview";
import type { MonacoAppearance } from "../monaco";
import { normalizeDisplayPath, relativizePathToWorkspace } from "../path-display";
import type { DiffMessage, WorkspaceFilesChangedEvent } from "../types";
import { workspaceFilesChangedEventChangeForPath } from "../workspace-file-events";
import {
  buildMarkdownDiffDocumentSegments,
  normalizeEditedMarkdownSection,
  normalizeMarkdownDocumentLineEndings,
  replaceMarkdownDocumentRange,
  type MarkdownDiffDocumentSegment,
  type MarkdownDiffPreviewModel,
  type MarkdownDiffPreviewSideSource,
  type MarkdownDocumentCompleteness,
} from "./markdown-diff-segments";
import {
  detectRenderableRegions,
  type SourceRenderableRegion,
} from "../source-renderers";
import { type SourceSaveOptions } from "./SourcePanel";
import { rebaseContentOntoDisk } from "./content-rebase";
import {
  buildReviewHandoffPrompt,
  createReviewComment,
  ensureReviewDocument,
  ensureReviewFiles,
  type ReviewOriginContext,
} from "./diff-review-document";
import {
  DEFAULT_DIFF_EDITOR_STATUS,
  DEFAULT_EDITOR_STATUS,
  createEditorStatusSnapshot,
  formatChangeNavigationLabel,
  formatIndentationLabel,
  formatLanguageLabel,
  isMarkdownDocument,
} from "./diff-editor-status-labels";
import {
  createInitialLatestFileState,
  isStaleFileSaveError,
  toLatestFileState,
  type LatestFileState,
} from "./diff-latest-file-state";
import {
  getMarkdownCaretNavigationDirection,
  redirectCaretOutOfRemovedMarkdownSection,
} from "./markdown-diff-caret-navigation";
import {
  defaultDiffViewMode,
  formatMarkdownSideSource,
  getErrorMessage,
  getMarkdownDiffSegmentLineNumber,
  type DiffViewMode,
} from "./diff-panel-helpers";
import { RawPatchView } from "./raw-patch-view";
import {
  RenderedDiffView,
  buildMarkdownDiffPreview,
} from "./rendered-diff-view";
import { renderEditFileView } from "./render-edit-file-view";
import { MarkdownDiffView } from "./markdown-diff-view";
import { StructuredDiffView } from "./StructuredDiffView";
import { useStableEvent } from "./use-stable-event";
import {
  findClosestMarkdownRange,
  hasOverlappingMarkdownCommitRanges,
  mapMarkdownRangeAcrossContentChange,
  markdownRangeMatches,
  resolveRenderedMarkdownCommitRange,
  type MarkdownDocumentRange,
  type RenderedMarkdownSectionCommit,
} from "./markdown-commit-ranges";
import { useStableMarkdownDiffDocumentSegments } from "./markdown-diff-segment-stability";
import {
  captureEditableMarkdownFocusSnapshot,
  focusAdjacentEditableMarkdownSection,
  isSelectionAtEditableSectionBoundary,
  moveEditableMarkdownCaretByPage,
  placeCaretInEditableMarkdownSection,
  scheduleEditableMarkdownFocusRestore,
  shouldSkipMarkdownEditableNode,
} from "./editable-markdown-focus";
import {
  insertSanitizedMarkdownPaste,
  serializeEditableMarkdownSection,
} from "./markdown-diff-edit-pipeline";
import {
  AllLinesIcon,
  ChangedOnlyIcon,
  CheckIcon,
  CopyIcon,
  DiffNavArrow,
  EditModeIcon,
  MarkdownModeIcon,
  RawPatchIcon,
} from "./DiffPanelIcons";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);
const MonacoDiffEditor = lazy(() =>
  import("../MonacoDiffEditor").then(({ MonacoDiffEditor }) => ({ default: MonacoDiffEditor })),
);

type DiffViewScrollPositions = Record<DiffViewMode, number>;

type ReviewState = {
  status: "idle" | "loading" | "ready" | "error";
  review: ReviewDocument | null;
  reviewFilePath: string | null;
  error: string | null;
};

type MarkdownDiffSaveHandler = () => Promise<void> | void;

function createInitialDiffViewScrollPositions(): DiffViewScrollPositions {
  return {
    all: 0,
    changes: 0,
    edit: 0,
    markdown: 0,
    rendered: 0,
    raw: 0,
  };
}

export function DiffPanel({
  appearance,
  changeType,
  changeSetId = null,
  fontSizePx,
  diff,
  documentEnrichmentNote = null,
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
  documentEnrichmentNote?: string | null;
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
  onOpenPath: (path: string, options?: OpenPathOptions) => void;
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
  // Tracks which rendered Markdown sections currently hold uncommitted
  // contentEditable drafts. Per-keystroke drafts intentionally stay local to
  // the section DOM so the rendered diff does not remount while the user is
  // typing; this set is the synchronous dirty signal for Save/reload paths.
  const [renderedMarkdownDraftSegmentIds, setRenderedMarkdownDraftSegmentIds] =
    useState<Set<string>>(() => new Set());
  const renderedMarkdownDraftSegmentIdsRef = useRef(renderedMarkdownDraftSegmentIds);
  const hasRenderedDraftActive = renderedMarkdownDraftSegmentIds.size > 0;
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
  const editEditorRef = useRef<MonacoCodeEditorHandle | null>(null);
  const diffEditorRef = useRef<MonacoDiffEditorHandle | null>(null);
  const structuredDiffScrollRef = useRef<HTMLDivElement | null>(null);
  const markdownDiffScrollRef = useRef<HTMLDivElement | null>(null);
  const rawPatchScrollRef = useRef<HTMLDivElement | null>(null);
  const diffViewRestoreTokenRef = useRef(0);
  const diffViewScrollPositionsRef = useRef<DiffViewScrollPositions>(
    createInitialDiffViewScrollPositions(),
  );
  const renderedMarkdownCommittersRef = useRef(
    new Set<() => RenderedMarkdownSectionCommit | null>(),
  );
  const pendingEditValueRef = useRef<string | null>(null);
  const latestFileRef = useRef(latestFile);
  const editValueRef = useRef(editValue);
  const markdownEditContentRef = useRef<string | null>(markdownEditContent);
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
  const setRenderedMarkdownDraftSegmentActive = (segmentId: string, isActive: boolean) => {
    setRenderedMarkdownDraftSegmentIds((current) => {
      if (current.has(segmentId) === isActive) {
        renderedMarkdownDraftSegmentIdsRef.current = current;
        return current;
      }

      const next = new Set(current);
      if (isActive) {
        next.add(segmentId);
      } else {
        next.delete(segmentId);
      }
      renderedMarkdownDraftSegmentIdsRef.current = next;
      return next;
    });
  };
  const clearRenderedMarkdownDraftSegments = () => {
    setRenderedMarkdownDraftSegmentIds((current) => {
      if (current.size === 0) {
        renderedMarkdownDraftSegmentIdsRef.current = current;
        return current;
      }

      const next = new Set<string>();
      renderedMarkdownDraftSegmentIdsRef.current = next;
      return next;
    });
  };
  const registerRenderedMarkdownCommitter = useCallback((committer: () => RenderedMarkdownSectionCommit | null) => {
    renderedMarkdownCommittersRef.current.add(committer);
    return () => {
      renderedMarkdownCommittersRef.current.delete(committer);
    };
  }, []);
  function collectRenderedMarkdownCommits() {
    return Array.from(renderedMarkdownCommittersRef.current)
      .map((committer) => committer())
      .filter((commit): commit is RenderedMarkdownSectionCommit => commit != null);
  }

  function commitRenderedMarkdownDrafts() {
    const commits = collectRenderedMarkdownCommits();
    if (commits.length === 0) {
      clearRenderedMarkdownDraftSegments();
      return;
    }
    if (handleRenderedMarkdownSectionCommits(commits)) {
      clearRenderedMarkdownDraftSegments();
    }
  }

  function commitRenderedMarkdownSectionDraft(commit: RenderedMarkdownSectionCommit) {
    const hasOtherActiveRenderedDraft = Array.from(renderedMarkdownDraftSegmentIdsRef.current)
      .some((segmentId) => segmentId !== commit.segment.id);
    if (hasOtherActiveRenderedDraft) {
      // A section commit can rebuild the rendered diff. Flush the other active
      // DOM drafts first so their source survives any segment remount.
      const commitsBySegmentId = new Map<string, RenderedMarkdownSectionCommit>([
        [commit.segment.id, commit],
      ]);
      for (const activeCommit of collectRenderedMarkdownCommits()) {
        commitsBySegmentId.set(activeCommit.segment.id, activeCommit);
      }
      const commits = Array.from(commitsBySegmentId.values());
      if (commits.length > 0) {
        handleRenderedMarkdownSectionCommits(commits);
        return;
      }
    }

    handleRenderedMarkdownSectionCommits([commit]);
  }

  const stableCommitRenderedMarkdownDrafts = useStableEvent(commitRenderedMarkdownDrafts);
  const stableCommitRenderedMarkdownSectionDraft = useStableEvent(commitRenderedMarkdownSectionDraft);
  const stableHandleRenderedMarkdownSectionDraftChange = useStableEvent(
    handleRenderedMarkdownSectionDraftChange,
  );
  const stableHandleOpenMarkdownSourceLink = useStableEvent(handleOpenMarkdownSourceLink);

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
    () => buildMarkdownDiffPreview(documentContent, documentEnrichmentNote, preview, changeType),
    [changeType, documentContent, documentEnrichmentNote, preview],
  );
  const canShowMarkdownView = isMarkdownTarget && markdownPreview !== null;
  // Phase 4 of `docs/features/source-renderers.md`: for non-Markdown
  // files whose after-side content has at least one renderable
  // region (e.g., a `.mmd` file with a whole-file Mermaid region),
  // surface a read-only "Rendered" view that reuses the registry's
  // detected regions. Edits stay in Monaco Edit mode — this preview
  // only shows rendered output on the diff's current after side
  // (working-tree for unstaged diffs, index for staged diffs) so
  // staged/unstaged side semantics are preserved by construction.
  const renderedDiffAfterContent = useMemo(() => {
    if (isMarkdownTarget) {
      return null;
    }
    if (documentContent?.after?.content) {
      return documentContent.after.content;
    }
    if (latestFile.status === "ready") {
      return latestFile.content;
    }
    return null;
  }, [documentContent?.after?.content, isMarkdownTarget, latestFile]);
  const renderedDiffRegions = useMemo<SourceRenderableRegion[]>(() => {
    if (renderedDiffAfterContent === null || !filePath) {
      return [];
    }
    return detectRenderableRegions({
      path: filePath,
      language: language ?? null,
      content: renderedDiffAfterContent,
      mode: "diff",
    });
  }, [filePath, language, renderedDiffAfterContent]);
  const canShowRenderedView =
    !isMarkdownTarget && renderedDiffRegions.length > 0;
  const markdownDisplayPreview = useMemo<MarkdownDiffPreviewModel | null>(() => {
    if (!markdownPreview) {
      return null;
    }

    const afterContent =
      markdownEditContent ??
      (latestFile.status === "ready" && editValue !== latestFile.content
        ? editValue
        : markdownPreview.after.content);

    return {
      ...markdownPreview,
      after: {
        ...markdownPreview.after,
        content: afterContent,
      },
    };
  }, [editValue, latestFile.content, latestFile.status, markdownEditContent, markdownPreview]);
  const preferMarkdownView =
    canShowMarkdownView && Boolean(gitSectionId && documentContent?.isCompleteDocument);
  const [viewMode, setViewMode] = useState<DiffViewMode>(() =>
    defaultDiffViewMode(
      buildDiffPreviewModel(diff, changeType).hasStructuredPreview,
      Boolean(filePath),
      preferMarkdownView,
    ),
  );

  function readDiffViewScrollTop(mode: DiffViewMode) {
    if (mode === "all") {
      return diffEditorRef.current?.getScrollTop() ?? diffViewScrollPositionsRef.current.all;
    }
    if (mode === "markdown") {
      return markdownDiffScrollRef.current?.scrollTop ?? diffViewScrollPositionsRef.current.markdown;
    }
    if (mode === "changes") {
      return structuredDiffScrollRef.current?.scrollTop ?? diffViewScrollPositionsRef.current.changes;
    }
    if (mode === "raw") {
      return rawPatchScrollRef.current?.scrollTop ?? diffViewScrollPositionsRef.current.raw;
    }
    if (mode === "edit") {
      return editEditorRef.current?.getScrollTop() ?? diffViewScrollPositionsRef.current.edit;
    }
    return diffViewScrollPositionsRef.current[mode] ?? 0;
  }

  function restoreDiffViewScrollTop(mode: DiffViewMode) {
    const scrollTop = diffViewScrollPositionsRef.current[mode] ?? 0;
    if (mode === "all") {
      const editor = diffEditorRef.current;
      if (!editor) {
        return false;
      }
      editor.setScrollTop(scrollTop);
      return true;
    }
    if (mode === "markdown") {
      const scrollRegion = markdownDiffScrollRef.current;
      if (!scrollRegion) {
        return false;
      }
      scrollRegion.scrollTop = scrollTop;
      return true;
    }
    if (mode === "changes") {
      const scrollRegion = structuredDiffScrollRef.current;
      if (!scrollRegion) {
        return false;
      }
      scrollRegion.scrollTop = scrollTop;
      return true;
    }
    if (mode === "raw") {
      const scrollRegion = rawPatchScrollRef.current;
      if (!scrollRegion) {
        return false;
      }
      scrollRegion.scrollTop = scrollTop;
      return true;
    }
    if (mode === "edit") {
      const editor = editEditorRef.current;
      if (!editor) {
        return false;
      }
      editor.setScrollTop(scrollTop);
      return true;
    }
    return true;
  }

  function rememberDiffViewScrollTop(mode: DiffViewMode = viewMode) {
    diffViewScrollPositionsRef.current = {
      ...diffViewScrollPositionsRef.current,
      [mode]: readDiffViewScrollTop(mode),
    };
  }

  function handleSelectViewMode(nextViewMode: DiffViewMode) {
    rememberDiffViewScrollTop();
    setViewMode(nextViewMode);
  }

  // This reset is intentionally tied to tab identity only. Derived preview
  // values can change during git refreshes, but user-selected view mode should
  // stay sticky while the same diff tab remains open.
  useEffect(() => {
    diffViewScrollPositionsRef.current = createInitialDiffViewScrollPositions();
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
    clearRenderedMarkdownDraftSegments();
  }, [diffMessageId, filePath]);

  useEffect(() => {
    const restoreToken = diffViewRestoreTokenRef.current + 1;
    diffViewRestoreTokenRef.current = restoreToken;
    let animationFrameId = 0;
    let attempts = 0;
    let cancelled = false;
    const restore = () => {
      if (cancelled || diffViewRestoreTokenRef.current !== restoreToken) {
        return;
      }
      attempts += 1;
      const restored = restoreDiffViewScrollTop(viewMode);
      if (!restored && attempts < 8) {
        animationFrameId = window.requestAnimationFrame(restore);
      }
    };

    animationFrameId = window.requestAnimationFrame(restore);
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(animationFrameId);
    };
  }, [diffMessageId, filePath, viewMode]);

  useEffect(() => {
    if (renderedMarkdownDraftSegmentIdsRef.current.size > 0) {
      commitRenderedMarkdownDrafts();
    }

    const currentFile = latestFileRef.current;
    if (currentFile.status === "ready" && editValueRef.current !== currentFile.content) {
      return;
    }

    setMarkdownEditContentState(null);
    clearRenderedMarkdownDraftSegments();
  }, [documentContent]);

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
  // Staged Markdown diffs are index snapshots, not the mutable worktree file.
  // Keep them navigable in rendered mode, but route edits through unstaged
  // worktree diffs where saving can write to an actual file buffer.
  const isStagedMarkdownDiff = gitSectionId === "staged" && canShowMarkdownView;
  const canEditVisualDiff =
    preview.hasStructuredPreview && latestFile.status === "ready" && Boolean(filePath) && !isStagedMarkdownDiff;
  const renderedMarkdownEditBlockedReason = isStagedMarkdownDiff
    ? documentContent?.editBlockedReason ?? "Staged Markdown diffs are read-only. Use the unstaged worktree diff to edit this file."
    : documentContent?.editBlockedReason ?? null;
  const canEditRenderedMarkdown =
    Boolean(filePath) &&
    !isStagedMarkdownDiff &&
    latestFile.status === "ready" &&
    markdownDisplayPreview?.after.completeness === "full" &&
    (documentContent?.canEdit ?? true);
  const hasVisualNavigation = viewMode === "all" && visualEditorStatus.changeCount > 0;
  // `isDirty` reflects both committed draft state (editValue ≠ disk content)
  // and any uncommitted rendered-Markdown section draft that has not yet been
  // flushed to editValue.
  const isDirty =
    (latestFile.status === "ready" && editValue !== latestFile.content) || hasRenderedDraftActive;
  const diffPreviewNote = documentEnrichmentNote ?? preview.note ?? null;
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

    if (renderedMarkdownDraftSegmentIdsRef.current.size > 0) {
      commitRenderedMarkdownDrafts();
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

        if (renderedMarkdownDraftSegmentIdsRef.current.size > 0) {
          commitRenderedMarkdownDrafts();
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
    commitRenderedMarkdownDrafts();

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
      commitRenderedMarkdownDrafts();
      const savedContent = response?.content ?? currentEditValue;
      const latestEditValue = editValueRef.current;
      const hasLocalEditsAfterSaveStarted = latestEditValue !== currentEditValue;
      if (hasLocalEditsAfterSaveStarted) {
        pendingEditValueRef.current = latestEditValue;
      }
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
        setMarkdownEditContentState(hasLocalEditsAfterSaveStarted ? latestEditValue : savedContent);
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

  // Rendered Markdown commits are batched so Save/blur/caret navigation can
  // flush every active contentEditable draft against one common baseline.
  // Applying bottom-to-top preserves downstream offsets when an earlier
  // section changes line counts.
  function handleRenderedMarkdownSectionCommits(
    commits: RenderedMarkdownSectionCommit[],
  ) {
    if (!canEditRenderedMarkdown || !markdownPreview || latestFileRef.current.status !== "ready") {
      return false;
    }

    // Normalize to LF so segment offsets (also LF-normalized in
    // `buildFullMarkdownDiffDocumentSegments`) line up with slices of
    // `sourceContent`. Without this, CRLF-on-disk documents (common on
    // Windows with `core.autocrlf=true`) made the resolver's
    // `sourceContent.slice(start, end) === segment.markdown` check fail by
    // every `\r` character, surfacing as an unresolvable commit and the
    // "Rendered Markdown edit could not be applied" error.
    const sourceContent = normalizeMarkdownDocumentLineEndings(
      markdownEditContentRef.current ??
        (latestFileRef.current.status === "ready" &&
        editValueRef.current !== latestFileRef.current.content
          ? editValueRef.current
          : markdownPreview.after.content),
    );
    const resolvedCommits = commits
      .map((commit) => ({
        commit,
        range: resolveRenderedMarkdownCommitRange(sourceContent, commit),
      }));
    const unresolvedCommitCount = resolvedCommits.filter(
      (entry) => entry.range === null,
    ).length;
    const validResolvedCommits = resolvedCommits.filter(
        (entry): entry is {
          commit: RenderedMarkdownSectionCommit;
          range: MarkdownDocumentRange;
        } => entry.range !== null,
      );
    const hasOverlappingRange = hasOverlappingMarkdownCommitRanges(validResolvedCommits);
    if (unresolvedCommitCount > 0 || hasOverlappingRange) {
      setSaveError(
        "Rendered Markdown edit could not be applied because the document changed under that section. Review the latest diff and edit again.",
      );
      return false;
    }

    const nextDocumentContent = validResolvedCommits
      .sort((left, right) => right.range.start - left.range.start)
      .reduce(
        (currentContent, { commit, range }) =>
          replaceMarkdownDocumentRange(
            currentContent,
            range.start,
            range.end,
            normalizeEditedMarkdownSection(commit.nextMarkdown, commit.segment.markdown),
        ),
        sourceContent,
      );
    if (nextDocumentContent === sourceContent) {
      for (const commit of commits) {
        setRenderedMarkdownDraftSegmentActive(commit.segment.id, false);
      }
      return true;
    }

    for (const commit of commits) {
      setRenderedMarkdownDraftSegmentActive(commit.segment.id, false);
    }
    setMarkdownEditContentState(nextDocumentContent);
    setEditValueState(nextDocumentContent);
    setSaveError(null);
    setDiffEditConflictOnDisk(false);
    if (markdownPreview.after.source !== "worktree") {
      setExternalFileNotice("Rendered Markdown edits will save this document to the worktree file.");
    } else {
      setExternalFileNotice(null);
    }
    return true;
  }

  // Rendered Markdown draft handler.
  //
  // Intentionally does NOT call `setEditValueState` on every keystroke. The
  // previous implementation propagated per-keystroke drafts to `editValue`,
  // which triggered `markdownDisplayPreview` to recompute and `segments` to
  // churn. The churn caused two silent data-loss regressions:
  //
  //   1. Mid-edit editor remount: positional segment IDs shifted as the
  //      user typed, so React unmounted the focused editor and remounted
  //      it without focus/cursor/IME state. The freeze-counter workaround
  //      introduced a separate "stale frozen baseline" bug (see below).
  //
  //   2. Stale frozen baseline on multi-section commits or watcher rebases:
  //      a counter-based freeze failed to thaw when transitions batched
  //      across a single React render (counter 1 → 0 → 1), and when a
  //      watcher rebase updated `editValue` mid-edit. Subsequent keystrokes
  //      or commits applied to the original baseline and silently
  //      overwrote the other edits.
  //
  // Keeping the draft local to the section component lets segments stay
  // stable during typing without a freeze, and the commit handler always
  // reads from the live source content.
  function handleRenderedMarkdownSectionDraftChange(
    segment: MarkdownDiffDocumentSegment,
    nextMarkdown: string,
  ) {
    if (!canEditRenderedMarkdown || !markdownPreview || latestFileRef.current.status !== "ready") {
      return;
    }

    const normalizedDraft = normalizeEditedMarkdownSection(nextMarkdown, segment.markdown);
    setRenderedMarkdownDraftSegmentActive(segment.id, normalizedDraft !== segment.markdown);
    setSaveError(null);
    setDiffEditConflictOnDisk(false);
    if (markdownPreview.after.source !== "worktree") {
      setExternalFileNotice("Rendered Markdown edits will save this document to the worktree file.");
    } else {
      setExternalFileNotice(null);
    }
  }

  async function handleApplyDiffEditsToDiskVersion() {
    flushSync(() => commitRenderedMarkdownDrafts());
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
    onOpenPath(target.path, {
      line: target.line,
      column: target.column,
      openInNewTab: target.openInNewTab,
    });
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
                onClick={() => handleSelectViewMode("all")}
              >
                <AllLinesIcon />
              </DiffPreviewToggleButton>
            ) : null}
            {preview.hasStructuredPreview ? (
              <DiffPreviewToggleButton
                selected={viewMode === "changes"}
                label="Changed only"
                onClick={() => handleSelectViewMode("changes")}
              >
                <ChangedOnlyIcon />
              </DiffPreviewToggleButton>
            ) : null}
            {canShowMarkdownView ? (
              <DiffPreviewToggleButton
                selected={viewMode === "markdown"}
                label="Rendered Markdown"
                onClick={() => handleSelectViewMode("markdown")}
              >
                <MarkdownModeIcon />
              </DiffPreviewToggleButton>
            ) : null}
            {canShowRenderedView ? (
              <DiffPreviewToggleButton
                selected={viewMode === "rendered"}
                label="Rendered"
                onClick={() => handleSelectViewMode("rendered")}
              >
                <MarkdownModeIcon />
              </DiffPreviewToggleButton>
            ) : null}
            {filePath ? (
              <DiffPreviewToggleButton
                selected={viewMode === "edit"}
                label="Edit mode"
                onClick={() => handleSelectViewMode("edit")}
              >
                <EditModeIcon />
              </DiffPreviewToggleButton>
            ) : null}
            <DiffPreviewToggleButton
              selected={viewMode === "raw"}
              label="Raw patch"
              onClick={() => handleSelectViewMode("raw")}
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
        {/*
         * Surface the full save-error message alongside the "Save failed"
         * pill. Previously `saveError` was only used to compute the pill
         * label, so stale-hash conflicts surfaced via `externalFileNotice`
         * but rendered-Markdown-commit failures (thrown from the frontend
         * before any network call) produced "Save failed" with no
         * explanation. Rendering the raw message here keeps both paths
         * diagnosable without changing the stale-disk recovery flow
         * (those cases also set `externalFileNotice` / `diffEditConflictOnDisk`
         * which render their own UI below).
         */}
        {saveError && !externalFileNotice && !diffEditConflictOnDisk ? (
          <p className="support-copy diff-preview-note">{`Save failed: ${saveError}`}</p>
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
              editorRef: editEditorRef,
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
            scrollRef={structuredDiffScrollRef}
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
            appearance={appearance}
            canEdit={canEditRenderedMarkdown}
            documentPath={filePath}
            editBlockedReason={renderedMarkdownEditBlockedReason}
            gitSectionId={gitSectionId}
            isDirty={isDirty}
            isSaving={isSaving}
            markdownPreview={markdownDisplayPreview}
            onCommitRenderedMarkdownDrafts={stableCommitRenderedMarkdownDrafts}
            onCommitRenderedMarkdownSectionDraft={stableCommitRenderedMarkdownSectionDraft}
            onOpenSourceLink={stableHandleOpenMarkdownSourceLink}
            onRegisterRenderedMarkdownCommitter={registerRenderedMarkdownCommitter}
            onRenderedMarkdownSectionDraftChange={stableHandleRenderedMarkdownSectionDraftChange}
            onSave={handleSave}
            preview={preview}
            saveStateLabel={saveStateLabel}
            scrollRef={markdownDiffScrollRef}
            workspaceRoot={workspaceRoot}
          />
        ) : null}

        {viewMode === "rendered" && canShowRenderedView ? (
          <RenderedDiffView
            appearance={appearance}
            documentPath={filePath}
            isCompleteDocument={Boolean(documentContent?.isCompleteDocument)}
            regions={renderedDiffRegions}
            workspaceRoot={workspaceRoot}
          />
        ) : null}

        {viewMode === "raw" || (viewMode === "all" && !preview.hasStructuredPreview) ? (
          <RawPatchView diff={diff} scrollRef={rawPatchScrollRef} />
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

        {viewMode !== "edit" && viewMode !== "markdown" && (diffPreviewNote || (reviewThreads.length > 0 && viewMode !== "changes")) ? (
          <p className="support-copy diff-preview-note">
            {diffPreviewNote ?? ""}
            {diffPreviewNote && reviewThreads.length > 0 && viewMode !== "changes" ? " " : ""}
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
