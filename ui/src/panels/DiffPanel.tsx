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
import { resolveMonacoLanguage, type MonacoAppearance } from "../monaco";
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
import { rebaseContentOntoDisk, type SourceSaveOptions } from "./SourcePanel";
import { StructuredDiffView } from "./StructuredDiffView";

function useStableEvent<TArgs extends unknown[], TResult>(
  callback: (...args: TArgs) => TResult,
) {
  const callbackRef = useRef(callback);
  // Some callers invoke the stable wrapper from flushSync-driven event paths,
  // so publish the latest callback before layout-phase work can run.
  useLayoutEffect(() => {
    callbackRef.current = callback;
  });

  return useCallback((...args: TArgs) => callbackRef.current(...args), []);
}

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);
const MonacoDiffEditor = lazy(() =>
  import("../MonacoDiffEditor").then(({ MonacoDiffEditor }) => ({ default: MonacoDiffEditor })),
);

type DiffViewMode = "all" | "changes" | "markdown" | "edit" | "raw";
type DiffViewScrollPositions = Record<DiffViewMode, number>;

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

type RenderedMarkdownSectionCommit = {
  currentSegment: MarkdownDiffDocumentSegment;
  segment: MarkdownDiffDocumentSegment;
  nextMarkdown: string;
  sourceContent: string;
};

type MarkdownDocumentRange = {
  end: number;
  start: number;
};

type MarkdownDiffSaveHandler = () => Promise<void> | void;

