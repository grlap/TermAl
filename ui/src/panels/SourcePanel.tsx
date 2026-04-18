import { Suspense, lazy, useEffect, useMemo, useRef, useState } from "react";
import { copyTextToClipboard } from "../clipboard";
import { MarkdownDocumentView } from "../MarkdownDocumentView";
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

const MAX_REBASE_DIFF_CELLS = 4_000_000;

type SourceDocumentMode = "code" | "preview" | "split";

export type SourceFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  contentHash?: string | null;
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

export type ContentRebaseResult =
  | { status: "clean"; content: string }
  | { status: "conflict"; reason: string };

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
  onReloadFile?: (path: string) => Promise<void>;
  onSaveFile: (
    path: string,
    content: string,
    options?: SourceSaveOptions,
  ) => Promise<void>;
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
  const pendingEditorValueRef = useRef<string | null>(null);
  const lastAutoRebaseKeyRef = useRef<string | null>(null);
  const mountedRef = useRef(false);
  const rebaseRequestTokenRef = useRef(0);
  const fileStateRef = useRef(fileState);
  const editorValueRef = useRef(editorValue);
  const isDirty = fileState.status === "ready" && editorValue !== fileState.content;
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
  const setEditorValueState = (nextValue: string) => {
    editorValueRef.current = nextValue;
    setEditorValue(nextValue);
  };

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
    };
  }, []);

  useEffect(() => {
    if (fileState.status === "ready") {
      const pendingEditorValue = pendingEditorValueRef.current;
      pendingEditorValueRef.current = null;
      const nextEditorValue = pendingEditorValue ?? fileState.content;
      setEditorValueState(nextEditorValue);
      setActionError(null);
      setSaveConflictOnDisk(false);
      setCompareDiskContent(null);
      setEditorStatus(createEditorStatusSnapshot(nextEditorValue));
      return;
    }

    if (fileState.status !== "loading") {
      setEditorValueState("");
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

  async function handleSave(options?: SourceSaveOptions) {
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
    try {
      await onSaveFile(currentFileState.path, currentEditorValue, options);
    } catch (error) {
      const message = getErrorMessage(error);
      setActionError(message);
      if (isStaleFileSaveError(message)) {
        setSaveConflictOnDisk(true);
      }
    } finally {
      setIsSaving(false);
    }
  }

  function handleEditorChange(nextValue: string) {
    setEditorValueState(nextValue);
    onDirtyChange?.(
      fileState.status === "ready" && nextValue !== fileState.content,
    );
  }

  async function handleReloadFromDisk() {
    if (fileState.status !== "ready" || !onReloadFile || isReloading) {
      return;
    }

    setIsReloading(true);
    setActionError(null);
    try {
      await onReloadFile(fileState.path);
    } catch (error) {
      setActionError(getErrorMessage(error));
    } finally {
      setIsReloading(false);
    }
  }

  async function handleApplyLocalEditsToDiskVersion() {
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
        onDirtyChange?.(false);
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
      onDirtyChange?.(rebaseResult.content !== latestFileState.content);
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
    if (
      fileState.status !== "ready" ||
      !onFetchLatestFile ||
      isLoadingCompare
    ) {
      return;
    }

    setIsLoadingCompare(true);
    setActionError(null);
    try {
      const latestFileState = await onFetchLatestFile(fileState.path);
      setCompareDiskContent(latestFileState.content);
    } catch (error) {
      setActionError(getErrorMessage(error));
    } finally {
      setIsLoadingCompare(false);
    }
  }

  async function handleCopyPath() {
    if (!displayPath) {
      return;
    }

    try {
      await copyTextToClipboard(displayPath);
      setCopiedPath(true);
    } catch {
      setCopiedPath(false);
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
                onClick={() => setDocumentMode("code")}
              />
              <SourceDocumentModeButton
                label="Preview"
                selected={documentMode === "preview"}
                onClick={() => setDocumentMode("preview")}
              />
              <SourceDocumentModeButton
                label="Split"
                selected={documentMode === "split"}
                onClick={() => setDocumentMode("split")}
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
              <RendererPreviewPane
                appearance={editorAppearance}
                content={editorValue}
                documentPath={fileState.path}
                isMarkdownSource={isMarkdownSource}
                onOpenSourceLink={onOpenSourceLink}
                renderableRegions={renderableRegions}
                workspaceRoot={workspaceRoot}
              />
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
                  <RendererPreviewPane
                    appearance={editorAppearance}
                    content={editorValue}
                    documentPath={fileState.path}
                    isMarkdownSource={isMarkdownSource}
                    onOpenSourceLink={onOpenSourceLink}
                    renderableRegions={renderableRegions}
                    workspaceRoot={workspaceRoot}
                  />
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

type LineDiffRange = {
  start: number;
  end: number;
  replacement: string[];
};

export function rebaseContentOntoDisk(
  baseContent: string,
  localContent: string,
  diskContent: string,
): ContentRebaseResult {
  // Be conservative here: false conflicts are recoverable, silent edit loss is not.
  const baseLines = splitContentLines(baseContent);
  const localLines = splitContentLines(localContent);
  const diskLines = splitContentLines(diskContent);
  const localRanges = diffLineRanges(baseLines, localLines);
  const diskRanges = diffLineRanges(baseLines, diskLines);

  if (!localRanges || !diskRanges) {
    return {
      status: "conflict",
      reason:
        "Could not apply edits automatically because the file is too large to merge safely.",
    };
  }

  for (const localRange of localRanges) {
    for (const diskRange of diskRanges) {
      if (!lineDiffRangesConflict(localRange, diskRange)) {
        continue;
      }

      return {
        status: "conflict",
        reason:
          "Could not apply your edits cleanly because they overlap with disk changes.",
      };
    }
  }

  const mergedRanges = dedupeEquivalentRanges([...diskRanges, ...localRanges]);
  mergedRanges.sort((left, right) => {
    const startOrder = left.start - right.start;
    if (startOrder !== 0) {
      return startOrder;
    }
    return left.end - right.end;
  });

  const mergedLines: string[] = [];
  let cursor = 0;
  for (const range of mergedRanges) {
    if (range.start < cursor) {
      return {
        status: "conflict",
        reason:
          "Could not apply your edits cleanly because the merged ranges overlap.",
      };
    }

    mergedLines.push(...baseLines.slice(cursor, range.start));
    mergedLines.push(...range.replacement);
    cursor = range.end;
  }
  mergedLines.push(...baseLines.slice(cursor));

  return {
    status: "clean",
    content: mergedLines.join(""),
  };
}

function splitContentLines(content: string) {
  return content.match(/[^\n]*\n|[^\n]+/g) ?? [];
}

function diffLineRanges(
  baseLines: string[],
  changedLines: string[],
): LineDiffRange[] | null {
  const anchors = buildLineLcsAnchors(baseLines, changedLines);
  if (!anchors) {
    return null;
  }

  const ranges: LineDiffRange[] = [];
  let baseCursor = 0;
  let changedCursor = 0;

  for (const anchor of anchors) {
    if (anchor.baseIndex > baseCursor || anchor.changedIndex > changedCursor) {
      ranges.push({
        start: baseCursor,
        end: anchor.baseIndex,
        replacement: changedLines.slice(changedCursor, anchor.changedIndex),
      });
    }
    baseCursor = anchor.baseIndex + 1;
    changedCursor = anchor.changedIndex + 1;
  }

  if (baseCursor < baseLines.length || changedCursor < changedLines.length) {
    ranges.push({
      start: baseCursor,
      end: baseLines.length,
      replacement: changedLines.slice(changedCursor),
    });
  }

  return ranges.filter(
    (range) => range.start !== range.end || range.replacement.length > 0,
  );
}

function buildLineLcsAnchors(baseLines: string[], changedLines: string[]) {
  const rows = baseLines.length + 1;
  const columns = changedLines.length + 1;
  if (rows * columns > MAX_REBASE_DIFF_CELLS) {
    return null;
  }

  const lengths = new Uint32Array(rows * columns);
  const indexFor = (row: number, column: number) => row * columns + column;

  for (let row = baseLines.length - 1; row >= 0; row -= 1) {
    for (let column = changedLines.length - 1; column >= 0; column -= 1) {
      lengths[indexFor(row, column)] =
        baseLines[row] === changedLines[column]
          ? lengths[indexFor(row + 1, column + 1)] + 1
          : Math.max(
              lengths[indexFor(row + 1, column)],
              lengths[indexFor(row, column + 1)],
            );
    }
  }

  const anchors: Array<{ baseIndex: number; changedIndex: number }> = [];
  let row = 0;
  let column = 0;
  while (row < baseLines.length && column < changedLines.length) {
    if (baseLines[row] === changedLines[column]) {
      anchors.push({ baseIndex: row, changedIndex: column });
      row += 1;
      column += 1;
    } else if (
      lengths[indexFor(row + 1, column)] >=
      lengths[indexFor(row, column + 1)]
    ) {
      row += 1;
    } else {
      column += 1;
    }
  }

  return anchors;
}

function lineDiffRangesConflict(left: LineDiffRange, right: LineDiffRange) {
  const leftMatchesRight =
    left.start === right.start &&
    left.end === right.end &&
    lineArraysEqual(left.replacement, right.replacement);
  if (leftMatchesRight) {
    return false;
  }

  const bothInsertAtSamePosition =
    left.start === left.end &&
    right.start === right.end &&
    left.start === right.start;
  if (bothInsertAtSamePosition) {
    return true;
  }

  return left.start < right.end && right.start < left.end;
}

function dedupeEquivalentRanges(ranges: LineDiffRange[]) {
  const deduped: LineDiffRange[] = [];
  for (const range of ranges) {
    if (
      deduped.some(
        (current) =>
          current.start === range.start &&
          current.end === range.end &&
          lineArraysEqual(current.replacement, range.replacement),
      )
    ) {
      continue;
    }
    deduped.push(range);
  }
  return deduped;
}

function lineArraysEqual(left: string[], right: string[]) {
  return (
    left.length === right.length &&
    left.every((line, index) => line === right[index])
  );
}

function isStaleFileSaveError(message: string) {
  return message.toLowerCase().includes("file changed on disk before save");
}

// Builds the synthetic Markdown fence that a Monaco view zone uses
// to render a single region inline. The Source panel (Code + Split
// modes) passes this into `MonacoCodeEditor`'s `inlineZones` prop;
// the Monaco view-zone machinery portals a `<MarkdownContent>` into
// a zone positioned after the region's last source line, reusing
// the Phase 1 Mermaid/KaTeX pipeline without a second renderer.
// Markdown-renderer regions (future Phase 6+) pass their body
// through as-is so `MarkdownContent` parses it.
function composeInlineRegionFence(region: SourceRenderableRegion): string {
  if (region.renderer === "mermaid") {
    return "```mermaid\n" + region.displayText.replace(/\s+$/, "") + "\n```";
  }
  if (region.renderer === "math") {
    return "$$\n" + region.displayText.replace(/\s+$/, "") + "\n$$";
  }
  return region.displayText;
}

// Preview pane for source files that have at least one renderable
// region (Phase 3 of `docs/features/source-renderers.md`). For
// Markdown files, delegates to `MarkdownDocumentView` so all the
// existing Markdown chrome (headings, table-of-contents, link
// handling) stays intact. For non-Markdown files the detected
// regions are composed into a synthetic Markdown fragment that
// `MarkdownContent` already knows how to render — reuses the
// Mermaid / KaTeX paths already wired in Phase 1 without a second
// renderer implementation.
//
// Layout rules:
//
// - Dedicated whole-file renderers (e.g. `.mmd` files) compose to a
//   single fence spanning the whole file; `MarkdownContent` picks it
//   up via the Mermaid code-block branch.
// - Mixed-content files (hypothetical — Rust in Phase 5) interleave
//   recognized regions with plain text showing the intervening
//   source; this Phase 3 implementation keeps it simple and shows
//   only the recognized regions with small source line headers so
//   the user can cross-reference them against the Monaco editor in
//   Split mode.
function RendererPreviewPane({
  appearance,
  content,
  documentPath,
  isMarkdownSource,
  onOpenSourceLink,
  renderableRegions,
  workspaceRoot,
}: {
  appearance: MonacoAppearance;
  content: string;
  documentPath: string;
  isMarkdownSource: boolean;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  renderableRegions: SourceRenderableRegion[];
  workspaceRoot: string | null;
}) {
  if (isMarkdownSource) {
    return (
      <MarkdownDocumentView
        appearance={appearance}
        documentPath={documentPath}
        markdown={content}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot={workspaceRoot}
      />
    );
  }
  // Non-Markdown files: compose the renderable regions into a
  // synthetic Markdown fragment. Each region becomes either a fenced
  // block (Mermaid / math fence) or an `$$...$$` math block,
  // prefaced by a subtle "Lines X-Y" label so the user can navigate
  // back to Monaco. `MarkdownContent` handles the rendering safely
  // (sandboxed Mermaid iframe, KaTeX output with
  // contentEditable={false} + data-markdown-serialization="skip").
  const synthetic = composeRendererPreviewMarkdown(renderableRegions);
  return (
    <div className="source-renderer-preview" aria-label="Rendered preview">
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        markdown={synthetic}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot={workspaceRoot}
      />
    </div>
  );
}

function composeRendererPreviewMarkdown(
  regions: SourceRenderableRegion[],
): string {
  if (regions.length === 0) {
    return "";
  }
  return regions
    .map((region) => {
      const header = `**Lines ${region.sourceStartLine}–${region.sourceEndLine}**`;
      const body = composeRendererPreviewRegion(region);
      return `${header}\n\n${body}`;
    })
    .join("\n\n");
}

function composeRendererPreviewRegion(region: SourceRenderableRegion): string {
  if (region.renderer === "mermaid") {
    return ["```mermaid", region.displayText.replace(/\s+$/, ""), "```"].join("\n");
  }
  if (region.renderer === "math") {
    const trimmed = region.displayText.replace(/\s+$/, "");
    // Block math rendered via `$$...$$` so `remark-math` tokenizes it
    // without the code-block path. Inline math would be awkward in a
    // preview pane, so we promote inline regions to block form too.
    return `$$\n${trimmed}\n$$`;
  }
  // Markdown-renderer region (Phase 5 Rust doc comments) lands here
  // as prose — emit the body directly so `MarkdownContent` parses it.
  return region.displayText;
}

function describeRenderableKinds(regions: SourceRenderableRegion[]): string {
  if (regions.length === 0) {
    return "Document";
  }
  const kinds = new Set<string>();
  for (const region of regions) {
    if (region.renderer === "mermaid") {
      kinds.add("Mermaid");
    } else if (region.renderer === "math") {
      kinds.add("Math");
    } else if (region.renderer === "markdown") {
      kinds.add("Markdown");
    }
  }
  const ordered = Array.from(kinds).sort();
  return ordered.join(" + ");
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
