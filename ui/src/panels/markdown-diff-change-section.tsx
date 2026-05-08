// Rendered-Markdown edit sections shared by DiffPanel and SourcePanel:
// the diff-tone per-segment shell (`RenderedMarkdownChangeSection`)
// and the content-editable section body
// (`EditableRenderedMarkdownSection`) that hosts the actual inline
// editor for a markdown document segment.
//
// Together these two components are responsible for:
//   - Rendering a markdown diff segment as a styled `<section>`
//     with optional added / removed tone, line gutter, and the
//     rendered Markdown body.
//   - When `canEdit`, turning the section into a
//     `contentEditable` editor that serialises back to Markdown on
//     input, captures + restores focus across re-renders, swallows
//     shortcut keystrokes (Ctrl-S for save, Escape for discard,
//     arrow / page keys for section-to-section navigation), and
//     sanitises HTML paste through
//     `insertSanitizedMarkdownPaste`.
//   - When the caller allows read-only caret (staged side of a
//     diff with keyboard navigation enabled but no editing), still
//     intercepting the arrow-key caret redirect out of removed
//     sections via `redirectCaretOutOfRemovedMarkdownSection`.
//
// What this file owns:
//   - `RenderedMarkdownChangeSection` — thin tone-wrapped section
//     (`markdown-diff-rendered-section-added` or `-removed`)
//     around `EditableRenderedMarkdownSection`.
//   - `EditableRenderedMarkdownSection` — the contentEditable
//     section with:
//       - refs for uncommitted-edit tracking, draft segment +
//         source-content baselines, the HTMLElement, and the
//         previous segment markdown (used to decide when to reset
//         render state after an external segment replacement).
//       - `renderResetVersion` + `useEffect` that bumps the key
//         of the inner content wrapper so the MarkdownContent
//         subtree remounts cleanly when an external edit lands.
//       - handlers for input, paste, blur, cut, drop,
//         beforeinput, keydown (Ctrl/Cmd+S to save with focus
//         snapshot restore, Escape to discard, arrow keys for
//         between-section navigation with page / end / start
//         boundary detection and an "append empty paragraph"
//         fallback when ArrowDown lands at the last section's
//         end).
//       - `onRegisterCommitter` integration so the parent panel
//         can batch-commit every section's draft from one
//         Save handler.
//
// What this file does NOT own:
//   - `MarkdownDiffView` / `MarkdownDiffDocument` / the
//     `renderMarkdownDiffSegments` switch — those stay in
//     `./DiffPanel.tsx` because they orchestrate segment order,
//     add / remove / unchanged transitions, and the no-changes
//     banner.
//   - Focus restore helpers (`captureEditableMarkdownFocusSnapshot`,
//     `focusAdjacentEditableMarkdownSection`,
//     `isSelectionAtEditableSectionBoundary`,
//     `moveEditableMarkdownCaretByPage`,
//     `placeCaretInEditableMarkdownSection`,
//     `scheduleEditableMarkdownFocusRestore`) — live in
//     `./editable-markdown-focus`.
//   - Markdown paste sanitiser + serializer
//     (`insertSanitizedMarkdownPaste`,
//     `serializeEditableMarkdownSection`) — live in
//     `./markdown-diff-edit-pipeline`.
//   - Caret navigation redirect for removed sections
//     (`getMarkdownCaretNavigationDirection`,
//     `redirectCaretOutOfRemovedMarkdownSection`) — lives in
//     `./markdown-diff-caret-navigation`.
//   - Segment normalisation (`normalizeEditedMarkdownSection`) —
//     lives in `./markdown-diff-segments`.
//   - Pointer / clipboard geometry helpers (`setDropCaretFromPoint`,
//     `getSelectionRangeInsideSection`, `serializeSelectedMarkdown`)
//     — pure DOM/Range utilities live in
//     `./markdown-diff-clipboard-pointer`.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same class names,
// same keyboard bindings, same data-attribute surface, same
// focus-snapshot + save orchestration, same append-empty-paragraph
// ArrowDown-at-EOF fallback (including the single-trailing-blank
// cap).

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type DragEvent,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import { flushSync } from "react-dom";
import { MarkdownContent, type MarkdownFileLinkTarget } from "../message-cards";
import type { MonacoAppearance } from "../monaco";
import { getMarkdownDiffSegmentLineNumber } from "./diff-panel-helpers";
import {
  captureEditableMarkdownFocusSnapshot,
  focusAdjacentEditableMarkdownSection,
  isSelectionAtEditableSectionBoundary,
  moveEditableMarkdownCaretByPage,
  scheduleEditableMarkdownFocusRestore,
} from "./editable-markdown-focus";
import {
  getMarkdownCaretNavigationDirection,
  redirectCaretOutOfRemovedMarkdownSection,
} from "./markdown-diff-caret-navigation";
import type { RenderedMarkdownSectionCommit } from "./markdown-commit-ranges";
import {
  getSelectionRangeInsideSection,
  serializeSelectedMarkdown,
  setDropCaretFromPoint,
} from "./markdown-diff-clipboard-pointer";
import {
  insertSanitizedMarkdownPaste,
  serializeEditableMarkdownSection,
} from "./markdown-diff-edit-pipeline";
import {
  normalizeEditedMarkdownSection,
  type MarkdownDiffDocumentSegment,
} from "./markdown-diff-segments";