type EditableMarkdownFocusSnapshot = {
  caretTextOffset: number | null;
  scrollRegion: HTMLElement | null;
  scrollTop: number | null;
  section: HTMLElement;
  segmentAfterEndOffset: number | null;
  segmentAfterStartOffset: number | null;
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

function createInitialDiffViewScrollPositions(): DiffViewScrollPositions {
  return {
    all: 0,
    changes: 0,
    edit: 0,
    markdown: 0,
    raw: 0,
  };
}

function resolveRenderedMarkdownCommitRange(
  currentContent: string,
  commit: RenderedMarkdownSectionCommit,
): MarkdownDocumentRange | null {
  const originalRange = {
    start: commit.segment.afterStartOffset,
    end: commit.segment.afterEndOffset,
  };
  if (markdownRangeMatches(currentContent, originalRange, commit.segment.markdown)) {
    return originalRange;
  }

  const mappedRange = mapMarkdownRangeAcrossContentChange(
    commit.sourceContent,
    currentContent,
    originalRange,
  );
  if (mappedRange && markdownRangeMatches(currentContent, mappedRange, commit.segment.markdown)) {
    return mappedRange;
  }

  const searchedRange = findClosestMarkdownRange(
    currentContent,
    commit.segment.markdown,
    mappedRange?.start ?? originalRange.start,
  );
  if (searchedRange) {
    return searchedRange;
  }

  const currentRange = {
    start: commit.currentSegment.afterStartOffset,
    end: commit.currentSegment.afterEndOffset,
  };
  if (markdownRangeMatches(currentContent, currentRange, commit.currentSegment.markdown)) {
    return currentRange;
  }

  return null;
}

function markdownRangeMatches(
  content: string,
  range: MarkdownDocumentRange,
  expected: string,
) {
  return (
    range.start >= 0 &&
    range.end >= range.start &&
    range.end <= content.length &&
    content.slice(range.start, range.end) === expected
  );
}

export function hasOverlappingMarkdownCommitRanges(
  commits: Array<{
    commit: RenderedMarkdownSectionCommit;
    range: MarkdownDocumentRange;
  }>,
) {
  const sortedRanges = [...commits].sort((left, right) => left.range.start - right.range.start);
  for (let index = 1; index < sortedRanges.length; index += 1) {
    const previous = sortedRanges[index - 1];
    const current = sortedRanges[index];
    if (!previous || !current) {
      continue;
    }
    // Strict `<` flags non-adjacent overlap for non-empty ranges. The
    // additional `<=` branch rejects two zero-length ranges that share
    // an insertion point (e.g., both resolve to `[10, 10)`), and
    // rejects a zero-length range that touches a non-empty sibling —
    // in both cases the subsequent descending-by-start splice would
    // apply both writes at the same offset in unspecified order and
    // silently garble the document.
    const isZeroLengthTouch =
      current.range.start === previous.range.end &&
      (current.range.start === current.range.end || previous.range.start === previous.range.end);
    if (current.range.start < previous.range.end || isZeroLengthTouch) {
      return true;
    }
  }

  return false;
}

function mapMarkdownRangeAcrossContentChange(
  sourceContent: string,
  currentContent: string,
  range: MarkdownDocumentRange,
): MarkdownDocumentRange | null {
  if (sourceContent === currentContent) {
    return range;
  }

  let prefixLength = 0;
  const sharedLength = Math.min(sourceContent.length, currentContent.length);
  while (
    prefixLength < sharedLength &&
    sourceContent.charCodeAt(prefixLength) === currentContent.charCodeAt(prefixLength)
  ) {
    prefixLength += 1;
  }

  let sourceSuffixStart = sourceContent.length;
  let currentSuffixStart = currentContent.length;
  while (
    sourceSuffixStart > prefixLength &&
    currentSuffixStart > prefixLength &&
    sourceContent.charCodeAt(sourceSuffixStart - 1) ===
      currentContent.charCodeAt(currentSuffixStart - 1)
  ) {
    sourceSuffixStart -= 1;
    currentSuffixStart -= 1;
  }

  if (range.end <= prefixLength) {
    return range;
  }

  if (range.start >= sourceSuffixStart) {
    const delta = currentSuffixStart - sourceSuffixStart;
    return {
      start: range.start + delta,
      end: range.end + delta,
    };
  }

  return null;
}

function findClosestMarkdownRange(
  content: string,
  markdown: string,
  preferredStart: number,
): MarkdownDocumentRange | null {
  if (markdown.length === 0) {
    return null;
  }

  let bestStart: number | null = null;
  let searchStart = 0;
  while (searchStart <= content.length) {
    const foundStart = content.indexOf(markdown, searchStart);
    if (foundStart === -1) {
      break;
    }

    if (
      bestStart === null ||
      Math.abs(foundStart - preferredStart) < Math.abs(bestStart - preferredStart)
    ) {
      bestStart = foundStart;
    }
    searchStart = foundStart + Math.max(markdown.length, 1);
  }

  return bestStart === null
    ? null
    : {
        start: bestStart,
        end: bestStart + markdown.length,
      };
}

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
  editorRef,
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
  editorRef: { current: MonacoCodeEditorHandle | null };
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
          ref={editorRef}
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

function RawPatchView({
  diff,
  scrollRef,
}: {
  diff: string;
  scrollRef?: { current: HTMLDivElement | null };
}) {
  const lines = diff.split("\n");

  return (
    <div className="diff-editor-shell diff-preview-raw-shell" ref={scrollRef}>
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

function buildMarkdownDiffPreview(
  documentContent: GitDiffDocumentContent | null | undefined,
  documentEnrichmentNote: string | null | undefined,
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

  const completeness: MarkdownDocumentCompleteness = "patch";
  const note = documentEnrichmentNote ?? preview.note ?? null;
  return {
    before: {
      content: changeType === "create" ? "" : preview.originalText,
      source: "patch",
      completeness,
      note,
    },
    after: {
      content: preview.modifiedText,
      source: "patch",
      completeness,
      note,
    },
  };
}

function MarkdownDiffView({
  appearance,
  canEdit,
  documentPath,
  editBlockedReason,
  gitSectionId,
  isDirty,
  isSaving,
  markdownPreview,
  onRenderedMarkdownSectionDraftChange,
  onCommitRenderedMarkdownSectionDraft,
  onOpenSourceLink,
  onCommitRenderedMarkdownDrafts,
  onRegisterRenderedMarkdownCommitter,
  onSave,
  preview,
  saveStateLabel,
  scrollRef,
  workspaceRoot,
}: {
  appearance: MonacoAppearance;
  canEdit: boolean;
  documentPath: string | null;
  editBlockedReason: string | null;
  gitSectionId: GitDiffSection | null;
  isDirty: boolean;
  isSaving: boolean;
  markdownPreview: MarkdownDiffPreviewModel;
  onRenderedMarkdownSectionDraftChange: (
    segment: MarkdownDiffDocumentSegment,
    nextMarkdown: string,
  ) => void;
  onCommitRenderedMarkdownSectionDraft: (commit: RenderedMarkdownSectionCommit) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onCommitRenderedMarkdownDrafts: () => void;
  onRegisterRenderedMarkdownCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onSave: MarkdownDiffSaveHandler;
  preview: ReturnType<typeof buildDiffPreviewModel>;
  saveStateLabel: string | null;
  scrollRef: { current: HTMLDivElement | null };
  workspaceRoot: string | null;
}) {
  const builtSegments = useMemo(
    () => buildMarkdownDiffDocumentSegments(markdownPreview, preview),
    [markdownPreview, preview],
  );
  const stableSegmentIdentityKey = [
    documentPath ?? "",
    gitSectionId ?? "",
    markdownPreview.before.source,
    markdownPreview.after.source,
  ].join("\0");
  const segments = useStableMarkdownDiffDocumentSegments(
    builtSegments,
    stableSegmentIdentityKey,
  );
  // Previous implementations here froze `segments` and the source content
  // they were computed from while any section was editing, in order to keep
  // the focused editor mounted across keystrokes. The freeze had two silent
  // data-loss hazards:
  //   1. Multi-section commit: committing section A then starting section B
  //      batched `count: 1 → 0 → 1` through a single render, so the counter
  //      never hit 0 and the frozen baseline never refreshed. B's edit then
  //      applied to the pre-A-commit content and silently overwrote A's
  //      changes.
  //   2. Watcher rebase: a file-change event rebased `editValue` onto new
  //      disk content, but the frozen baseline stayed stale, so the next
  //      keystroke replayed the section edit against the old content and
  //      dropped the rebased disk changes.
  // Instead, `EditableRenderedMarkdownSection` now holds its draft in the
  // contentEditable DOM and does not propagate drafts to `editValue` per
  // keystroke. Segments therefore do not churn during typing, which keeps the
  // editor DOM identity stable without requiring a freeze at all. The draft handler
  // still emits a signal so `isDirty` flips immediately for the Save button.
  const handleRenderedMarkdownSectionDraftChange = useCallback(
    (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => {
      onRenderedMarkdownSectionDraftChange(
        segment,
        nextMarkdown,
      );
    },
    [onRenderedMarkdownSectionDraftChange],
  );
  const gitSectionLabel =
    gitSectionId === "staged" ? "Staged" : gitSectionId === "unstaged" ? "Unstaged" : null;
  const [readOnlyResetVersion, setReadOnlyResetVersion] = useState(0);

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
            <button className="ghost-button" type="button" disabled={!isDirty || isSaving} onClick={() => void onSave()}>
              {isSaving ? "Saving..." : isDirty ? "Save Markdown" : "Saved"}
            </button>
          </div>
        ) : null}
      </div>
      {!canEdit && editBlockedReason ? (
        <p className="support-copy markdown-document-note">{editBlockedReason}</p>
      ) : null}
      <MarkdownDiffDocument
        allowReadOnlyCaret={!canEdit && gitSectionId === "staged"}
        appearance={appearance}
        canEdit={canEdit}
        completeness={markdownPreview.after.completeness}
        documentPath={documentPath}
        key={readOnlyResetVersion}
        note={markdownPreview.after.note}
        onCommitRenderedMarkdownSectionDraft={onCommitRenderedMarkdownSectionDraft}
        onRenderedMarkdownSectionDraftChange={handleRenderedMarkdownSectionDraftChange}
        onOpenSourceLink={onOpenSourceLink}
        onCommitRenderedMarkdownDrafts={onCommitRenderedMarkdownDrafts}
        onRegisterRenderedMarkdownCommitter={onRegisterRenderedMarkdownCommitter}
        onReadOnlyRenderedMarkdownMutation={() => setReadOnlyResetVersion((current) => current + 1)}
        onSave={onSave}
        scrollRef={scrollRef}
        segments={segments}
        sourceContent={markdownPreview.after.content}
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

function useStableMarkdownDiffDocumentSegments(
  segments: MarkdownDiffDocumentSegment[],
  identityKey: string,
) {
  const previousRef = useRef<{
    identityKey: string;
    segments: MarkdownDiffDocumentSegment[];
  } | null>(null);
  const stableSegments = useMemo(() => {
    const previous =
      previousRef.current?.identityKey === identityKey
        ? previousRef.current.segments
        : [];
    return stabilizeMarkdownDiffSegmentIds(previous, segments);
  }, [identityKey, segments]);

  useEffect(() => {
    previousRef.current = { identityKey, segments: stableSegments };
  }, [identityKey, stableSegments]);

  return stableSegments;
}

type IndexedMarkdownDiffSegment = {
  context: MarkdownDiffSegmentContext;
  identity: string;
  index: number;
  segment: MarkdownDiffDocumentSegment;
};

type MarkdownDiffSegmentContext = {
  next: string | null;
  nextNext: string | null;
  previous: string | null;
  previousPrevious: string | null;
};

function stabilizeMarkdownDiffSegmentIds(
  previousSegments: MarkdownDiffDocumentSegment[],
  nextSegments: MarkdownDiffDocumentSegment[],
) {
  if (previousSegments.length === 0 || nextSegments.length === 0) {
    return nextSegments;
  }

  const previousEntries = indexMarkdownDiffSegments(previousSegments);
  const nextEntries = indexMarkdownDiffSegments(nextSegments);
  const previousByIdentity = new Map<string, IndexedMarkdownDiffSegment[]>();
  for (const entry of previousEntries) {
    const entries = previousByIdentity.get(entry.identity) ?? [];
    entries.push(entry);
    previousByIdentity.set(entry.identity, entries);
  }

  const candidatePairs: Array<{
    contextScore: number;
    distance: number;
    nextIndex: number;
    previousIndex: number;
  }> = [];
  for (const nextEntry of nextEntries) {
    const matchingPreviousEntries = previousByIdentity.get(nextEntry.identity) ?? [];
    for (const previousEntry of matchingPreviousEntries) {
      candidatePairs.push({
        contextScore: scoreMarkdownDiffSegmentContext(
          previousEntry.context,
          nextEntry.context,
        ),
        distance: getMarkdownDiffSegmentDistance(
          previousEntry.segment,
          nextEntry.segment,
          previousEntry.index,
          nextEntry.index,
        ),
        nextIndex: nextEntry.index,
        previousIndex: previousEntry.index,
      });
    }
  }

  candidatePairs.sort(
    (left, right) =>
      right.contextScore - left.contextScore ||
      left.distance - right.distance ||
      left.nextIndex - right.nextIndex ||
      left.previousIndex - right.previousIndex,
  );

  const matchedPreviousIndexes = new Set<number>();
  const previousIdByNextIndex = new Map<number, string>();
  // Reuse ids from the previous committed render before trusting the new
  // occurrence-based ids. This keeps repeated equal Markdown chunks mounted
  // when an earlier duplicate is inserted or removed.
  for (const pair of candidatePairs) {
    if (
      matchedPreviousIndexes.has(pair.previousIndex) ||
      previousIdByNextIndex.has(pair.nextIndex)
    ) {
      continue;
    }

    const previousSegment = previousSegments[pair.previousIndex];
    if (!previousSegment) {
      continue;
    }

    matchedPreviousIndexes.add(pair.previousIndex);
    previousIdByNextIndex.set(pair.nextIndex, previousSegment.id);
  }

  const usedIds = new Set<string>();
  const reservedMatchedIds = new Set(previousIdByNextIndex.values());
  return nextSegments.map((segment, index) => {
    const matchedPreviousId = previousIdByNextIndex.get(index);
    const preferredId =
      matchedPreviousId ??
      (reservedMatchedIds.has(segment.id) ? `${segment.id}:new` : segment.id);
    const id = getAvailableMarkdownDiffSegmentId(preferredId, usedIds);
    usedIds.add(id);
    return id === segment.id ? segment : { ...segment, id };
  });
}

function indexMarkdownDiffSegments(segments: MarkdownDiffDocumentSegment[]) {
  return segments.map((segment, index) => ({
    context: getMarkdownDiffSegmentContext(segments, index),
    identity: getMarkdownDiffSegmentIdentity(segment),
    index,
    segment,
  }));
}

function getMarkdownDiffSegmentContext(
  segments: MarkdownDiffDocumentSegment[],
  index: number,
): MarkdownDiffSegmentContext {
  return {
    next: getMarkdownDiffSegmentIdentityOrNull(segments[index + 1]),
    nextNext: getMarkdownDiffSegmentIdentityOrNull(segments[index + 2]),
    previous: getMarkdownDiffSegmentIdentityOrNull(segments[index - 1]),
    previousPrevious: getMarkdownDiffSegmentIdentityOrNull(segments[index - 2]),
  };
}

function getMarkdownDiffSegmentIdentityOrNull(
  segment: MarkdownDiffDocumentSegment | undefined,
) {
  return segment ? getMarkdownDiffSegmentIdentity(segment) : null;
}

function getMarkdownDiffSegmentIdentity(segment: MarkdownDiffDocumentSegment) {
  return [
    segment.kind,
    segment.isInAfterDocument ? "after" : "before",
    segment.markdown.length,
    getStableMarkdownDiffSegmentHash(segment.markdown),
  ].join("\0");
}

function getStableMarkdownDiffSegmentHash(text: string) {
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36);
}

function scoreMarkdownDiffSegmentContext(
  previousContext: MarkdownDiffSegmentContext,
  nextContext: MarkdownDiffSegmentContext,
) {
  return (
    scoreMarkdownDiffSegmentContextValue(previousContext.previous, nextContext.previous, 8) +
    scoreMarkdownDiffSegmentContextValue(previousContext.next, nextContext.next, 8) +
    scoreMarkdownDiffSegmentContextValue(
      previousContext.previousPrevious,
      nextContext.previousPrevious,
      2,
    ) +
    scoreMarkdownDiffSegmentContextValue(previousContext.nextNext, nextContext.nextNext, 2)
  );
}

function scoreMarkdownDiffSegmentContextValue(
  previousValue: string | null,
  nextValue: string | null,
  score: number,
) {
  return previousValue != null && previousValue === nextValue ? score : 0;
}

function getMarkdownDiffSegmentDistance(
  previousSegment: MarkdownDiffDocumentSegment,
  nextSegment: MarkdownDiffDocumentSegment,
  previousIndex: number,
  nextIndex: number,
) {
  const distances = [
    Math.abs(previousIndex - nextIndex) * 1_000_000,
  ];
  if (previousSegment.newStart != null && nextSegment.newStart != null) {
    distances.push(Math.abs(previousSegment.newStart - nextSegment.newStart) * 10_000);
  }
  if (previousSegment.oldStart != null && nextSegment.oldStart != null) {
    distances.push(Math.abs(previousSegment.oldStart - nextSegment.oldStart) * 10_000);
  }
  if (
    previousSegment.afterStartOffset !== 0 ||
    previousSegment.afterEndOffset !== 0 ||
    nextSegment.afterStartOffset !== 0 ||
    nextSegment.afterEndOffset !== 0
  ) {
    distances.push(
      Math.abs(previousSegment.afterStartOffset - nextSegment.afterStartOffset),
    );
  }
  return Math.min(...distances);
}

function getAvailableMarkdownDiffSegmentId(
  preferredId: string,
  usedIds: Set<string>,
) {
  if (!usedIds.has(preferredId)) {
    return preferredId;
  }

  let suffix = 1;
  while (usedIds.has(`${preferredId}:stable-${suffix}`)) {
    suffix += 1;
  }
  return `${preferredId}:stable-${suffix}`;
}

function MarkdownDiffDocument({
  allowReadOnlyCaret,
  appearance,
  canEdit,
  completeness,
  documentPath,
  note,
  onCommitRenderedMarkdownSectionDraft,
  onRenderedMarkdownSectionDraftChange,
  onOpenSourceLink,
  onCommitRenderedMarkdownDrafts,
  onRegisterRenderedMarkdownCommitter,
  onReadOnlyRenderedMarkdownMutation,
  onSave,
  scrollRef,
  segments,
  sourceContent,
  workspaceRoot,
}: {
  allowReadOnlyCaret: boolean;
  appearance: MonacoAppearance;
  canEdit: boolean;
  completeness: MarkdownDocumentCompleteness;
  documentPath: string | null;
  note: string | null;
  onCommitRenderedMarkdownSectionDraft: (commit: RenderedMarkdownSectionCommit) => void;
  onRenderedMarkdownSectionDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onCommitRenderedMarkdownDrafts: () => void;
  onRegisterRenderedMarkdownCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onReadOnlyRenderedMarkdownMutation: () => void;
  onSave: MarkdownDiffSaveHandler;
  scrollRef: { current: HTMLDivElement | null };
  segments: MarkdownDiffDocumentSegment[];
  sourceContent: string;
  workspaceRoot: string | null;
}) {
  const visibleNote =
    note ??
    (completeness === "patch"
      ? "Rendered from patch context only. Unchanged document sections outside the diff are omitted."
      : null);

  function handleScrollKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!allowReadOnlyCaret) {
      return;
    }

    const direction = getMarkdownCaretNavigationDirection(event);
    if (direction == null) {
      return;
    }

    redirectCaretOutOfRemovedMarkdownSection(event, event.currentTarget, direction);
  }

  return (
    <div className="markdown-diff-change-view">
      <div className="markdown-document-header">
        <span className="chip">Rendered document</span>
        <span className="chip">{completeness === "full" ? "Full document" : "Patch preview"}</span>
      </div>
      {visibleNote ? <p className="support-copy markdown-document-note">{visibleNote}</p> : null}
      <div
        className="markdown-diff-change-scroll"
        onKeyDown={handleScrollKeyDown}
        ref={scrollRef}
      >
        {segments.length === 0 ? (
          <p className="support-copy markdown-document-empty">No rendered Markdown changes were found.</p>
        ) : (
          renderMarkdownDiffSegments({
            allowReadOnlyCaret,
            appearance,
            canEdit,
            documentPath,
            onCommitRenderedMarkdownDrafts,
            onCommitRenderedMarkdownSectionDraft,
            onOpenSourceLink,
            onRegisterRenderedMarkdownCommitter,
            onReadOnlyRenderedMarkdownMutation,
            onRenderedMarkdownSectionDraftChange,
            onSave,
            segments,
            sourceContent,
            workspaceRoot,
          })
        )}
      </div>
    </div>
  );
}

