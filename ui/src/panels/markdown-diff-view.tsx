// Top-level Markdown diff view components used by the diff panel
// when the file under review is markdown and the rendered-Markdown
// view mode is active. Orchestrates the diff toolbar, the inner
// document shell, and the per-segment rendering fan-out to the
// editable change sections.
//
// What this file owns:
//   - `MarkdownDiffView` — the outer shell. Derives stable
//     segments from `buildMarkdownDiffDocumentSegments` +
//     `useStableMarkdownDiffDocumentSegments`, renders the
//     toolbar chips (Rendered Markdown / staged chip / One
//     document / Editable / side-source), the save-state copy +
//     save button, an optional edit-blocked-reason note, the
//     inner `<MarkdownDiffDocument>`, and the status footer
//     (section count + full/patch pill).
//   - `MarkdownDiffDocument` — the inner pane. Hosts the
//     rendered-document header chips, the optional
//     document-wide note (defaulting to "Rendered from patch
//     context only..." when completeness is `"patch"` and no
//     explicit note arrived), the scroll region, and the
//     "No rendered Markdown changes were found." empty state.
//     Intercepts arrow-key caret redirects out of removed
//     sections when `allowReadOnlyCaret` is set.
//   - `renderMarkdownDiffSegments` — the pure segment-to-JSX
//     fan-out: normal segments render as `EditableRenderedMarkdownSection`,
//     added / removed runs group into a single
//     `.markdown-diff-change-block` with per-tone
//     `RenderedMarkdownChangeSection` children. The grouping
//     collapses consecutive non-normal segments so authors see a
//     single removed→added diff block instead of two stacked
//     sections.
//
// What this file does NOT own:
//   - `EditableRenderedMarkdownSection` / `RenderedMarkdownChangeSection`
//     — live in `./markdown-diff-change-section`.
//   - Segment stability + construction — lives in
//     `./markdown-diff-segment-stability` and
//     `./markdown-diff-segments`.
//   - Caret navigation helpers (`getMarkdownCaretNavigationDirection`,
//     `redirectCaretOutOfRemovedMarkdownSection`) — live in
//     `./markdown-diff-caret-navigation`.
//   - The Markdown-side source label formatter
//     (`formatMarkdownSideSource`) — lives in
//     `./diff-panel-helpers`.
//   - `buildDiffPreviewModel` — lives in `../diff-preview`.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same class names,
// chip copy, save-state strings ("Save Markdown" / "Saving..." /
// "Saved"), "Rendered from patch context only..." fallback note,
// and the "No rendered Markdown changes were found." empty-state
// text.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import type { GitDiffSection } from "../api";
import type { buildDiffPreviewModel } from "../diff-preview";
import type { MarkdownFileLinkTarget } from "../message-cards";
import type { MonacoAppearance } from "../monaco";
import { formatMarkdownSideSource } from "./diff-panel-helpers";
import { DiffNavArrow } from "./DiffPanelIcons";
import {
  EditableRenderedMarkdownSection,
  RenderedMarkdownChangeSection,
} from "./markdown-diff-change-section";
import {
  computeMarkdownDiffChangeBlocks,
  type MarkdownDiffChangeBlock,
} from "./markdown-diff-change-index";
import {
  getMarkdownCaretNavigationDirection,
  redirectCaretOutOfRemovedMarkdownSection,
} from "./markdown-diff-caret-navigation";
import type { RenderedMarkdownSectionCommit } from "./markdown-commit-ranges";
import { useStableMarkdownDiffDocumentSegments } from "./markdown-diff-segment-stability";
import {
  buildMarkdownDiffDocumentSegments,
  type MarkdownDiffDocumentSegment,
  type MarkdownDiffPreviewModel,
  type MarkdownDocumentCompleteness,
} from "./markdown-diff-segments";

type MarkdownDiffSaveHandler = () => Promise<void> | void;

