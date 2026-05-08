import { Suspense, lazy, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { copyTextToClipboard } from "../clipboard";
import { MarkdownContent, type MarkdownFileLinkTarget } from "../message-cards";
import type { MonacoCodeEditorStatus, MonacoInlineZone } from "../MonacoCodeEditor";
import { resolveMonacoLanguage, type MonacoAppearance } from "../monaco";
import {
  detectRenderableRegions,
  isMermaidFenceLanguage,
  isMathFenceLanguage,
  type SourceRenderableRegion,
} from "../source-renderers";
import type { WorkspaceFileChangeKind } from "../types";
import { rebaseContentOntoDisk } from "./content-rebase";
// Rendered-Markdown editing is shared with DiffPanel; these diff-named
// modules own the neutral segment and commit-range contract.
import { EditableRenderedMarkdownSection } from "./markdown-diff-change-section";
import {
  hasOverlappingMarkdownCommitRanges,
  resolveRenderedMarkdownCommitRange,
  type MarkdownDocumentRange,
  type RenderedMarkdownSectionCommit,
} from "./markdown-commit-ranges";
import {
  applyMarkdownDocumentEolStyle,
  detectMarkdownDocumentEolStyle,
  normalizeEditedMarkdownSection,
  normalizeMarkdownDocumentLineEndings,
  replaceMarkdownDocumentRange,
  type MarkdownDiffDocumentSegment,
} from "./markdown-diff-segments";
import {
  RendererPreviewPane,
  composeInlineRegionFence,
  describeRenderableKinds,
} from "./source-renderer-preview";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);
const MonacoDiffEditor = lazy(() =>
  import("../MonacoDiffEditor").then(({ MonacoDiffEditor }) => ({ default: MonacoDiffEditor })),
);

const DEFAULT_EDITOR_STATUS: MonacoCodeEditorStatus = {
  line: 1,
  column: 1,
  tabSize: 2,
  insertSpaces: true,
  endOfLine: "LF",
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

type SourceDocumentMode = "code" | "preview" | "split";

export type SourceFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  contentHash: string | null;
  mtimeMs?: number | null;
  sizeBytes?: number | null;
  staleOnDisk?: boolean;
  externalChangeKind?: WorkspaceFileChangeKind | null;
  externalContentHash?: string | null;
  externalMtimeMs?: number | null;
  externalSizeBytes?: number | null;
  error: string | null;
  language: string | null;
};

export type SourcePanelFocus = {
  line: number;
  column: number | null;
  token: string | null;
};

export type SourceSaveOptions = {
  baseHash?: string | null;
  overwrite?: boolean;
};