function renderMarkdownDiffSegments({
  allowReadOnlyCaret,
  appearance,
  canEdit,
  documentPath,
  onCommitRenderedMarkdownDrafts,
  onCommitRenderedMarkdownSectionDraft,
  onOpenSourceLink,
  onRegisterRenderedMarkdownCommitter,
  onReadOnlyRenderedMarkdownMutation,
  onRenderedMarkdownSectionDraftChange,
  onSave,
  segments,
  sourceContent,
  workspaceRoot,
}: {
  allowReadOnlyCaret: boolean;
  appearance: MonacoAppearance;
  canEdit: boolean;
  documentPath: string | null;
  onCommitRenderedMarkdownDrafts: () => void;
  onCommitRenderedMarkdownSectionDraft: (commit: RenderedMarkdownSectionCommit) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onRegisterRenderedMarkdownCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onReadOnlyRenderedMarkdownMutation: () => void;
  onRenderedMarkdownSectionDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onSave: MarkdownDiffSaveHandler;
  segments: MarkdownDiffDocumentSegment[];
  sourceContent: string;
  workspaceRoot: string | null;
}) {
  const rendered: ReactNode[] = [];
  for (let index = 0; index < segments.length; index += 1) {
    const segment = segments[index];
    if (!segment) {
      continue;
    }

    if (segment.kind === "normal") {
      rendered.push(
        <EditableRenderedMarkdownSection
          allowReadOnlyCaret={allowReadOnlyCaret && segment.isInAfterDocument}
          appearance={appearance}
          canEdit={canEdit && segment.isInAfterDocument}
          className="markdown-diff-normal-section"
          documentPath={documentPath}
          key={segment.id}
          onDraftChange={onRenderedMarkdownSectionDraftChange}
          onOpenSourceLink={onOpenSourceLink}
          onCommitDrafts={onCommitRenderedMarkdownDrafts}
          onCommitSectionDraft={onCommitRenderedMarkdownSectionDraft}
          onReadOnlyMutation={onReadOnlyRenderedMarkdownMutation}
          onRegisterCommitter={onRegisterRenderedMarkdownCommitter}
          onSave={onSave}
          segment={segment}
          sourceContent={sourceContent}
          workspaceRoot={workspaceRoot}
        />,
      );
      continue;
    }

    const changedSegments = [segment];
    while (segments[index + 1]?.kind !== "normal" && segments[index + 1] != null) {
      changedSegments.push(segments[index + 1]!);
      index += 1;
    }

    rendered.push(
      <section className="markdown-diff-change-block" key={changedSegments.map((changed) => changed.id).join(":")}>
        {changedSegments.map((changedSegment) => {
          const tone = changedSegment.kind === "removed" ? "removed" : "added";
          return (
            <RenderedMarkdownChangeSection
              allowReadOnlyCaret={allowReadOnlyCaret && tone === "added" && changedSegment.isInAfterDocument}
              appearance={appearance}
              canEdit={canEdit && tone === "added" && changedSegment.isInAfterDocument}
              documentPath={documentPath}
              key={changedSegment.id}
              onDraftChange={onRenderedMarkdownSectionDraftChange}
              onOpenSourceLink={onOpenSourceLink}
              onCommitDrafts={onCommitRenderedMarkdownDrafts}
              onCommitSectionDraft={onCommitRenderedMarkdownSectionDraft}
              onReadOnlyMutation={onReadOnlyRenderedMarkdownMutation}
              onRegisterCommitter={onRegisterRenderedMarkdownCommitter}
              onSave={onSave}
              segment={changedSegment}
              sourceContent={sourceContent}
              tone={tone}
              workspaceRoot={workspaceRoot}
            />
          );
        })}
      </section>,
    );
  }

  return rendered;
}