type MarkdownDiffSaveHandler = () => Promise<void> | void;

export function RenderedMarkdownChangeSection({
  allowReadOnlyCaret,
  allowCurrentSegmentFallback,
  appearance,
  canEdit,
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
  tone,
  workspaceRoot,
}: {
  allowReadOnlyCaret: boolean;
  allowCurrentSegmentFallback: boolean;
  appearance: MonacoAppearance;
  canEdit: boolean;
  documentPath: string | null;
  editableAriaLabel: string;
  onCommitDrafts: () => boolean;
  onCommitSectionDraft: (commit: RenderedMarkdownSectionCommit) => boolean;
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
        allowCurrentSegmentFallback={allowCurrentSegmentFallback}
        appearance={appearance}
        canEdit={canEdit}
        className="markdown-diff-rendered-section-body"
        documentPath={documentPath}
        editableAriaLabel={editableAriaLabel}
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

export function EditableRenderedMarkdownSection({
  allowReadOnlyCaret,
  allowCurrentSegmentFallback,
  appearance,
  canEdit,
  className,
  documentPath,
  editableAriaLabel,
  fillMermaidAvailableSpace = false,
  onCommitDrafts,
  onCommitSectionDraft,
  onDraftChange,
  onOpenSourceLink,
  onReadOnlyMutation,
  onRegisterCommitter,
  onSave,
  resetOnSegmentMarkdownChange = true,
  segment,
  sourceContent,
  workspaceRoot,
}: {
  allowReadOnlyCaret: boolean;
  allowCurrentSegmentFallback: boolean;
  appearance: MonacoAppearance;
  canEdit: boolean;
  className: string;
  documentPath: string | null;
  editableAriaLabel: string;
  fillMermaidAvailableSpace?: boolean;
  onCommitDrafts: () => boolean;
  onCommitSectionDraft: (commit: RenderedMarkdownSectionCommit) => boolean;
  onDraftChange: (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => void;
  onOpenSourceLink: (target: MarkdownFileLinkTarget) => void;
  onReadOnlyMutation: () => void;
  onRegisterCommitter: (committer: () => RenderedMarkdownSectionCommit | null) => () => void;
  onSave: MarkdownDiffSaveHandler;
  resetOnSegmentMarkdownChange?: boolean;
  segment: MarkdownDiffDocumentSegment;
  sourceContent: string;
  workspaceRoot: string | null;
}) {
  const canUseCaret = canEdit || allowReadOnlyCaret;
  const classNames = `${className}${canUseCaret ? " markdown-diff-editable-section" : ""}${allowReadOnlyCaret && !canEdit ? " markdown-diff-readonly-caret-section" : ""}${fillMermaidAvailableSpace ? " markdown-diff-section-fill-mermaid" : ""}`;
  const hasUncommittedUserEditRef = useRef(false);
  const draftSegmentRef = useRef<MarkdownDiffDocumentSegment | null>(null);
  const draftSourceContentRef = useRef<string | null>(null);
  const previousSegmentIdRef = useRef(segment.id);
  const previousSegmentMarkdownRef = useRef(segment.markdown);
  const sectionRef = useRef<HTMLElement | null>(null);
  const [renderResetVersion, setRenderResetVersion] = useState(0);

  useEffect(() => {
    const segmentIdentityChanged = previousSegmentIdRef.current !== segment.id;
    if (!segmentIdentityChanged && previousSegmentMarkdownRef.current === segment.markdown) {
      return;
    }

    if (hasUncommittedUserEditRef.current) {
      // External concurrent change (e.g. file watcher during a
      // typing session). Only reset if the DOM already agrees with
      // the new segment — otherwise keep the user's in-progress
      // edits so a mid-air file refresh doesn't stomp typed content.
      // (The commit-coming-back case is already handled directly in
      // `collectSectionEdit`, which clears draft state + bumps
      // `renderResetVersion` at the moment the commit is handed off.
      // By the time the save pipeline re-renders segments back into
      // this component, `hasUncommittedUserEditRef` is already false
      // and we take the reset path below.)
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

    previousSegmentIdRef.current = segment.id;
    previousSegmentMarkdownRef.current = segment.markdown;
    hasUncommittedUserEditRef.current = false;
    draftSegmentRef.current = null;
    draftSourceContentRef.current = null;
    if (segmentIdentityChanged || resetOnSegmentMarkdownChange) {
      setRenderResetVersion((current) => current + 1);
    }
  }, [resetOnSegmentMarkdownChange, segment.id, segment.markdown]);

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

  function clearCommittedDraft(
    baseSegment: MarkdownDiffDocumentSegment,
    options?: { resetRenderedContent?: boolean },
  ) {
    hasUncommittedUserEditRef.current = false;
    draftSegmentRef.current = null;
    draftSourceContentRef.current = null;
    onDraftChange(baseSegment, baseSegment.markdown);
    if (options?.resetRenderedContent !== false) {
      setRenderResetVersion((current) => current + 1);
    }
  }

  function handleInput(event: FormEvent<HTMLElement>) {
    if (!canEdit) {
      if (allowReadOnlyCaret) {
        // `onReadOnlyMutation()` bumps a reset version on the
        // parent which remounts this section; the next commit
        // re-paints the rendered Markdown DOM under React's
        // control. Previously we ALSO assigned
        // `event.currentTarget.textContent = segment.markdown`
        // here as a "snap back immediately" guard — but that
        // wrote RAW SOURCE text into the contentEditable for a
        // single paint frame before React's remount landed,
        // producing a visible plain-source flash on every
        // disallowed read-only edit. Dropping the assignment
        // leaves the unwanted user-typed characters visible for
        // one frame, which is less disruptive than the
        // raw-source flash (and React reconciles them away on
        // the immediately-following commit from the
        // `onReadOnlyMutation` remount).
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
      // Updated contract: the parent clears this draft through `onApplied`
      // only after resolving and accepting the commit.
      // Rejected commits leave the draft refs intact so the user can recover
      // or retry instead of losing the in-progress rendered edit.
      return {
        allowCurrentSegmentFallback,
        currentSegment: segment,
        nextMarkdown,
        onApplied: (options) => clearCommittedDraft(commitSegment, options),
        segment: commitSegment,
        sourceContent: sourceAtDraftStart,
      };
    }

    hasUncommittedUserEditRef.current = false;
    draftSegmentRef.current = null;
    draftSourceContentRef.current = null;
    onDraftChange(commitSegment, commitSegment.markdown);
    return null;
  }, [allowCurrentSegmentFallback, canEdit, onDraftChange, segment, sourceContent]);
  const collectSectionEditRef = useRef(collectSectionEdit);
  collectSectionEditRef.current = collectSectionEdit;

  function commitOwnDraft() {
    const section = sectionRef.current;
    if (!section) {
      return true;
    }

    const commit = collectSectionEdit(section);
    if (commit) {
      return onCommitSectionDraft(commit);
    }
    return true;
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

  function handleCopy(event: ClipboardEvent<HTMLElement>) {
    if (!canEdit) {
      return;
    }

    const range = getSelectionRangeInsideSection(event.currentTarget);
    if (!range) {
      return;
    }

    const baseSegment = draftSegmentRef.current ?? segment;
    event.clipboardData.setData(
      "text/plain",
      serializeSelectedMarkdown(range, baseSegment.markdown, event.currentTarget),
    );
    event.preventDefault();
  }

  function handleCut(event: ClipboardEvent<HTMLElement>) {
    if (!canEdit) {
      handleReadOnlyMutationEvent(event);
      return;
    }

    const range = getSelectionRangeInsideSection(event.currentTarget);
    if (!range) {
      return;
    }

    const baseSegment = draftSegmentRef.current ?? segment;
    event.clipboardData.setData(
      "text/plain",
      serializeSelectedMarkdown(range, baseSegment.markdown, event.currentTarget),
    );
    event.preventDefault();
    range.deleteContents();
    range.collapse(true);

    const selection = event.currentTarget.ownerDocument.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    updateLocalDraftState(readEditedMarkdown(event.currentTarget, baseSegment.markdown));
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

  function handleDrop(event: DragEvent<HTMLElement>) {
    if (!canEdit) {
      if (allowReadOnlyCaret) {
        event.preventDefault();
      }
      return;
    }

    event.preventDefault();
    const html = event.dataTransfer.getData("text/html");
    const plainText = event.dataTransfer.getData("text/plain");
    const uriList = event.dataTransfer.getData("text/uri-list");
    const firstUri = uriList
      .split(/\r?\n/)
      .map((line) => line.trim())
      .find((line) => line.length > 0 && !line.startsWith("#"));
    const fallbackText = plainText || firstUri || "";
    if (!html && !fallbackText) {
      return;
    }

    setDropCaretFromPoint(event.currentTarget, event.clientX, event.clientY);
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
        return section ? collectSectionEditRef.current(section) : null;
      }),
    [onRegisterCommitter],
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
      if (!onCommitDrafts()) {
        scheduleEditableMarkdownFocusRestore(focusSnapshot);
        return;
      }
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
        return;
      }

      // No adjacent editable section below — user is at the end of
      // the last editable section and native Down has nowhere to go
      // (common when the section ends with a mermaid fence whose
      // last editable text node sits inside the preserved source
      // code-block). Append an empty editable paragraph and drop
      // the caret inside it.
      //
      // Target element subtlety: the contentEditable <section> wraps
      // a `.markdown-copy-shell-with-line-numbers` div (green/red
      // diff background + line gutter) whose inner `.markdown-copy`
      // holds the rendered markdown blocks. The serializer in
      // `serializeEditableMarkdownSection` reads ONLY from
      // `.markdown-copy`, and the diff-background styling is scoped
      // to that shell too. Appending to the section directly puts
      // the new paragraph as a sibling of the shell — orphaned from
      // both serialization and styling, which is why the user's
      // earlier typed text sat outside the green band with no line
      // number. Targeting `.markdown-copy` keeps the new paragraph
      // inside the React-rendered tree's wrapper so serialization
      // picks it up, the green background covers it, and the line
      // gutter renderer extends to include it on the next re-render.
      if (canEdit) {
        event.preventDefault();
        const section = event.currentTarget;
        const markdownCopy =
          section.querySelector<HTMLElement>(".markdown-copy") ?? section;
        const lastChild = markdownCopy.lastElementChild;
        // Cap trailing empty paragraphs at one. Pressing Down repeatedly
        // without typing anything shouldn't accumulate blank lines — the
        // user gets a single landing spot. Once they type into it, the
        // paragraph stops being empty and the next Down-at-EOF can append
        // another. After a save, any surviving DOM-only empty paragraphs
        // are reconciled away by the re-render (the serializer filters
        // empty blocks), so Down-at-EOF-then-Save naturally unlocks
        // another empty line, matching the intended UX.
        const isEmptyTrailingParagraph =
          lastChild instanceof HTMLParagraphElement &&
          (lastChild.childNodes.length === 0 ||
            (lastChild.childNodes.length === 1 &&
              lastChild.firstChild instanceof HTMLBRElement));
        let targetParagraph: HTMLElement;
        if (isEmptyTrailingParagraph) {
          targetParagraph = lastChild;
        } else {
          // Compute a line-gutter number for the fresh paragraph by
          // extending the highest existing `[data-markdown-line-start]`
          // marker. Markdown paragraphs are separated by a blank line in
          // source, so the next content line is `maxLine + 2`. Without
          // this attribute, MarkdownContent's gutter renderer
          // (`markdown-line-gutter`) wouldn't surface a number for the
          // new blank — it only collects markers from elements already
          // stamped with line-start data.
          let maxLine = 0;
          markdownCopy
            .querySelectorAll<HTMLElement>("[data-markdown-line-start]")
            .forEach((element) => {
              const rangeAttr =
                element.dataset.markdownLineRange ??
                element.dataset.markdownLineStart ??
                "";
              const parts = rangeAttr.split("-");
              const endCandidate = Number(parts[parts.length - 1]);
              if (Number.isFinite(endCandidate) && endCandidate > maxLine) {
                maxLine = endCandidate;
              }
            });
          const nextLine = maxLine > 0 ? maxLine + 2 : 0;
          const trailingParagraph = document.createElement("p");
          trailingParagraph.appendChild(document.createElement("br"));
          if (nextLine > 0) {
            trailingParagraph.dataset.markdownLineStart = String(nextLine);
            trailingParagraph.dataset.markdownLineRange = String(nextLine);
          }
          markdownCopy.appendChild(trailingParagraph);
          targetParagraph = trailingParagraph;
        }
        const range = document.createRange();
        range.setStart(targetParagraph, 0);
        range.collapse(true);
        const selection = window.getSelection();
        if (selection) {
          selection.removeAllRanges();
          selection.addRange(range);
        }
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
        fillMermaidAvailableSpace={fillMermaidAvailableSpace}
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
      fillMermaidAvailableSpace,
      onOpenSourceLink,
      renderedSegment.markdown,
      renderedSegmentLineNumber,
      workspaceRoot,
    ],
  );

  return (
    <section
      className={classNames}
      aria-label={canEdit ? editableAriaLabel : undefined}
      aria-multiline={canEdit ? true : undefined}
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
      onCopy={handleCopy}
      onCut={handleCut}
      onDrop={handleDrop}
      onInput={handleInput}
      onKeyDown={handleKeyDown}
      onPaste={handlePaste}
      ref={sectionRef}
      role={canEdit ? "textbox" : undefined}
      suppressContentEditableWarning
      tabIndex={canUseCaret ? 0 : undefined}
    >
      <div key={renderResetVersion} className="markdown-diff-rendered-section-content">
        {renderedMarkdownContent}
      </div>
    </section>
  );
}