export function SourcePanel({
  editorAppearance,
  editorFontSizePx,
  fileState,
  sourceFocus = null,
  sourcePath,
  onOpenInstructionDebugger,
  onDirtyChange,
  onFetchLatestFile,
  onAdoptFileState,
  onOpenSourceLink,
  onReloadFile,
  onSaveFile,
  workspaceRoot = null,
}: {
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  fileState: SourceFileState;
  sourceFocus?: SourcePanelFocus | null;
  sourcePath: string | null;
  onOpenInstructionDebugger?: (() => void) | null;
  onDirtyChange?: (isDirty: boolean) => void;
  onFetchLatestFile?: (path: string) => Promise<SourceFileState>;
  onAdoptFileState?: (fileState: SourceFileState) => void;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  onReloadFile?: (path: string) => Promise<SourceFileState | void>;
  onSaveFile: (
    path: string,
    content: string,
    options?: SourceSaveOptions,
  ) => Promise<SourceFileState | void>;
  workspaceRoot?: string | null;
}) {
  const [editorValue, setEditorValue] = useState("");
  const [actionError, setActionError] = useState<string | null>(null);
  const [saveConflictOnDisk, setSaveConflictOnDisk] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isReloading, setIsReloading] = useState(false);
  const [isRebasing, setIsRebasing] = useState(false);
  const [isLoadingCompare, setIsLoadingCompare] = useState(false);
  const [compareDiskContent, setCompareDiskContent] = useState<string | null>(null);
  const [documentMode, setDocumentMode] = useState<SourceDocumentMode>("code");
  const [editorStatus, setEditorStatus] = useState<MonacoCodeEditorStatus>(DEFAULT_EDITOR_STATUS);
  const [copiedPath, setCopiedPath] = useState(false);
  const [hasRenderedMarkdownDraftActive, setHasRenderedMarkdownDraftActive] = useState(false);
  const pendingEditorValueRef = useRef<string | null>(null);
  const preserveActionErrorOnFileStateAdoptionRef = useRef(false);
  const lastAutoRebaseKeyRef = useRef<string | null>(null);
  const mountedRef = useRef(false);
  const rebaseRequestTokenRef = useRef(0);
  const saveRequestTokenRef = useRef(0);
  const reloadRequestTokenRef = useRef(0);
  const compareRequestTokenRef = useRef(0);
  const copyPathRequestTokenRef = useRef(0);
  const fileStateRef = useRef(fileState);
  const editorValueRef = useRef(editorValue);
  const renderedMarkdownDocumentPathRef = useRef(
    fileState.status === "ready" ? fileState.path : null,
  );
  const renderedMarkdownCommittersRef = useRef(
    new Set<() => RenderedMarkdownSectionCommit | null>(),
  );
  const isEditorDirty = fileState.status === "ready" && editorValue !== fileState.content;
  const isDirty = isEditorDirty || hasRenderedMarkdownDraftActive;
  const isMarkdownSource =
    fileState.status === "ready" && isMarkdownDocument(fileState.language, fileState.path);
  // Phase 3 of `docs/features/source-renderers.md`: expose
  // Preview/Split for non-Markdown files too when the renderer
  // registry detects at least one renderable region. Detection runs
  // against the CURRENT edit buffer (not the saved file content) so
  // the preview reflects unsaved edits, matching the Markdown
  // pane's existing behavior. Kept in a useMemo so the expensive
  // scan only reruns when editorValue / path / language change.
  const renderableRegions = useMemo<SourceRenderableRegion[]>(() => {
    if (fileState.status !== "ready") {
      return [];
    }
    return detectRenderableRegions({
      path: fileState.path,
      language: fileState.language,
      content: editorValue,
      mode: "source",
    });
  }, [editorValue, fileState.language, fileState.path, fileState.status]);
  // Show the mode switcher for Markdown files unconditionally (they
  // have Markdown chrome even without renderable regions — headings,
  // tables, etc.) AND for non-Markdown files when at least one
  // renderable region was detected. Dedicated Mermaid files
  // (`.mmd`) surface a whole-file region, so they get the switcher.
  const canShowRendererPreview =
    fileState.status === "ready" && (isMarkdownSource || renderableRegions.length > 0);
  const normalizedEditorValue = useMemo(
    () => normalizeMarkdownDocumentLineEndings(editorValue),
    [editorValue],
  );
  const renderedMarkdownSegment = useMemo<MarkdownDiffDocumentSegment | null>(() => {
    if (!isMarkdownSource || fileState.status !== "ready") {
      return null;
    }

    return {
      afterEndOffset: normalizedEditorValue.length,
      afterStartOffset: 0,
      id: `source-preview:${fileState.path}`,
      isInAfterDocument: true,
      kind: "normal",
      markdown: normalizedEditorValue,
      newStart: 1,
      oldStart: 1,
    };
  }, [
    fileState.path,
    fileState.status,
    isMarkdownSource,
    normalizedEditorValue,
  ]);
  // Inline Monaco view zones: each detected renderable region gets a
  // zone pinned AFTER its last source line, hosting a portal that
  // renders the region's display text via MarkdownContent. Keyed by
  // the region's stable id so React keeps the portal mounted as the
  // user types — a mid-edit fence change shifts the line number
  // (Monaco removes + re-adds the zone) but the diagram DOM node
  // survives, preventing the Mermaid iframe from unmounting and
  // reinitializing on every keystroke. Empty when the registry
  // detects no regions, so non-renderable files pay zero cost.
  const inlineZones = useMemo<MonacoInlineZone[]>(() => {
    if (renderableRegions.length === 0) {
      return [];
    }
    return renderableRegions.map((region) => ({
      id: region.id,
      afterLineNumber: region.sourceEndLine,
      render: () => (
        <MarkdownContent
          appearance={editorAppearance}
          documentPath={fileState.path}
          markdown={composeInlineRegionFence(region)}
          workspaceRoot={workspaceRoot}
        />
      ),
    }));
  }, [editorAppearance, fileState.path, renderableRegions, workspaceRoot]);
  const setEditorValueState = useCallback((nextValue: string) => {
    editorValueRef.current = nextValue;
    setEditorValue(nextValue);
  }, []);

  function beginSourceRequest(requestTokenRef: { current: number }) {
    const requestToken = requestTokenRef.current + 1;
    requestTokenRef.current = requestToken;
    return requestToken;
  }

  function isActiveSourceRequest(
    requestTokenRef: { current: number },
    requestToken: number,
    requestPath?: string,
  ) {
    if (!mountedRef.current || requestTokenRef.current !== requestToken) {
      return false;
    }
    if (requestPath === undefined) {
      return true;
    }

    const currentFileState = fileStateRef.current;
    return currentFileState.status === "ready" && currentFileState.path === requestPath;
  }

  useEffect(() => {
    fileStateRef.current = fileState;
  }, [fileState]);

  useEffect(() => {
    editorValueRef.current = editorValue;
  }, [editorValue]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      rebaseRequestTokenRef.current += 1;
      saveRequestTokenRef.current += 1;
      reloadRequestTokenRef.current += 1;
      compareRequestTokenRef.current += 1;
      copyPathRequestTokenRef.current += 1;
    };
  }, []);

  useLayoutEffect(() => {
    const nextRenderedMarkdownDocumentPath =
      fileState.status === "ready" ? fileState.path : null;
    if (renderedMarkdownDocumentPathRef.current !== nextRenderedMarkdownDocumentPath) {
      renderedMarkdownDocumentPathRef.current = nextRenderedMarkdownDocumentPath;
      renderedMarkdownCommittersRef.current.clear();
    }
  }, [fileState.path, fileState.status]);

  useEffect(() => {
    if (fileState.status === "ready") {
      const pendingEditorValue = pendingEditorValueRef.current;
      const shouldPreserveActionError = preserveActionErrorOnFileStateAdoptionRef.current;
      pendingEditorValueRef.current = null;
      preserveActionErrorOnFileStateAdoptionRef.current = false;
      const nextEditorValue = pendingEditorValue ?? fileState.content;
      setEditorValueState(nextEditorValue);
      if (!shouldPreserveActionError) {
        setActionError(null);
      }
      setSaveConflictOnDisk(false);
      setCompareDiskContent(null);
      setHasRenderedMarkdownDraftActive(shouldPreserveActionError);
      setEditorStatus(createEditorStatusSnapshot(nextEditorValue));
      return;
    }

    if (fileState.status !== "loading") {
      setEditorValueState("");
      setHasRenderedMarkdownDraftActive(false);
      setEditorStatus(DEFAULT_EDITOR_STATUS);
    }
  }, [
    fileState.content,
    fileState.contentHash,
    fileState.path,
    fileState.status,
  ]);

  useEffect(() => {
    onDirtyChange?.(isDirty);
  }, [isDirty, onDirtyChange]);

  useEffect(() => {
    // Force back to code view when the renderer-preview gate closes
    // (file was renderable but user's edits removed all regions, or
    // a tab switched to a plain-prose non-Markdown file).
    if (!canShowRendererPreview && documentMode !== "code") {
      setDocumentMode("code");
    }
  }, [canShowRendererPreview, documentMode]);

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

  const registerRenderedMarkdownCommitter = useCallback(
    (committer: () => RenderedMarkdownSectionCommit | null) => {
      renderedMarkdownCommittersRef.current.add(committer);
      return () => {
        renderedMarkdownCommittersRef.current.delete(committer);
      };
    },
    [],
  );

  const collectRenderedMarkdownCommits = useCallback(() => {
    return Array.from(renderedMarkdownCommittersRef.current)
      .map((committer) => committer())
      .filter((commit): commit is RenderedMarkdownSectionCommit => commit != null);
  }, []);

  const handleRenderedMarkdownSectionCommits = useCallback(
    (commits: RenderedMarkdownSectionCommit[]) => {
      const currentFileState = fileStateRef.current;
      if (!isMarkdownSource || currentFileState.status !== "ready") {
        return false;
      }

      const rawSourceContent = editorValueRef.current;
      const originalEolStyle = detectMarkdownDocumentEolStyle(rawSourceContent);
      const sourceContent =
        normalizeMarkdownDocumentLineEndings(rawSourceContent);
      const resolvedCommits = commits.map((commit) => ({
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
      const hasOverlappingRange =
        hasOverlappingMarkdownCommitRanges(validResolvedCommits);
      if (unresolvedCommitCount > 0 || hasOverlappingRange) {
        setActionError(
          "Rendered Markdown edit could not be applied because the document changed under that section. Review the latest file and edit again.",
        );
        return false;
      }

      const nextDocumentContentLf = validResolvedCommits
        .sort((left, right) => right.range.start - left.range.start)
        .reduce(
          (currentContent, { commit, range }) =>
            replaceMarkdownDocumentRange(
              currentContent,
              range.start,
              range.end,
              normalizeEditedMarkdownSection(
                commit.nextMarkdown,
                commit.segment.markdown,
              ),
            ),
          sourceContent,
        );
      if (nextDocumentContentLf === sourceContent) {
        commits.forEach((commit) =>
          commit.onApplied?.({ resetRenderedContent: false }),
        );
        setHasRenderedMarkdownDraftActive(false);
        return true;
      }

      const nextDocumentContent = applyMarkdownDocumentEolStyle(
        nextDocumentContentLf,
        originalEolStyle,
      );
      setHasRenderedMarkdownDraftActive(false);
      setEditorValueState(nextDocumentContent);
      setEditorStatus(createEditorStatusSnapshot(nextDocumentContent));
      setActionError(null);
      setSaveConflictOnDisk(false);
      commits.forEach((commit) => commit.onApplied?.());
      return true;
    },
    [isMarkdownSource],
  );

  const commitRenderedMarkdownDrafts = useCallback((): boolean => {
    const commits = collectRenderedMarkdownCommits();
    if (commits.length === 0) {
      setHasRenderedMarkdownDraftActive(false);
      return true;
    }

    return handleRenderedMarkdownSectionCommits(commits);
  }, [collectRenderedMarkdownCommits, handleRenderedMarkdownSectionCommits]);

  const commitRenderedMarkdownSectionDraft = useCallback(
    (commit: RenderedMarkdownSectionCommit) => {
      return handleRenderedMarkdownSectionCommits([commit]);
    },
    [handleRenderedMarkdownSectionCommits],
  );

  const handleRenderedMarkdownSectionDraftChange = useCallback(
    (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => {
      if (!isMarkdownSource || fileStateRef.current.status !== "ready") {
        return;
      }

      const normalizedDraft = normalizeEditedMarkdownSection(
        nextMarkdown,
        segment.markdown,
      );
      const nextHasDraft = normalizedDraft !== segment.markdown;
      setHasRenderedMarkdownDraftActive(nextHasDraft);
      setActionError(null);
      setSaveConflictOnDisk(false);
    },
    [isMarkdownSource],
  );

  const handleRenderedMarkdownReadOnlyMutation = useCallback(() => {
    // SourcePanel Markdown preview is editable; this is only required by
    // the shared rendered-Markdown section API.
  }, []);

  const handleSelectDocumentMode = useCallback(
    (nextMode: SourceDocumentMode) => {
      if (documentMode === nextMode) {
        return;
      }
      if (!commitRenderedMarkdownDrafts()) {
        return;
      }
      setDocumentMode(nextMode);
    },
    [commitRenderedMarkdownDrafts, documentMode],
  );

  async function handleSave(options?: SourceSaveOptions) {
    if (!commitRenderedMarkdownDrafts()) {
      return;
    }

    const currentFileState = fileStateRef.current;
    const currentEditorValue = editorValueRef.current;
    const canRestoreDeletedFile =
      options?.overwrite && currentFileState.status === "ready" && fileDeletedOnDisk;
    const isCurrentDirty =
      currentFileState.status === "ready" &&
      currentEditorValue !== currentFileState.content;
    if (
      currentFileState.status !== "ready" ||
      (!isCurrentDirty && !canRestoreDeletedFile) ||
      isSaving
    ) {
      return;
    }

    setIsSaving(true);
    setActionError(null);
    setSaveConflictOnDisk(false);
    const savePath = currentFileState.path;
    const requestToken = beginSourceRequest(saveRequestTokenRef);
    try {
      const savedFileState = await onSaveFile(savePath, currentEditorValue, options);
      if (
        savedFileState &&
        isActiveSourceRequest(saveRequestTokenRef, requestToken, savePath)
      ) {
        const draftsApplied = commitRenderedMarkdownDrafts();
        const latestEditorValue = editorValueRef.current;
        if (latestEditorValue !== currentEditorValue) {
          pendingEditorValueRef.current = latestEditorValue;
        }
        if (!draftsApplied && onAdoptFileState) {
          preserveActionErrorOnFileStateAdoptionRef.current = true;
        }
        onAdoptFileState?.(savedFileState);
      }
    } catch (error) {
      if (!isActiveSourceRequest(saveRequestTokenRef, requestToken, savePath)) {
        return;
      }
      const message = getErrorMessage(error);
      setActionError(message);
      if (isStaleFileSaveError(message)) {
        setSaveConflictOnDisk(true);
      }
    } finally {
      if (isActiveSourceRequest(saveRequestTokenRef, requestToken)) {
        setIsSaving(false);
      }
    }
  }

  const handleEditorChange = useCallback((nextValue: string) => {
    setEditorValueState(nextValue);
  }, [setEditorValueState]);

  async function handleReloadFromDisk() {
    if (fileState.status !== "ready" || !onReloadFile || isReloading) {
      return;
    }

    setIsReloading(true);
    setActionError(null);
    const reloadPath = fileState.path;
    const requestToken = beginSourceRequest(reloadRequestTokenRef);
    try {
      const reloadedFileState = await onReloadFile(reloadPath);
      if (
        reloadedFileState &&
        isActiveSourceRequest(reloadRequestTokenRef, requestToken, reloadPath)
      ) {
        onAdoptFileState?.(reloadedFileState);
      }
    } catch (error) {
      if (isActiveSourceRequest(reloadRequestTokenRef, requestToken, reloadPath)) {
        setActionError(getErrorMessage(error));
      }
    } finally {
      if (isActiveSourceRequest(reloadRequestTokenRef, requestToken)) {
        setIsReloading(false);
      }
    }
  }

  async function handleApplyLocalEditsToDiskVersion() {
    if (!commitRenderedMarkdownDrafts()) {
      return;
    }

    const currentFileState = fileStateRef.current;
    const currentEditorValue = editorValueRef.current;
    if (
      currentFileState.status !== "ready" ||
      currentEditorValue === currentFileState.content ||
      !onFetchLatestFile ||
      !onAdoptFileState ||
      isRebasing
    ) {
      return;
    }

    setIsRebasing(true);
    setActionError(null);
    const requestToken = rebaseRequestTokenRef.current + 1;
    rebaseRequestTokenRef.current = requestToken;
    try {
      const latestFileState = await onFetchLatestFile(currentFileState.path);
      if (!mountedRef.current || rebaseRequestTokenRef.current !== requestToken) {
        return;
      }
      const latestEditorSnapshot = editorValueRef.current;
      const latestFileSnapshot = fileStateRef.current;
      if (
        latestFileSnapshot.status !== "ready" ||
        latestFileSnapshot.path !== currentFileState.path
      ) {
        return;
      }

      if (latestEditorSnapshot === latestFileSnapshot.content) {
        onAdoptFileState(latestFileState);
        return;
      }

      const rebaseResult = rebaseContentOntoDisk(
        latestFileSnapshot.content,
        latestEditorSnapshot,
        latestFileState.content,
      );
      if (rebaseResult.status === "conflict") {
        setActionError(rebaseResult.reason);
        return;
      }

      pendingEditorValueRef.current = rebaseResult.content;
      onAdoptFileState(latestFileState);
      setEditorValueState(rebaseResult.content);
    } catch (error) {
      if (mountedRef.current && rebaseRequestTokenRef.current === requestToken) {
        setActionError(getErrorMessage(error));
      }
    } finally {
      if (mountedRef.current && rebaseRequestTokenRef.current === requestToken) {
        setIsRebasing(false);
      }
    }
  }

  async function handleShowCompare() {
    if (!commitRenderedMarkdownDrafts()) {
      return;
    }

    if (
      fileState.status !== "ready" ||
      !onFetchLatestFile ||
      isLoadingCompare
    ) {
      return;
    }

    setIsLoadingCompare(true);
    setActionError(null);
    const comparePath = fileState.path;
    const requestToken = beginSourceRequest(compareRequestTokenRef);
    try {
      const latestFileState = await onFetchLatestFile(comparePath);
      if (isActiveSourceRequest(compareRequestTokenRef, requestToken, comparePath)) {
        setCompareDiskContent(latestFileState.content);
      }
    } catch (error) {
      if (isActiveSourceRequest(compareRequestTokenRef, requestToken, comparePath)) {
        setActionError(getErrorMessage(error));
      }
    } finally {
      if (isActiveSourceRequest(compareRequestTokenRef, requestToken)) {
        setIsLoadingCompare(false);
      }
    }
  }

  async function handleCopyPath() {
    if (!displayPath) {
      return;
    }

    const copiedDisplayPath = displayPath;
    const requestToken = beginSourceRequest(copyPathRequestTokenRef);
    try {
      await copyTextToClipboard(copiedDisplayPath);
      const currentDisplayPath = fileStateRef.current.path.trim() || sourcePath?.trim() || "";
      if (
        isActiveSourceRequest(copyPathRequestTokenRef, requestToken) &&
        currentDisplayPath === copiedDisplayPath
      ) {
        setCopiedPath(true);
      }
    } catch {
      const currentDisplayPath = fileStateRef.current.path.trim() || sourcePath?.trim() || "";
      if (
        isActiveSourceRequest(copyPathRequestTokenRef, requestToken) &&
        currentDisplayPath === copiedDisplayPath
      ) {
        setCopiedPath(false);
      }
    }
  }

  const saveStateLabel = actionError
    ? "Action failed"
    : isSaving
      ? "Saving..."
      : isRebasing
        ? "Applying edits..."
        : fileState.status === "ready" && fileState.staleOnDisk
          ? fileState.externalChangeKind === "deleted"
            ? "Deleted on disk"
            : "Changed on disk"
          : isDirty
            ? "Unsaved changes"
            : null;
  const displayPath = fileState.path.trim() || sourcePath?.trim() || "";
  const activeInstructionPath = displayPath;
  const canCopyPath = displayPath.length > 0;
  const canDebugInstructions =
    typeof onOpenInstructionDebugger === "function" &&
    isInstructionLikePath(activeInstructionPath);
  const hasExternalDiskChange =
    fileState.status === "ready" &&
    (fileState.staleOnDisk || saveConflictOnDisk);
  const externalChangeKind =
    fileState.status === "ready" ? fileState.externalChangeKind ?? null : null;
  const fileDeletedOnDisk =
    hasExternalDiskChange && externalChangeKind === "deleted";
  const externalChangeKey =
    fileState.status === "ready" && fileState.staleOnDisk
      ? [
          fileState.path,
          fileState.externalChangeKind ?? "",
          fileState.contentHash ?? "",
          fileState.externalContentHash ?? "",
        ].join("\0")
      : "";

  useEffect(() => {
    if (
      !externalChangeKey ||
      fileDeletedOnDisk ||
      !isDirty ||
      isRebasing ||
      !onFetchLatestFile ||
      !onAdoptFileState ||
      lastAutoRebaseKeyRef.current === externalChangeKey
    ) {
      return;
    }

    lastAutoRebaseKeyRef.current = externalChangeKey;
    void handleApplyLocalEditsToDiskVersion();
  }, [
    externalChangeKey,
    fileDeletedOnDisk,
    isDirty,
    isRebasing,
    onAdoptFileState,
    onFetchLatestFile,
  ]);

  return (
    <div className={`source-pane${fileState.status === "ready" ? " has-editor" : ""}`}>
      <div className="source-toolbar">
        <div className="source-path-row source-path-display-row">
          <div
            className="source-path-display"
            role={fileState.status === "loading" ? "status" : undefined}
            aria-label={fileState.status === "loading" ? "Loading source file" : undefined}
            aria-live={fileState.status === "loading" ? "polite" : undefined}
            title={displayPath || undefined}
          >
            <span className="source-path-display-text">{displayPath || "No source file selected"}</span>
            {fileState.status === "loading" ? (
              <span className="activity-spinner source-path-loading-spinner" aria-hidden="true" />
            ) : null}
          </div>
          {(canCopyPath || canDebugInstructions) ? (
            <div className="source-path-actions">
              {canCopyPath ? (
                <button
                  className={`command-icon-button source-path-copy-button${copiedPath ? " copied" : ""}`}
                  type="button"
                  onClick={() => void handleCopyPath()}
                  aria-label={copiedPath ? "Path copied" : "Copy path"}
                  title={copiedPath ? "Copied" : "Copy path"}
                >
                  {copiedPath ? <CheckIcon /> : <CopyIcon />}
                </button>
              ) : null}
              {canDebugInstructions ? (
                <button
                  className="ghost-button source-toolbar-action"
                  type="button"
                  onClick={onOpenInstructionDebugger}
                >
                  Debug instructions
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
        {canShowRendererPreview && compareDiskContent === null ? (
          <div
            className="source-editor-toolbar source-document-mode-toolbar"
            aria-label="Document view mode"
          >
            <div className="source-editor-status">
              <span className="chip">
                {isMarkdownSource
                  ? "Markdown"
                  : describeRenderableKinds(renderableRegions)}
              </span>
            </div>
            <div className="source-editor-actions">
              <SourceDocumentModeButton
                label="Code"
                selected={documentMode === "code"}
                onClick={() => handleSelectDocumentMode("code")}
              />
              <SourceDocumentModeButton
                label="Preview"
                selected={documentMode === "preview"}
                onClick={() => handleSelectDocumentMode("preview")}
              />
              <SourceDocumentModeButton
                label="Split"
                selected={documentMode === "split"}
                onClick={() => handleSelectDocumentMode("split")}
              />
            </div>
          </div>
        ) : null}
      </div>

      {fileState.status === "idle" ? (
        <EmptyState
          title="No source file selected"
          body="Open a file from the workspace to show its source in this tile."
        />
      ) : null}

      {fileState.status === "error" ? (
        <article className="thread-notice">
          <div className="card-label">Source</div>
          <p>{fileState.error}</p>
        </article>
      ) : null}

      {actionError ? (
        <article className="thread-notice">
          <div className="card-label">Action failed</div>
          <p>{actionError}</p>
        </article>
      ) : null}

      {hasExternalDiskChange ? (
        <article className="thread-notice source-file-change-notice">
          <div className="card-label">
            {fileDeletedOnDisk ? "File deleted on disk" : "File changed on disk"}
          </div>
          {fileDeletedOnDisk ? (
            <p>
              Another process deleted this file after you opened it. Your editor
              buffer is still preserved here; restore it to the same path if you
              want to recreate the file.
            </p>
          ) : (
            <p>
              Another process changed this file after you opened it. You can try
              applying your local edits on top of the disk version, reload from
              disk, or intentionally overwrite the disk file.
            </p>
          )}
          <div className="source-file-change-actions">
            {isDirty && !fileDeletedOnDisk ? (
              <button
                className="ghost-button"
                type="button"
                disabled={!onFetchLatestFile || isLoadingCompare}
                onClick={() => void handleShowCompare()}
              >
                {isLoadingCompare ? "Loading compare..." : "Compare"}
              </button>
            ) : null}
            {isDirty && !fileDeletedOnDisk ? (
              <button
                className="ghost-button"
                type="button"
                disabled={!onFetchLatestFile || !onAdoptFileState || isRebasing}
                onClick={() => void handleApplyLocalEditsToDiskVersion()}
              >
                {isRebasing ? "Applying..." : "Apply my edits to disk version"}
              </button>
            ) : null}
            {isDirty || fileDeletedOnDisk ? (
              <button
                className="ghost-button"
                type="button"
                disabled={isSaving}
                onClick={() => void handleSave({ overwrite: true })}
              >
                {isSaving
                  ? "Saving..."
                  : fileDeletedOnDisk
                    ? "Restore file"
                    : "Save anyway"}
              </button>
            ) : null}
            {!fileDeletedOnDisk ? (
              <button
                className="ghost-button"
                type="button"
                disabled={!onReloadFile || isReloading}
                onClick={() => void handleReloadFromDisk()}
              >
                {isReloading ? "Reloading..." : "Reload from disk"}
              </button>
            ) : null}
          </div>
        </article>
      ) : null}

      {fileState.status === "ready" ? (
        <div className="source-editor-region">
          <div className="source-editor-shell source-editor-shell-with-statusbar">
            {compareDiskContent !== null ? (
              <>
                <div className="source-compare-toolbar">
                  <span>Comparing disk version to your editor buffer</span>
                  <button
                    className="ghost-button source-compare-close"
                    type="button"
                    onClick={() => setCompareDiskContent(null)}
                  >
                    Back to edit
                  </button>
                </div>
                <Suspense fallback={<div className="source-editor-loading">Loading compare...</div>}>
                  <MonacoDiffEditor
                    appearance={editorAppearance}
                    ariaLabel={`Source compare for ${fileState.path}`}
                    fontSizePx={editorFontSizePx}
                    language={fileState.language}
                    modifiedValue={editorValue}
                    originalValue={compareDiskContent}
                    path={fileState.path}
                    readOnly
                  />
                </Suspense>
              </>
            ) : canShowRendererPreview && documentMode === "preview" ? (
              isMarkdownSource && renderedMarkdownSegment ? (
                <EditableMarkdownPreviewPane
                  appearance={editorAppearance}
                  documentPath={fileState.path}
                  editableAriaLabel={`Edit rendered Markdown preview for ${fileState.path}`}
                  onCommitDrafts={commitRenderedMarkdownDrafts}
                  onCommitSectionDraft={commitRenderedMarkdownSectionDraft}
                  onDraftChange={handleRenderedMarkdownSectionDraftChange}
                  onOpenSourceLink={onOpenSourceLink}
                  onReadOnlyMutation={handleRenderedMarkdownReadOnlyMutation}
                  onRegisterCommitter={registerRenderedMarkdownCommitter}
                  onSave={() => handleSave()}
                  segment={renderedMarkdownSegment}
                  sourceContent={normalizedEditorValue}
                  workspaceRoot={workspaceRoot}
                />
              ) : (
                <RendererPreviewPane
                  appearance={editorAppearance}
                  content={editorValue}
                  documentPath={fileState.path}
                  isMarkdownSource={isMarkdownSource}
                  onOpenSourceLink={onOpenSourceLink}
                  renderableRegions={renderableRegions}
                  workspaceRoot={workspaceRoot}
                />
              )
            ) : canShowRendererPreview && documentMode === "split" ? (
              <div className="source-markdown-split">
                <div className="source-markdown-split-pane source-markdown-editor-pane">
                  <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
                    <MonacoCodeEditor
                      appearance={editorAppearance}
                      ariaLabel={`Source editor for ${fileState.path}`}
                      fontSizePx={editorFontSizePx}
                      highlightedColumnNumber={sourceFocus?.column ?? null}
                      highlightedLineNumber={sourceFocus?.line ?? null}
                      highlightToken={sourceFocus?.token ?? null}
                      language={fileState.language}
                      path={fileState.path}
                      value={editorValue}
                      onChange={handleEditorChange}
                      onSave={() => void handleSave()}
                      onStatusChange={setEditorStatus}
                    />
                  </Suspense>
                </div>
                <div className="source-markdown-split-pane">
                  {isMarkdownSource && renderedMarkdownSegment ? (
                    <EditableMarkdownPreviewPane
                      appearance={editorAppearance}
                      documentPath={fileState.path}
                      editableAriaLabel={`Edit rendered Markdown preview for ${fileState.path}`}
                      onCommitDrafts={commitRenderedMarkdownDrafts}
                      onCommitSectionDraft={commitRenderedMarkdownSectionDraft}
                      onDraftChange={handleRenderedMarkdownSectionDraftChange}
                      onOpenSourceLink={onOpenSourceLink}
                      onReadOnlyMutation={handleRenderedMarkdownReadOnlyMutation}
                      onRegisterCommitter={registerRenderedMarkdownCommitter}
                      onSave={() => handleSave()}
                      segment={renderedMarkdownSegment}
                      sourceContent={normalizedEditorValue}
                      workspaceRoot={workspaceRoot}
                    />
                  ) : (
                    <RendererPreviewPane
                      appearance={editorAppearance}
                      content={editorValue}
                      documentPath={fileState.path}
                      isMarkdownSource={isMarkdownSource}
                      onOpenSourceLink={onOpenSourceLink}
                      renderableRegions={renderableRegions}
                      workspaceRoot={workspaceRoot}
                    />
                  )}
                </div>
              </div>
            ) : (
              <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
                <MonacoCodeEditor
                  appearance={editorAppearance}
                  ariaLabel={`Source editor for ${fileState.path}`}
                  fontSizePx={editorFontSizePx}
                  highlightedColumnNumber={sourceFocus?.column ?? null}
                  highlightedLineNumber={sourceFocus?.line ?? null}
                  highlightToken={sourceFocus?.token ?? null}
                  inlineZones={inlineZones}
                  language={fileState.language}
                  path={fileState.path}
                  value={editorValue}
                  onChange={handleEditorChange}
                  onSave={() => void handleSave()}
                  onStatusChange={setEditorStatus}
                />
              </Suspense>
            )}
            <footer className="source-editor-statusbar" aria-label="Editor status">
              <div className="source-editor-statusbar-group">
                {saveStateLabel ? <span className="source-editor-statusbar-item source-editor-statusbar-state">{saveStateLabel}</span> : null}
              </div>
              <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
                <span className="source-editor-statusbar-item">{`Ln ${editorStatus.line}, Col ${editorStatus.column}`}</span>
                <span className="source-editor-statusbar-item">{formatIndentationLabel(editorStatus)}</span>
                <span className="source-editor-statusbar-item">UTF-8</span>
                <span className="source-editor-statusbar-item">{editorStatus.endOfLine}</span>
                <span className="source-editor-statusbar-item">{formatLanguageLabel(fileState.language, fileState.path)}</span>
              </div>
            </footer>
          </div>
        </div>
      ) : null}
    </div>
  );
}

const noopOpenSourceLink = (_target: MarkdownFileLinkTarget) => {};

function EditableMarkdownPreviewPane({
  appearance,
  documentPath,
  editableAriaLabel,
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
  appearance: MonacoAppearance;
  documentPath: string | null;
  editableAriaLabel: string;
  onCommitDrafts: () => boolean;
  onCommitSectionDraft: (commit: RenderedMarkdownSectionCommit) => boolean;
  onDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  onReadOnlyMutation: () => void;
  onRegisterCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onSave: () => Promise<void> | void;
  segment: MarkdownDiffDocumentSegment;
  sourceContent: string;
  workspaceRoot: string | null;
}) {
  return (
    <div className="source-renderer-preview source-renderer-preview-editable" aria-label="Rendered preview">
      <EditableRenderedMarkdownSection
        allowReadOnlyCaret={false}
        allowCurrentSegmentFallback={false}
        appearance={appearance}
        canEdit
        className="source-rendered-markdown-section markdown-diff-normal-section"
        documentPath={documentPath}
        editableAriaLabel={editableAriaLabel}
        fillMermaidAvailableSpace
        onCommitDrafts={onCommitDrafts}
        onCommitSectionDraft={onCommitSectionDraft}
        onDraftChange={onDraftChange}
        onOpenSourceLink={onOpenSourceLink ?? noopOpenSourceLink}
        onReadOnlyMutation={onReadOnlyMutation}
        onRegisterCommitter={onRegisterCommitter}
        onSave={onSave}
        resetOnSegmentMarkdownChange={false}
        segment={segment}
        sourceContent={sourceContent}
        workspaceRoot={workspaceRoot}
      />
    </div>
  );
}

function isStaleFileSaveError(message: string) {
  return message.toLowerCase().includes("file changed on disk before save");
}

function SourceDocumentModeButton({
  label,
  onClick,
  selected,
}: {
  label: string;
  onClick: () => void;
  selected: boolean;
}) {
  return (
    <button
      className={`ghost-button source-document-mode-button${selected ? " selected" : ""}`}
      type="button"
      aria-pressed={selected}
      onClick={onClick}
    >
      {label}
    </button>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state-card">
      <div className="card-label">Workspace</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function createEditorStatusSnapshot(content: string): MonacoCodeEditorStatus {
  return {
    ...DEFAULT_EDITOR_STATUS,
    endOfLine: content.includes("\r\n") ? "CRLF" : "LF",
  };
}

function formatIndentationLabel(status: MonacoCodeEditorStatus) {
  return status.insertSpaces ? `Spaces: ${status.tabSize}` : `Tab Size: ${status.tabSize}`;
}

function formatLanguageLabel(language: string | null, path: string) {
  const resolved = resolveMonacoLanguage(language, path);
  const normalizedPath = path.trim().toLowerCase();
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

function isInstructionLikePath(path: string) {
  const normalized = path.trim().toLowerCase().replace(/[\\/]+/g, "/");
  if (!normalized) {
    return false;
  }

  return (
    normalized.endsWith("/agents.md") ||
    normalized.endsWith("/claude.md") ||
    normalized.endsWith("/gemini.md") ||
    normalized.endsWith("/rules.md") ||
    normalized.endsWith("/skills.md") ||
    normalized.endsWith("/skill.md") ||
    normalized.endsWith("/agent.md") ||
    normalized.endsWith("/.claude.md") ||
    normalized.includes("/.claude/commands/") ||
    normalized.includes("/.claude/reviewers/") ||
    normalized.includes("/.cursor/rules/") ||
    normalized.includes("/skills/") ||
    normalized.includes("/rules/")
  );
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}

function CopyIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="5" y="3" width="8" height="10" rx="1.5" />
      <path d="M3 11V5.5C3 4.67 3.67 4 4.5 4H10" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M3.5 8.5 6.5 11.5 12.5 4.5" />
    </svg>
  );
}