function RenderedMarkdownChangeSection({
  allowReadOnlyCaret,
  appearance,
  canEdit,
  documentPath,
  onCommitDrafts,
  onCommitSectionDraft,
  onDraftChange,
  onOpenSourceLink,
  onReadOnlyMutation,
  onRegisterCommitter,
  onSave,
  segment,
  sourceContent,
  tone,
  workspaceRoot,
}: {
  allowReadOnlyCaret: boolean;
  appearance: MonacoAppearance;
  canEdit: boolean;
  documentPath: string | null;
  onCommitDrafts: () => void;
  onCommitSectionDraft: (commit: RenderedMarkdownSectionCommit) => void;
  onDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onReadOnlyMutation: () => void;
  onRegisterCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onSave: MarkdownDiffSaveHandler;
  segment: MarkdownDiffDocumentSegment;
  sourceContent: string;
  tone: "added" | "removed";
  workspaceRoot: string | null;
}) {
  return (
    <section
      className={`markdown-diff-rendered-section markdown-diff-rendered-section-with-line-gutter markdown-diff-rendered-section-${tone}`}
    >
      <EditableRenderedMarkdownSection
        allowReadOnlyCaret={allowReadOnlyCaret}
        appearance={appearance}
        canEdit={canEdit}
        className="markdown-diff-rendered-section-body"
        documentPath={documentPath}
        onCommitDrafts={onCommitDrafts}
        onCommitSectionDraft={onCommitSectionDraft}
        onDraftChange={onDraftChange}
        onOpenSourceLink={onOpenSourceLink}
        onReadOnlyMutation={onReadOnlyMutation}
        onRegisterCommitter={onRegisterCommitter}
        onSave={onSave}
        segment={segment}
        sourceContent={sourceContent}
        workspaceRoot={workspaceRoot}
      />
    </section>
  );
}