export function MarkdownDiffView({
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
  onCommitRenderedMarkdownSectionDraft: (commit: RenderedMarkdownSectionCommit) => boolean;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onCommitRenderedMarkdownDrafts: () => boolean;
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
  // Change blocks mirror the renderer's grouping rules so prev/next
  // navigation visits the same blocks the user sees. Memoised on the
  // stable `segments` array so block identity matches the renderer's
  // React keys.
  const changeBlocks = useMemo(
    () => computeMarkdownDiffChangeBlocks(segments),
    [segments],
  );
  const changeCount = changeBlocks.length;
  const [currentChangeIndex, setCurrentChangeIndex] = useState(0);
  // Clamp the current index when the segment set shrinks (e.g., the
  // user committed a section so a previously-changed block is now
  // unchanged). Falls back to 0 when there are no changes; the
  // counter UI handles the empty case via `changeCount === 0`.
  useEffect(() => {
    setCurrentChangeIndex((current) => {
      if (changeCount === 0) {
        return 0;
      }
      if (current >= changeCount) {
        return changeCount - 1;
      }
      return current;
    });
  }, [changeCount]);
  const goToPreviousChange = useCallback(() => {
    setCurrentChangeIndex((current) =>
      current <= 0 ? Math.max(changeCount - 1, 0) : current - 1,
    );
  }, [changeCount]);
  const goToNextChange = useCallback(() => {
    setCurrentChangeIndex((current) =>
      current >= changeCount - 1 ? 0 : current + 1,
    );
  }, [changeCount]);
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
  // Scroll the active change-block into view whenever the index
  // advances via prev/next. We re-run on `changeCount` too so a fresh
  // block list (e.g., after a commit reduced the change count) still
  // scrolls the now-current block into view. Using `scrollIntoView`
  // with `block: "center"` keeps the block visible without yanking
  // the scroll past in-flight edits in adjacent sections. Initial
  // mount intentionally skips the scroll: the user has not pressed
  // a navigation button yet and the panel should respect whatever
  // scroll position the parent restored.
  const lastScrolledChangeIndexRef = useRef<number | null>(null);
  useEffect(() => {
    if (changeCount === 0) {
      lastScrolledChangeIndexRef.current = null;
      return;
    }
    if (lastScrolledChangeIndexRef.current === currentChangeIndex) {
      return;
    }
    if (lastScrolledChangeIndexRef.current === null) {
      lastScrolledChangeIndexRef.current = currentChangeIndex;
      return;
    }
    const scrollContainer = scrollRef.current;
    if (!scrollContainer) {
      return;
    }
    const target = scrollContainer.querySelector<HTMLElement>(
      `[data-markdown-diff-change-index="${currentChangeIndex}"]`,
    );
    if (!target) {
      return;
    }
    target.scrollIntoView({ block: "center" });
    lastScrolledChangeIndexRef.current = currentChangeIndex;
  }, [changeCount, currentChangeIndex, scrollRef]);

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
        changeBlocks={changeBlocks}
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
          <div className="diff-preview-change-nav" aria-label="Change navigation">
            <button
              className="diff-preview-nav-button"
              type="button"
              onClick={goToPreviousChange}
              disabled={changeCount === 0}
              aria-label="Previous change"
              title="Previous change"
            >
              <DiffNavArrow direction="up" />
            </button>
            <button
              className="diff-preview-nav-button"
              type="button"
              onClick={goToNextChange}
              disabled={changeCount === 0}
              aria-label="Next change"
              title="Next change"
            >
              <DiffNavArrow direction="down" />
            </button>
          </div>
          <span className="source-editor-statusbar-item source-editor-statusbar-state">
            {changeCount === 0
              ? "No changes"
              : `Change ${currentChangeIndex + 1} of ${changeCount}`}
          </span>
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

function MarkdownDiffDocument({
  allowReadOnlyCaret,
  appearance,
  canEdit,
  changeBlocks,
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
  changeBlocks: MarkdownDiffChangeBlock[];
  completeness: MarkdownDocumentCompleteness;
  documentPath: string | null;
  note: string | null;
  onCommitRenderedMarkdownSectionDraft: (commit: RenderedMarkdownSectionCommit) => boolean;
  onRenderedMarkdownSectionDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onCommitRenderedMarkdownDrafts: () => boolean;
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
            changeBlocks,
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
  changeBlocks,
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
  changeBlocks: MarkdownDiffChangeBlock[];
  documentPath: string | null;
  onCommitRenderedMarkdownDrafts: () => boolean;
  onCommitRenderedMarkdownSectionDraft: (commit: RenderedMarkdownSectionCommit) => boolean;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onRegisterRenderedMarkdownCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onReadOnlyRenderedMarkdownMutation: () => void;
  onRenderedMarkdownSectionDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onSave: MarkdownDiffSaveHandler;
  segments: MarkdownDiffDocumentSegment[];
  sourceContent: string;
  workspaceRoot: string | null;
}) {
  // Map block id → index for `data-markdown-diff-change-index` so the
  // navigation prev/next handlers can scroll the right block into
  // view. The block ids match `computeMarkdownDiffChangeBlocks`'s
  // output (segment ids joined by `:`), which is also the React `key`
  // of each `<section className="markdown-diff-change-block">`.
  const changeBlockIndexById = new Map<string, number>();
  changeBlocks.forEach((block, index) => {
    changeBlockIndexById.set(block.id, index);
  });
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
          allowCurrentSegmentFallback={true}
          appearance={appearance}
          canEdit={canEdit && segment.isInAfterDocument}
          className="markdown-diff-normal-section"
          documentPath={documentPath}
          editableAriaLabel="Edit unchanged Markdown section"
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
      // Break the change-block at a pure-add → removed transition.
      // `pushChangedRange` in `./markdown-diff-segments.ts` emits
      // pre-fence pure additions BEFORE the paired removed+added
      // fence replacement, so this transition marks the boundary
      // between "text typed before a fence change" and "the fence
      // replacement itself". Keeping them in one change-block smears
      // the two unrelated changes visually; breaking here renders
      // the pure add in its own green block and the fence removal +
      // fence addition as a separate red→green pair below it.
      const current = changedSegments[changedSegments.length - 1];
      const next = segments[index + 1];
      if (current?.kind === "added" && next?.kind === "removed") {
        break;
      }
      changedSegments.push(segments[index + 1]!);
      index += 1;
    }

    const changeBlockId = changedSegments.map((changed) => changed.id).join(":");
    const changeBlockIndex = changeBlockIndexById.get(changeBlockId);
    rendered.push(
      <section
        className="markdown-diff-change-block"
        data-markdown-diff-change-index={changeBlockIndex ?? undefined}
        key={changeBlockId}
      >
        {changedSegments.map((changedSegment) => {
          const tone = changedSegment.kind === "removed" ? "removed" : "added";
          return (
            <RenderedMarkdownChangeSection
              allowReadOnlyCaret={allowReadOnlyCaret && tone === "added" && changedSegment.isInAfterDocument}
              allowCurrentSegmentFallback={true}
              appearance={appearance}
              canEdit={canEdit && tone === "added" && changedSegment.isInAfterDocument}
              documentPath={documentPath}
              editableAriaLabel={`Edit ${tone} Markdown section`}
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