function EditableRenderedMarkdownSection({
  allowReadOnlyCaret,
  appearance,
  canEdit,
  className,
  documentPath,
  onCommitDrafts,
  onCommitSectionDraft,
  onDraftChange,
  onOpenSourceLink,
  onReadOnlyMutation,
  onRegisterCommitter,
  onSave,
  segment,
  sourceContent,
  workspaceRoot,
}: {
  allowReadOnlyCaret: boolean;
  appearance: MonacoAppearance;
  canEdit: boolean;
  className: string;
  documentPath: string | null;
  onCommitDrafts: () => void;
  onCommitSectionDraft: (commit: RenderedMarkdownSectionCommit) => void;
  onDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onReadOnlyMutation: () => void;
  onRegisterCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onSave: MarkdownDiffSaveHandler;
  segment: MarkdownDiffDocumentSegment;
  sourceContent: string;
  workspaceRoot: string | null;
}) {
  const canUseCaret = canEdit || allowReadOnlyCaret;
  const classNames = `${className}${canUseCaret ? " markdown-diff-editable-section" : ""}${allowReadOnlyCaret && !canEdit ? " markdown-diff-readonly-caret-section" : ""}`;
  const hasUncommittedUserEditRef = useRef(false);
  const draftSegmentRef = useRef<MarkdownDiffDocumentSegment | null>(null);
  const draftSourceContentRef = useRef<string | null>(null);
  const previousSegmentMarkdownRef = useRef(segment.markdown);
  const sectionRef = useRef<HTMLElement | null>(null);
  const [renderResetVersion, setRenderResetVersion] = useState(0);

  useEffect(() => {
    if (previousSegmentMarkdownRef.current === segment.markdown) {
      return;
    }

    if (hasUncommittedUserEditRef.current) {
      const section = sectionRef.current;
      const editedMarkdown = section
        ? normalizeEditedMarkdownSection(
            serializeEditableMarkdownSection(section),
            previousSegmentMarkdownRef.current,
          )
        : null;
      if (editedMarkdown !== segment.markdown) {
        return;
      }
    }

    previousSegmentMarkdownRef.current = segment.markdown;
    hasUncommittedUserEditRef.current = false;
    draftSegmentRef.current = null;
    draftSourceContentRef.current = null;
    setRenderResetVersion((current) => current + 1);
  }, [segment.markdown]);

  function readEditedMarkdown(section: HTMLElement, baselineMarkdown = segment.markdown) {
    return normalizeEditedMarkdownSection(
      serializeEditableMarkdownSection(section),
      baselineMarkdown,
    );
  }

  function updateLocalDraftState(nextMarkdown: string) {
    const baseSegment = draftSegmentRef.current ?? segment;
    const normalizedDraft = normalizeEditedMarkdownSection(
      nextMarkdown,
      baseSegment.markdown,
    );
    const isDirty = normalizedDraft !== baseSegment.markdown;
    hasUncommittedUserEditRef.current = isDirty;
    if (isDirty) {
      draftSegmentRef.current = baseSegment;
      draftSourceContentRef.current = draftSourceContentRef.current ?? sourceContent;
    } else {
      draftSegmentRef.current = null;
      draftSourceContentRef.current = null;
    }
    onDraftChange(baseSegment, normalizedDraft);
  }

  function handleInput(event: FormEvent<HTMLElement>) {
    if (!canEdit) {
      if (allowReadOnlyCaret) {
        event.currentTarget.textContent = segment.markdown;
        onReadOnlyMutation();
      }
      return;
    }

    const baseSegment = draftSegmentRef.current ?? segment;
    const nextMarkdown = readEditedMarkdown(event.currentTarget, baseSegment.markdown);
    updateLocalDraftState(nextMarkdown);
  }

  const collectSectionEdit = useCallback((section: HTMLElement): RenderedMarkdownSectionCommit | null => {
    if (!canEdit) {
      return null;
    }

    // Cursor-only focus changes must not serialize rendered Markdown. The
    // renderer is intentionally richer than the source text, so a no-input
    // serialize pass can rewrite harmless source formatting (for example
    // `*` bullets to `-` bullets) and create new diff sections.
    if (!hasUncommittedUserEditRef.current) {
      return null;
    }

    const commitSegment = draftSegmentRef.current ?? segment;
    const nextMarkdown = readEditedMarkdown(section, commitSegment.markdown);
    const sourceAtDraftStart = draftSourceContentRef.current ?? sourceContent;
    if (nextMarkdown !== commitSegment.markdown) {
      return {
        currentSegment: segment,
        nextMarkdown,
        segment: commitSegment,
        sourceContent: sourceAtDraftStart,
      };
    }

    hasUncommittedUserEditRef.current = false;
    draftSegmentRef.current = null;
    draftSourceContentRef.current = null;
    onDraftChange(commitSegment, commitSegment.markdown);
    return null;
  }, [canEdit, onDraftChange, segment, sourceContent]);

  function commitOwnDraft() {
    const section = sectionRef.current;
    if (!section) {
      return;
    }

    const commit = collectSectionEdit(section);
    if (commit) {
      onCommitSectionDraft(commit);
    }
  }

  function handleBeforeInput(event: FormEvent<HTMLElement>) {
    if (allowReadOnlyCaret && !canEdit) {
      event.preventDefault();
    }
  }

  function handleReadOnlyMutationEvent(event: FormEvent<HTMLElement>) {
    if (allowReadOnlyCaret && !canEdit) {
      event.preventDefault();
    }
  }

  function handlePaste(event: ClipboardEvent<HTMLElement>) {
    if (!canEdit) {
      handleReadOnlyMutationEvent(event);
      return;
    }

    const html = event.clipboardData.getData("text/html");
    const fallbackText = event.clipboardData.getData("text/plain");
    if (!html && !fallbackText) {
      return;
    }

    event.preventDefault();
    insertSanitizedMarkdownPaste(
      event.currentTarget,
      html,
      fallbackText,
    );
    const baseSegment = draftSegmentRef.current ?? segment;
    const nextMarkdown = readEditedMarkdown(event.currentTarget, baseSegment.markdown);
    updateLocalDraftState(nextMarkdown);
  }

  function isReadOnlyMutationKey(event: KeyboardEvent<HTMLElement>) {
    if (event.altKey || event.ctrlKey || event.metaKey) {
      return false;
    }

    return event.key === "Backspace" || event.key === "Delete" || event.key === "Enter" || event.key.length === 1;
  }

  useEffect(
    () =>
      onRegisterCommitter(() => {
        const section = sectionRef.current;
        return section ? collectSectionEdit(section) : null;
      }),
    [collectSectionEdit, onRegisterCommitter],
  );

  function handleKeyDown(event: KeyboardEvent<HTMLElement>) {
    if (!canUseCaret) {
      return;
    }

    const direction = getMarkdownCaretNavigationDirection(event);
    const scrollRegion = event.currentTarget.closest<HTMLElement>(".markdown-diff-change-scroll");
    if (
      allowReadOnlyCaret &&
      !canEdit &&
      direction != null &&
      redirectCaretOutOfRemovedMarkdownSection(event, scrollRegion, direction)
    ) {
      return;
    }

    if (allowReadOnlyCaret && !canEdit && isReadOnlyMutationKey(event)) {
      event.preventDefault();
      return;
    }

    if (canEdit && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      const focusSnapshot = captureEditableMarkdownFocusSnapshot(event.currentTarget);
      onCommitDrafts();
      scheduleEditableMarkdownFocusRestore(focusSnapshot);
      const saveResult = onSave();
      if (saveResult && typeof saveResult.then === "function") {
        void saveResult.finally(() => {
          scheduleEditableMarkdownFocusRestore(focusSnapshot);
        });
      }
      return;
    }

    if (canEdit && event.key === "Escape") {
      if (hasUncommittedUserEditRef.current) {
        event.preventDefault();
        hasUncommittedUserEditRef.current = false;
        const baseSegment = draftSegmentRef.current ?? segment;
        draftSegmentRef.current = null;
        draftSourceContentRef.current = null;
        onDraftChange(baseSegment, baseSegment.markdown);
        setRenderResetVersion((current) => current + 1);
      }
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return;
    }

    const flushDraftsBeforeNavigation = canEdit
      ? () => {
          flushSync(() => onCommitDrafts());
        }
      : undefined;

    if (direction != null && (event.key === "PageDown" || event.key === "PageUp")) {
      if (
        moveEditableMarkdownCaretByPage(
          event.currentTarget,
          direction,
          flushDraftsBeforeNavigation,
        )
      ) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowDown" &&
      isSelectionAtEditableSectionBoundary(event.currentTarget, "end")
    ) {
      if (
        focusAdjacentEditableMarkdownSection(event.currentTarget, 1, flushDraftsBeforeNavigation)
      ) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowRight" &&
      isSelectionAtEditableSectionBoundary(event.currentTarget, "end")
    ) {
      if (
        focusAdjacentEditableMarkdownSection(event.currentTarget, 1, flushDraftsBeforeNavigation)
      ) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowUp" &&
      isSelectionAtEditableSectionBoundary(event.currentTarget, "start")
    ) {
      if (
        focusAdjacentEditableMarkdownSection(event.currentTarget, -1, flushDraftsBeforeNavigation)
      ) {
        event.preventDefault();
      }
      return;
    }

    if (
      event.key === "ArrowLeft" &&
      isSelectionAtEditableSectionBoundary(event.currentTarget, "start")
    ) {
      if (
        focusAdjacentEditableMarkdownSection(event.currentTarget, -1, flushDraftsBeforeNavigation)
      ) {
        event.preventDefault();
      }
    }
  }

  const renderedSegment = hasUncommittedUserEditRef.current
    ? draftSegmentRef.current ?? segment
    : segment;
  const renderedSegmentLineNumber = getMarkdownDiffSegmentLineNumber(renderedSegment);
  const renderedMarkdownContent = useMemo(
    () => (
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        markdown={renderedSegment.markdown}
        onOpenSourceLink={onOpenSourceLink}
        preserveMermaidSource={canEdit}
        renderMermaidDiagrams
        showLineNumbers={renderedSegmentLineNumber != null}
        startLineNumber={renderedSegmentLineNumber}
        workspaceRoot={workspaceRoot}
      />
    ),
    [
      appearance,
      canEdit,
      documentPath,
      onOpenSourceLink,
      renderedSegment.markdown,
      renderedSegmentLineNumber,
      workspaceRoot,
    ],
  );

  return (
    <section
      className={classNames}
      aria-readonly={allowReadOnlyCaret && !canEdit ? true : undefined}
      contentEditable={canUseCaret}
      data-markdown-caret={canUseCaret ? "true" : undefined}
      data-markdown-editable={canEdit ? "true" : undefined}
      data-markdown-readonly={allowReadOnlyCaret && !canEdit ? "true" : undefined}
      data-markdown-segment-after-end={renderedSegment.afterEndOffset}
      data-markdown-segment-after-start={renderedSegment.afterStartOffset}
      onBeforeInput={handleBeforeInput}
      onBlur={() => {
        if (canEdit) {
          commitOwnDraft();
        }
      }}
      onCut={handleReadOnlyMutationEvent}
      onDrop={handleReadOnlyMutationEvent}
      onInput={handleInput}
      onKeyDown={handleKeyDown}
      onPaste={handlePaste}
      ref={sectionRef}
      suppressContentEditableWarning
      tabIndex={canUseCaret ? 0 : undefined}
    >
      <div key={renderResetVersion} className="markdown-diff-rendered-section-content">
        {renderedMarkdownContent}
      </div>
    </section>
  );
}

function getMarkdownCaretNavigationDirection(event: KeyboardEvent<HTMLElement>) {
  if (event.altKey || event.ctrlKey || event.metaKey) {
    return null;
  }

  if (event.key === "ArrowDown" || event.key === "ArrowRight" || event.key === "PageDown") {
    return 1;
  }

  if (event.key === "ArrowUp" || event.key === "ArrowLeft" || event.key === "PageUp") {
    return -1;
  }

  return null;
}

function redirectCaretOutOfRemovedMarkdownSection(
  event: KeyboardEvent<HTMLElement>,
  scrollRegion: HTMLElement | null,
  direction: -1 | 1,
) {
  if (!scrollRegion) {
    return false;
  }

  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return false;
  }

  const range = selection.getRangeAt(0);
  const removedSection = closestElementFromNode(
    range.startContainer,
    ".markdown-diff-rendered-section-removed",
  );
  if (!removedSection || !scrollRegion.contains(removedSection)) {
    return false;
  }

  const targetSection = findAdjacentMarkdownCaretSection(scrollRegion, removedSection, direction)
    ?? findAdjacentMarkdownCaretSection(scrollRegion, removedSection, direction > 0 ? -1 : 1);
  if (!targetSection) {
    return false;
  }

  event.preventDefault();
  event.stopPropagation();
  placeCaretInEditableMarkdownSection(targetSection, direction > 0 ? "start" : "end");
  targetSection.scrollIntoView?.({ block: "nearest" });
  return true;
}

function findAdjacentMarkdownCaretSection(
  scrollRegion: HTMLElement,
  origin: HTMLElement,
  direction: -1 | 1,
) {
  const caretSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );

  if (direction > 0) {
    return caretSections.find((section) =>
      Boolean(origin.compareDocumentPosition(section) & Node.DOCUMENT_POSITION_FOLLOWING),
    ) ?? null;
  }

  for (let index = caretSections.length - 1; index >= 0; index -= 1) {
    const section = caretSections[index];
    if (section && Boolean(origin.compareDocumentPosition(section) & Node.DOCUMENT_POSITION_PRECEDING)) {
      return section;
    }
  }

  return null;
}

function closestElementFromNode(node: Node, selector: string) {
  const element = node instanceof Element ? node : node.parentElement;
  return element?.closest<HTMLElement>(selector) ?? null;
}

function getMarkdownDiffSegmentLineNumber(segment: MarkdownDiffDocumentSegment) {
  return segment.kind === "removed"
    ? segment.oldStart ?? segment.newStart ?? null
    : segment.newStart ?? segment.oldStart ?? null;
}

function insertSanitizedMarkdownPaste(
  section: HTMLElement,
  html: string,
  fallbackText: string,
) {
  const template = document.createElement("template");
  template.innerHTML = html;
  sanitizePastedMarkdownFragment(template.content);

  let fragment = template.content;
  if (fragment.childNodes.length === 0 && fallbackText.length > 0) {
    fragment = document.createDocumentFragment();
    fragment.append(document.createTextNode(fallbackText));
  }

  const insertedNodes = Array.from(fragment.childNodes);
  if (insertedNodes.length === 0) {
    return;
  }

  const selection = window.getSelection();
  const range =
    selection && selection.rangeCount > 0 && section.contains(selection.anchorNode)
      ? selection.getRangeAt(0)
      : document.createRange();
  if (!selection || selection.rangeCount === 0 || !section.contains(selection.anchorNode)) {
    range.selectNodeContents(section);
    range.collapse(false);
  }

  range.deleteContents();
  range.insertNode(fragment);

  const lastInsertedNode = insertedNodes[insertedNodes.length - 1];
  if (lastInsertedNode?.parentNode) {
    const afterInsertRange = document.createRange();
    afterInsertRange.setStartAfter(lastInsertedNode);
    afterInsertRange.collapse(true);
    selection?.removeAllRanges();
    selection?.addRange(afterInsertRange);
  }
}

const PASTED_MARKDOWN_HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";

const PASTED_MARKDOWN_ALLOWED_ELEMENTS = new Set([
  "a",
  "b",
  "blockquote",
  "br",
  "code",
  "del",
  "div",
  "em",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "hr",
  "i",
  "li",
  "ol",
  "p",
  "pre",
  "s",
  "span",
  "strong",
  "table",
  "tbody",
  "td",
  "tfoot",
  "th",
  "thead",
  "tr",
  "ul",
]);

const PASTED_MARKDOWN_DROPPED_ELEMENTS = new Set([
  "area",
  "audio",
  "base",
  "button",
  "canvas",
  "embed",
  "form",
  "iframe",
  "img",
  "input",
  "link",
  "map",
  "math",
  "meta",
  "object",
  "option",
  "picture",
  "script",
  "select",
  "source",
  "style",
  "svg",
  "textarea",
  "video",
]);

function sanitizePastedMarkdownFragment(root: ParentNode) {
  const rootNode = root as Node;
  const elements =
    root instanceof Element
      ? [root, ...Array.from(root.querySelectorAll<Element>("*"))]
      : Array.from(root.querySelectorAll<Element>("*"));

  for (const element of elements) {
    if (!rootNode.contains(element)) {
      continue;
    }

    const tagName = element.tagName.toLowerCase();
    if (
      element.namespaceURI !== PASTED_MARKDOWN_HTML_NAMESPACE ||
      PASTED_MARKDOWN_DROPPED_ELEMENTS.has(tagName)
    ) {
      element.remove();
      continue;
    }

    if (!PASTED_MARKDOWN_ALLOWED_ELEMENTS.has(tagName)) {
      element.replaceWith(...Array.from(element.childNodes));
      continue;
    }

    for (const attributeName of element.getAttributeNames()) {
      const normalizedAttributeName = attributeName.toLowerCase();
      if (tagName === "a" && normalizedAttributeName === "href") {
        const href = element.getAttribute(attributeName);
        if (href && isSafePastedMarkdownHref(href)) {
          continue;
        }
      } else if (tagName === "code" && normalizedAttributeName === "class") {
        const languageClass = normalizePastedMarkdownCodeClass(
          element.getAttribute(attributeName) ?? "",
        );
        if (languageClass) {
          element.setAttribute("class", languageClass);
          continue;
        }
      }

      if (element.hasAttribute(attributeName)) {
        element.removeAttribute(attributeName);
      }
    }
  }
}

function normalizePastedMarkdownCodeClass(className: string) {
  const languageMatch = className.match(/(?:^|\s)language-([\w-]+)(?:\s|$)/);
  return languageMatch ? `language-${languageMatch[1]}` : "";
}

function isSafePastedMarkdownHref(href: string) {
  const trimmed = href.trim();
  if (!trimmed) {
    return false;
  }

  if (/^[a-zA-Z]:[\\/]/.test(trimmed)) {
    return true;
  }

  const normalized = trimmed.replace(/[\u0000-\u001F\u007F\s]+/g, "");
  const colonIndex = normalized.indexOf(":");
  if (colonIndex === -1) {
    return true;
  }

  const protocol = normalized.slice(0, colonIndex).toLowerCase();
  return protocol === "http" || protocol === "https" || protocol === "mailto";
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
  return (
    node.tagName.toLowerCase() === "button" ||
    node.getAttribute("aria-hidden") === "true" ||
    node.dataset.markdownSerialization === "skip"
  );
}

function captureEditableMarkdownFocusSnapshot(section: HTMLElement): EditableMarkdownFocusSnapshot {
  const scrollRegion = section.closest<HTMLElement>(".markdown-diff-change-scroll");
  return {
    caretTextOffset: readEditableMarkdownCaretTextOffset(section),
    scrollRegion,
    scrollTop: scrollRegion?.scrollTop ?? null,
    section,
    segmentAfterEndOffset: readNumberDataAttribute(section, "markdownSegmentAfterEnd"),
    segmentAfterStartOffset: readNumberDataAttribute(section, "markdownSegmentAfterStart"),
  };
}

function scheduleEditableMarkdownFocusRestore(snapshot: EditableMarkdownFocusSnapshot) {
  window.requestAnimationFrame(() => {
    if (restoreEditableMarkdownFocus(snapshot)) {
      return;
    }

    window.setTimeout(() => {
      restoreEditableMarkdownFocus(snapshot);
    }, 0);
  });
}

function restoreEditableMarkdownFocus(snapshot: EditableMarkdownFocusSnapshot) {
  if (!shouldRestoreEditableMarkdownFocus(snapshot)) {
    return true;
  }

  const section = findEditableMarkdownFocusTarget(snapshot);
  if (!section) {
    return false;
  }

  if (snapshot.caretTextOffset == null) {
    placeCaretInEditableMarkdownSection(section, "end");
  } else {
    placeCaretInEditableMarkdownSectionAtTextOffset(section, snapshot.caretTextOffset);
  }

  if (snapshot.scrollRegion?.isConnected && snapshot.scrollTop != null) {
    snapshot.scrollRegion.scrollTop = snapshot.scrollTop;
  }

  return true;
}

function shouldRestoreEditableMarkdownFocus(snapshot: EditableMarkdownFocusSnapshot) {
  const activeElement = document.activeElement;
  if (!activeElement || activeElement === document.body) {
    return true;
  }

  if (snapshot.section.isConnected && activeElement === snapshot.section) {
    return true;
  }

  return false;
}

function findEditableMarkdownFocusTarget(snapshot: EditableMarkdownFocusSnapshot) {
  if (snapshot.section.isConnected && snapshot.section.dataset.markdownEditable === "true") {
    return snapshot.section;
  }

  const scrollRegion =
    snapshot.scrollRegion?.isConnected === true
      ? snapshot.scrollRegion
      : document.querySelector<HTMLElement>(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return null;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-editable='true']"),
  );
  if (editableSections.length === 0) {
    return null;
  }

  const exactMatch = editableSections.find(
    (section) =>
      readNumberDataAttribute(section, "markdownSegmentAfterStart") === snapshot.segmentAfterStartOffset,
  );
  if (exactMatch) {
    return exactMatch;
  }

  const containingMatch = editableSections.find((section) => {
    const start = readNumberDataAttribute(section, "markdownSegmentAfterStart");
    const end = readNumberDataAttribute(section, "markdownSegmentAfterEnd");
    return (
      snapshot.segmentAfterStartOffset != null &&
      start != null &&
      end != null &&
      start <= snapshot.segmentAfterStartOffset &&
      snapshot.segmentAfterStartOffset <= end
    );
  });
  if (containingMatch) {
    return containingMatch;
  }

  return editableSections[0] ?? null;
}

function readEditableMarkdownCaretTextOffset(section: HTMLElement) {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return null;
  }

  const range = selection.getRangeAt(0);
  if (!section.contains(range.startContainer)) {
    return null;
  }

  const root = section.querySelector<HTMLElement>(".markdown-copy") ?? section;
  const prefixRange = document.createRange();
  prefixRange.selectNodeContents(root);
  try {
    prefixRange.setEnd(range.startContainer, range.startOffset);
  } catch {
    return null;
  }
  return prefixRange.toString().length;
}

function readNumberDataAttribute(element: HTMLElement, key: string) {
  const value = element.dataset[key];
  if (value == null || value.length === 0) {
    return null;
  }

  const numberValue = Number(value);
  return Number.isFinite(numberValue) ? numberValue : null;
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

  const textNodes = collectEditableMarkdownTextNodes(section);
  const boundaryTextNode = boundary === "start" ? textNodes[0] : textNodes[textNodes.length - 1];
  if (!boundaryTextNode) {
    return true;
  }

  const caretRange = range.cloneRange();
  caretRange.collapse(true);
  const boundaryRange = document.createRange();
  if (boundary === "start") {
    boundaryRange.setStart(boundaryTextNode, 0);
    boundaryRange.collapse(true);
    return caretRange.compareBoundaryPoints(Range.START_TO_START, boundaryRange) <= 0;
  }

  boundaryRange.setStart(boundaryTextNode, boundaryTextNode.textContent?.length ?? 0);
  boundaryRange.collapse(true);
  return caretRange.compareBoundaryPoints(Range.START_TO_START, boundaryRange) >= 0;
}

function focusAdjacentEditableMarkdownSection(
  currentSection: HTMLElement,
  direction: -1 | 1,
  beforeFocus?: () => void,
) {
  const scrollRegion = currentSection.closest<HTMLElement>(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return false;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const currentIndex = editableSections.indexOf(currentSection);
  if (currentIndex < 0) {
    return false;
  }

  const targetIndex = currentIndex + direction;
  if (targetIndex < 0 || targetIndex >= editableSections.length) {
    return false;
  }

  beforeFocus?.();

  const latestEditableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const latestCurrentIndex = latestEditableSections.indexOf(currentSection);
  const resolvedTargetIndex = latestCurrentIndex >= 0 ? latestCurrentIndex + direction : targetIndex;
  const nextSection = latestEditableSections[resolvedTargetIndex];
  if (!nextSection) {
    return false;
  }

  const shouldPreserveScroll = isElementVisibleWithinScrollRegion(nextSection, scrollRegion);
  const previousScrollTop = scrollRegion.scrollTop;
  placeCaretInEditableMarkdownSection(nextSection, direction > 0 ? "start" : "end");
  if (shouldPreserveScroll) {
    scrollRegion.scrollTop = previousScrollTop;
    window.requestAnimationFrame(() => {
      scrollRegion.scrollTop = previousScrollTop;
    });
  }
  return true;
}

function moveEditableMarkdownCaretByPage(
  currentSection: HTMLElement,
  direction: -1 | 1,
  beforeMove?: () => void,
) {
  const scrollRegion = currentSection.closest<HTMLElement>(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return false;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const currentIndex = editableSections.indexOf(currentSection);
  if (currentIndex < 0) {
    return false;
  }

  const targetIndex = resolveEditableMarkdownPageTargetIndex(
    editableSections,
    currentSection,
    scrollRegion,
    currentIndex,
    direction,
  );
  if (targetIndex === currentIndex) {
    return false;
  }
  const targetIndexDelta = targetIndex - currentIndex;

  beforeMove?.();

  const latestEditableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const latestCurrentIndex = latestEditableSections.indexOf(currentSection);
  const resolvedTargetIndex =
    latestCurrentIndex >= 0
      ? latestCurrentIndex + targetIndexDelta
      : targetIndex;
  const nextSection = latestEditableSections[
    clampNumber(resolvedTargetIndex, 0, latestEditableSections.length - 1)
  ];
  if (!nextSection) {
    return false;
  }

  placeCaretInEditableMarkdownSection(nextSection, direction > 0 ? "start" : "end");
  nextSection.scrollIntoView?.({ block: "nearest" });
  return true;
}

function resolveEditableMarkdownPageTargetIndex(
  editableSections: HTMLElement[],
  currentSection: HTMLElement,
  scrollRegion: HTMLElement,
  currentIndex: number,
  direction: -1 | 1,
) {
  const fallbackIndex = clampNumber(currentIndex + direction, 0, editableSections.length - 1);
  const scrollRegionRect = scrollRegion.getBoundingClientRect();
  const currentRect = currentSection.getBoundingClientRect();
  if (
    scrollRegion.clientHeight <= 0 ||
    scrollRegionRect.height <= 0 ||
    currentRect.height <= 0
  ) {
    return fallbackIndex;
  }

  const pageDistance = Math.max(scrollRegion.clientHeight * 0.85, 160);
  const currentBoundaryY =
    direction > 0
      ? currentRect.bottom + scrollRegion.scrollTop - scrollRegionRect.top
      : currentRect.top + scrollRegion.scrollTop - scrollRegionRect.top;
  const targetY = currentBoundaryY + direction * pageDistance;

  if (direction > 0) {
    const targetIndex = editableSections.findIndex((section, index) => {
      if (index <= currentIndex) {
        return false;
      }

      const sectionTop = section.getBoundingClientRect().top + scrollRegion.scrollTop - scrollRegionRect.top;
      return sectionTop >= targetY;
    });
    return targetIndex >= 0 ? targetIndex : editableSections.length - 1;
  }

  for (let index = currentIndex - 1; index >= 0; index -= 1) {
    const sectionBottom =
      editableSections[index].getBoundingClientRect().bottom +
      scrollRegion.scrollTop -
      scrollRegionRect.top;
    if (sectionBottom <= targetY) {
      return index;
    }
  }
  return 0;
}

function isElementVisibleWithinScrollRegion(element: HTMLElement, scrollRegion: HTMLElement) {
  const elementRect = element.getBoundingClientRect();
  const scrollRegionRect = scrollRegion.getBoundingClientRect();
  return elementRect.bottom >= scrollRegionRect.top && elementRect.top <= scrollRegionRect.bottom;
}

function placeCaretInEditableMarkdownSection(section: HTMLElement, boundary: "end" | "start") {
  section.focus({ preventScroll: true });

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
    range.selectNodeContents(section.querySelector(".markdown-copy") ?? section);
    range.collapse(boundary === "start");
  }
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
}

function placeCaretInEditableMarkdownSectionAtTextOffset(section: HTMLElement, caretTextOffset: number) {
  section.focus({ preventScroll: true });

  const selection = window.getSelection();
  if (!selection) {
    return;
  }

  const range = document.createRange();
  const textNodes = collectEditableMarkdownTextNodes(section);
  let remainingOffset = Math.max(0, caretTextOffset);
  for (const textNode of textNodes) {
    const textLength = textNode.textContent?.length ?? 0;
    if (remainingOffset <= textLength) {
      range.setStart(textNode, remainingOffset);
      range.collapse(true);
      selection.removeAllRanges();
      selection.addRange(range);
      return;
    }
    remainingOffset -= textLength;
  }

  placeCaretInEditableMarkdownSection(section, "end");
}

function findEditableMarkdownTextNode(root: Node, position: "first" | "last"): Text | null {
  const textNodes = root instanceof HTMLElement ? collectEditableMarkdownTextNodes(root) : [];
  return position === "first" ? textNodes[0] ?? null : textNodes[textNodes.length - 1] ?? null;
}

function collectEditableMarkdownTextNodes(root: HTMLElement) {
  const textNodes: Text[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      if (!(node instanceof Text) || !node.textContent || node.textContent.trim().length === 0) {
        return NodeFilter.FILTER_REJECT;
      }

      return isNodeInsideSkippedMarkdownEditableNode(node, root)
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT;
    },
  });

  let currentNode = walker.nextNode();
  while (currentNode) {
    textNodes.push(currentNode as Text);
    currentNode = walker.nextNode();
  }

  return textNodes;
}

function isNodeInsideSkippedMarkdownEditableNode(node: Node, root: HTMLElement) {
  let current = node.parentElement;
  while (current && current !== root) {
    if (shouldSkipMarkdownEditableNode(current)) {
      return true;
    }
    current = current.parentElement;
  }
  return false;
}

function clampNumber(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

function formatMarkdownSideSource(source: MarkdownDiffPreviewSideSource) {
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
