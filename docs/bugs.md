# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

## Normalize_git_repo_relative_path accepts Unix-rooted paths on Windows

**Severity:** High - a crafted Git diff request can read files outside the repository on Windows.

`normalize_git_repo_relative_path` rejects paths where `Path::is_absolute()` is true, but on Windows that predicate returns `false` for Unix-rooted paths like `/etc/passwd` or `\etc\passwd` (Windows considers only drive-prefixed or UNC paths "absolute"). `PathBuf::join` then rebinds a rooted-but-not-absolute path to the root of the current drive, so `repo_root.join("/etc/passwd")` becomes `C:\etc\passwd`. `read_git_worktree_text` then reads that file. The `language == "markdown"` extension gate is trivially bypassed by naming any file `.md`. Windows is a P0 platform.

**Current behavior:**
- `is_absolute()` does not catch `/foo` or `\foo` on Windows.
- The `..` split check passes rooted paths.
- `fs::read(repo_root.join(path))` reads arbitrary files on the same drive as the repo.

**Proposal:**
- Also reject `Path::has_root()` (or `trimmed.starts_with('/') || trimmed.starts_with('\\')`) in `normalize_git_repo_relative_path`.
- Add regression tests for `/etc/passwd`, `\etc\passwd`, and `./foo` on both Windows and Unix.

## Rendered staged Markdown edits can silently overwrite unstaged worktree changes

**Severity:** High - editing a staged Markdown diff can clobber unstaged edits in the same file.

When the user edits a rendered staged Markdown diff, segment offsets in `handleRenderedMarkdownSectionChange` index into `markdownPreview.after.content` (the **index** blob), but `handleSave` writes the resulting buffer to the worktree file via `onSaveFile`. If the worktree diverged from the index — which is the exact scenario where staged diffs most matter — the save overwrites unstaged worktree edits with an index-derived document. `latestFile.contentHash` only catches changes since the tab opened, not divergence between index and worktree at that time. The existing notice "Rendered Markdown edits will save this document to the worktree file" does not warn that unstaged changes will be lost.

**Current behavior:**
- Rendered edit is enabled whenever `markdownDisplayPreview.after.completeness === "full"`, including for staged diffs.
- Section offsets are computed against the index blob.
- Save writes the edited index-derived content to the worktree, clobbering unstaged edits.

**Proposal:**
- Make rendered edit read-only when `markdownPreview.after.source === "index"` (staged review is pure review).
- Or for staged diffs, base edits on `latestFile.content` (worktree) and compute segment offsets against the worktree content rather than the index.
- Add a regression test that covers a staged edit with a divergent worktree.

## Rendered Markdown serializer has no escaping for prose special characters

**Severity:** High - editing rendered Markdown can silently corrupt unchanged prose, tables, and code blocks.

`serializeEditableMarkdownSection` and its block/inline helpers emit Markdown without escaping `*`, `_`, `` ` ``, `#`, `[`, `]`, `(`, `)`, `|`, `\`, `>`, `!`, or hard line breaks. Round-tripping a section containing literal `*not italic*`, `# not heading`, a code block whose content contains ``` ``` ```, a table cell with `|`, or `<br>` inside `<p>` silently rewrites the source. Additional gaps: `.trim()` at the block level drops significant whitespace between inline elements (collapsing `<strong>a</strong> <em>b</em>` to `**a***b*`), `<img>` has no case so all images are stripped, nested non-list blocks inside `<li>` are dropped, and workspace file-link hrefs round-trip as absolute paths even for unchanged links.

**Current behavior:**
- Block and inline serializers emit text verbatim with no escaping.
- Code-block fences collide with matching backticks inside the code.
- Table cells with `|` silently create extra columns on re-render.
- Images disappear on edit.
- `.trim()` collapses whitespace text nodes between inline elements.
- Nested list block children (`<p>` inside `<li>`) are dropped.
- File-link canonicalization mutates untouched relative links to absolute paths.

**Proposal:**
- Introduce `escapeMarkdownInline`, `escapeMarkdownTableCell`, and `escapeMarkdownBlock` helpers and apply them in the serializer.
- Preserve significant whitespace text nodes in inline serialization.
- Add `<img>` and nested-block handling, and disable edit for sections containing unsupported constructs (`<table>`, `<pre>`, `<img>`, task lists) until round-trip tests cover them.
- Add round-trip tests for every v1-required Markdown feature.

## Rendered Markdown reset effect wipes undo state on successful saves

**Severity:** High - mid-edit state can be lost when the diff panel refreshes for unrelated reasons.

The `useEffect` that resets `viewMode`, editor statuses, `markdownEditContent`, and the undo/redo stacks depends on `[diffMessageId, filePath, preferMarkdownView, preview.hasStructuredPreview]`. Both `preferMarkdownView` and `preview.hasStructuredPreview` are derived values that flip whenever `latestFile.content` changes, `previewSourceContent` changes, or `documentContent.isCompleteDocument` toggles. A successful save updates `latestFile.content`, which flows into `previewSourceContent`, which flips `preview.hasStructuredPreview` for certain diffs — the reset effect then clears `markdownEditContent` and undo history mid-flow.

**Current behavior:**
- Reset deps include two derived values plus `preferMarkdownView`.
- A save cycle can trigger the reset and wipe in-progress rendered edits and undo stacks.
- Users lose undo history after every successful save even if no tab identity change occurred.

**Proposal:**
- Split tab-identity reset from derived-flag defaulting: only clear `markdownEditContent` and undo/redo stacks on `[diffMessageId, filePath]` change.
- Thread `preferMarkdownView` only into the initial `useState()` for `viewMode`.
- Add a regression test that saves mid-edit and asserts undo history survives.

## Rendered Markdown diff recomputes LCS on every keystroke

**Severity:** High - typing inside a rendered Markdown diff scales poorly with document size.

`handleInput` calls `serializeEditableMarkdownSection` on every `input` event, which feeds `handleRenderedMarkdownSectionChange` → `replaceMarkdownDocumentRange` → `setMarkdownEditContentState` → new `markdownDisplayPreview` memo → `buildFullMarkdownDiffDocumentSegments`. That function runs a full LCS scan over the whole document (O(N*M), up to 1,000,000 cells before the greedy fallback) and re-renders N `ReactMarkdown` subtrees. Typing in a mid-size README becomes laggy; large documents are unusable.

**Current behavior:**
- Every `input` event triggers a full-document serialize + LCS + N ReactMarkdown re-renders.
- No debounce or deferred value splits the edit path from the segmentation path.
- The greedy LCS fallback is still O(N*M) in time (only the memory footprint is bounded).

**Proposal:**
- Debounce the serialize + LCS path with `useDeferredValue`, `startTransition`, or explicit `setTimeout` scheduling.
- Separate "edit the local section's buffer" from "recompute the full diff view"; only re-run segmentation on blur or save.
- Replace the greedy LCS fallback with a `Map<compareText, number[]>` index to achieve amortized `O(N + M)`.

## Rendered Markdown custom undo/redo fights contentEditable and loses caret state

**Severity:** High - undo/redo inside a rendered Markdown section destroys focus, selection, and scroll position.

The custom `Ctrl/Cmd-Z` / `Ctrl/Cmd-Y` path calls `preventDefault()` and replaces document content via `applyRenderedMarkdownContentFromHistory`, which calls `bumpRenderedMarkdownEditRevision`. The revision is a React `key` component, so every undo force-remounts all sections. Browser-native contentEditable undo is destroyed on every keystroke (because React re-renders the subtree), so the custom stack is the only source of truth. After each undo step the caret, focus, and scroll position are lost, and further keystrokes start from the top of the section. Worse, the stack retains full-document snapshots capped at 100, so a 1 MB Markdown document can pin ~100 MB.

**Current behavior:**
- Every undo/redo bumps `editorRevision`, forcing a full remount of all sections.
- Browser-native contentEditable undo history is destroyed on every React re-render.
- Caret position is lost after every undo step.
- Undo stack stores full-document string snapshots, uncapped by total bytes.

**Proposal:**
- Stop remounting sections on undo; apply edits imperatively (e.g. `innerHTML` or a stable integer key) so React does not own the subtree during edit.
- Store operations (offset, removed text, inserted text) instead of full snapshots, or cap by total bytes.
- Coalesce snapshots by time/identity before pushing.
- Add a regression test that performs several edits, undoes them one at a time, and asserts the caret position and section state are preserved.

## MarkdownContent components prop identity churns, remounting subtree per keystroke

**Severity:** High - typing inside a rendered Markdown section loses focus because the entire `ReactMarkdown` subtree unmounts and remounts on every render.

Inside the `MarkdownContent` memoized `rendered` tree, the `components` object passed to `ReactMarkdown` is recreated as an object literal on every memo re-run. React treats each new object identity as a different component type, so `ReactMarkdown` unmounts the DOM subtree and recreates it. For rendered Markdown editing, `segment.markdown` changes on every keystroke, which re-runs the memo, which creates a new `components` object, which remounts the rendered subtree — losing focus, selection, and caret position inside the contentEditable section.

**Current behavior:**
- `components={{ ... }}` is declared inline inside `useMemo` in `MarkdownContent`.
- Every memo re-run produces new identity for every sub-component function.
- Rendered edit sections lose focus on every input event.

**Proposal:**
- Hoist `components` to a module-level constant, or memoize it via `useMemo(() => ({...}), [stable deps])`.
- Access callbacks via refs so they do not force memo invalidation.
- Add a regression test: type a character into a rendered Markdown diff section and assert the caret is still inside the section.

## Rendered Markdown diff editing can save stale content

**Severity:** High - rendered Markdown edits can diverge from the buffer that
is actually saved.

Rendered Markdown diff mode keeps its own `markdownEditContent` state while
the existing diff editor lifecycle still updates and saves `editValue`.
Code-mode edits, reloads, disk-change rebases, and post-save sync can leave
the rendered view showing stale content or can overwrite newer rendered edits
with an older saved snapshot.

**Current behavior:**
- Rendered Markdown displays `markdownEditContent` when present.
- Normal edit, reload, and rebase paths update `editValue` without always
  adopting or clearing the rendered Markdown buffer and undo/redo history.
- Saving can write a buffer that is not the document the user is currently
  seeing in rendered mode.

**Proposal:**
- Make one editable document buffer authoritative for rendered and code edit
  modes.
- Centralize buffer adoption so `editValue`, `markdownEditContent`, refs,
  dirty state, undo/redo history, and editor revision move together.
- Guard post-save sync with a freshness check or disable rendered editing
  while a save is in flight.

## Markdown diff worktree reads follow symlinks

**Severity:** High - symlinked Markdown paths can expose or render the wrong
worktree content.

The Markdown document diff loader reads the worktree side with
`fs::read(repo_root.join(path))`. On symlink paths this follows the target
instead of matching Git's symlink blob semantics, and a symlink can point
outside the repository.

**Current behavior:**
- Worktree document-content reads follow symlinks.
- A Markdown diff for a symlinked path can show target file contents rather
  than the symlink target text Git stores.
- A symlink can escape the intended repository containment boundary.

**Proposal:**
- Use `symlink_metadata` before reading worktree document sides.
- For symlinks, return the symlink target text to match Git blob semantics.
- For regular files, canonicalize the repo root and target path and reject
  targets outside the canonical repo root.

## Markdown document diff sides are loaded without size limits

**Severity:** Medium - large Markdown blobs or files can produce unbounded
memory use and oversized JSON responses.

The new full-document Markdown diff enrichment buffers entire Git blobs with
`git show ... .output()` and reads whole worktree files with `fs::read`.
Unlike `/api/file`, this path does not enforce the existing
`MAX_FILE_CONTENT_BYTES` ceiling.

**Current behavior:**
- HEAD and index document sides are loaded through unbounded `git show`
  output buffering.
- Worktree document sides are read fully into memory.
- Large Markdown files can make `/api/git/diff` allocate and return far more
  data than other file-reading paths allow.

**Proposal:**
- Enforce the same content-size ceiling used by source file reads.
- Check Git object size with `git cat-file -s` or stream Git output through a
  capped reader.
- Check worktree metadata before reading and reject over-limit files.

## Markdown document enrichment can fail the whole diff response

**Severity:** Medium - optional rendered-document enrichment can hide an
otherwise valid raw diff.

The backend loads the raw Git diff first, then enriches Markdown diffs with
before/after document content. If enrichment fails for a status it cannot
model, such as an unmerged Markdown file without a stage-0 index entry, the
entire `/api/git/diff` request fails instead of falling back to raw or patch
preview.

**Current behavior:**
- Enrichment errors from `load_git_diff_document_content` propagate as endpoint
  failures.
- Conflict or unmerged Markdown diffs can return an API error even when
  `git diff -- <path>` has usable output.

**Proposal:**
- Treat Markdown document-content enrichment as best-effort.
- Skip `documentContent` for unsupported statuses such as unmerged files.
- Fall back to the existing raw/structured diff response when enrichment
  fails after the core diff has loaded.

## Unstaged Markdown previews can read the wrong index path after a staged rename

**Severity:** Medium - staged rename plus unstaged edit can break Markdown
diff preview loading.

For an unstaged comparison after a staged rename, the index side should be the
new path. The current document-content path can use `originalPath` for the
unstaged index side, which tries to read the old path from the index.

**Current behavior:**
- A state like `RM old.md -> new.md` plus an unstaged edit to `new.md` can
  request `git show :old.md`.
- That index entry no longer exists, so Markdown document preview loading can
  fail.

**Proposal:**
- Make document-side path selection section/status aware.
- Use `currentPath` for unstaged index sides unless the unstaged change itself
  is a rename or copy.
- Keep `originalPath` tied to the diff section where it is actually valid.

## Markdown document content can be persisted in workspace layout

**Severity:** Medium - opening a Markdown diff can persist full file contents
into layout state.

`WorkspaceDiffPreviewTab` now carries `documentContent`, which includes full
before/after Markdown documents. Workspace layout persistence can therefore
write unchanged document sections, including possible secrets, into the
TermAl session state file.

**Current behavior:**
- Diff preview tabs can hold full Markdown document contents.
- Workspace layout autosave serializes tab data.
- Restored layout state can contain more than view metadata and patch content.

**Proposal:**
- Keep `documentContent` ephemeral.
- Strip full document content before workspace layout persistence.
- Re-fetch document content when restoring or reopening a diff preview tab.

## Patch-based Markdown previews can be mislabeled as complete documents

**Severity:** Medium - rendered editing can be enabled for incomplete
patch-reconstructed Markdown.

When backend `documentContent` is absent, the frontend infers completeness from
`preview.note`. That field is presentation text, not provenance. A
patch-reconstructed preview can be mislabeled as a full document and become
editable even though unchanged sections may be missing.

**Current behavior:**
- `preview.note` is used to choose between `patch` and `full` completeness for
  fallback Markdown preview data.
- Generic preview data without a note can be treated as a complete document.
- Rendered Markdown editing can be enabled for incomplete content.

**Proposal:**
- Add explicit completeness/provenance to the diff preview model.
- Treat patch reconstruction as `patch` unless full-document content was
  loaded from an authoritative file, index, or Git object source.

## Rendered Markdown task-list serialization drops checkbox state

**Severity:** Medium - editing task lists in rendered Markdown can corrupt GFM
task-list syntax.

ReactMarkdown renders GFM task-list markers as checkbox inputs. The rendered
Markdown serializer currently serializes list items from inline child text, so
checkbox inputs contribute no `[x]` or `[ ]` marker.

**Current behavior:**
- Editing `- [x] Done` through rendered Markdown can save as `- Done`.
- Checked and unchecked task-list state is lost during serialization.

**Proposal:**
- Detect task-list checkbox inputs while serializing list items.
- Prefix serialized list item text with `[x]` or `[ ]` based on checkbox state.
- Cover checked and unchecked task-list saves with tests.

## canEditVisualDiff gate disables Monaco inline edit for any Markdown diff

**Severity:** Medium - opening a Monaco Markdown diff in `all` view no longer allows inline edits when the backend supplies `documentContent`.

`canEditVisualDiff` is gated on `!documentContent && preview.hasStructuredPreview && ...` in `DiffPanel.tsx`. The gate mixes two concerns — rendered-Markdown-edit safety vs. Monaco inline edit capability — so any Git-backed Markdown diff (including unstaged) silently loses the `all`-mode inline editing affordance that used to work.

**Current behavior:**
- Opening a Markdown Git diff in `all` view shows Monaco but the inline edit affordance is disabled.
- The disable is triggered by the mere presence of `documentContent`, not by the selected view mode.
- Non-Markdown diffs are unaffected.

**Proposal:**
- Gate rendered-edit safety on `viewMode === "markdown"` rather than the presence of `documentContent`.
- Keep `canEditVisualDiff` independent of the new document-content path.
- Add a test that opens a Markdown diff, selects `all`, and asserts the Monaco inline edit affordance is available.

## Rendered Markdown diff view mode default overrides user selection on refresh

**Severity:** Medium - users lose their chosen diff view mode when the diff panel refreshes.

`preferMarkdownView` is included in the reset-effect dependency array. Any refresh that flips `documentContent` between `null` and present (e.g. auto-refresh after a Git status change) resets `viewMode` back to `markdown` even if the user had explicitly selected `Raw` or `Changes`. A user mid-review can be yanked out of their chosen mode without any interaction.

**Current behavior:**
- `preferMarkdownView` is in the reset-effect deps.
- Auto-refresh after Git status change resets `viewMode` to the default.
- User's explicit view selection is lost.

**Proposal:**
- Use `preferMarkdownView` only in the initial `useState(...)`, not in the reset effect.
- Keep the user's selection sticky across refreshes.
- Add a regression test that switches to `Raw`, triggers a refresh, and asserts the mode stays `Raw`.

## Markdown diff anchor normalization silently hides real line changes

**Severity:** Medium - link-only and table-separator line changes can render as unchanged in rendered Markdown diff.

`normalizeMarkdownLineForDiff` strips link syntax (`[foo](bar)` → `foo`) and collapses table separators to compute the `compareText` used for LCS anchor matching. But `splitMarkdownDocumentLinesWithOffsets` stores the raw `text` too, and the segment builder renders segments from `text`, not `compareText`. A line matched via normalization renders only the after side, so a link-only style change on that line is silently hidden from rendered Markdown review even though the `Raw` and `Changes` views still show it.

**Current behavior:**
- Anchors match on normalized text; segments render raw text.
- Link-only changes inside an anchored line disappear from the rendered Markdown view.
- `Raw` and `Changes` views still show the change, so the views disagree.

**Proposal:**
- When an anchor is matched via non-identity normalization, mark the segment `changed` and render both sides inline.
- Or document the tradeoff explicitly in the feature spec and surface a hint in the UI.
- Add a test that exercises a link-only change and asserts both the before and after sides render.

## Markdown document sides corrupt non-UTF-8 content on save

**Severity:** Medium - Markdown files with BOM or Latin-1 content lose their original bytes when edited through rendered Markdown.

`read_git_object_text`, `read_git_index_text`, and `read_git_worktree_text` decode via `String::from_utf8_lossy`. For a Markdown file with a UTF-16 BOM or Windows-1252 bytes (legitimate on Windows — P0 platform), invalid UTF-8 bytes become U+FFFD replacement characters. If the user then edits the rendered view and saves, the lossy string is written back through `onSaveFile`, permanently losing the original bytes.

**Current behavior:**
- All three readers call `.from_utf8_lossy(...).into_owned()`.
- Non-UTF-8 bytes are silently replaced with U+FFFD.
- Saving from rendered mode persists the lossy content to disk.

**Proposal:**
- Detect non-UTF-8 with `std::str::from_utf8` before constructing `documentContent`.
- On failure, return `None` for `documentContent` or set `note: "binary or non-UTF-8 document"` and disable `canEditRenderedMarkdown` on the frontend.
- Add a test that stages a file with Latin-1 bytes and asserts `document_content.is_none()` (or the note is set).

## Rendered Markdown contentEditable lacks ARIA role and label

**Severity:** Medium - screen reader users cannot tell that a rendered Markdown section is editable.

`EditableRenderedMarkdownSection` renders `<section contentEditable={canEdit} tabIndex={...}>` without `role="textbox"`, `aria-multiline="true"`, or `aria-label`. Assistive technology announces a plain block with no signal that it accepts input — a regression from the Monaco editor the user would have been using in `Edit` mode.

**Current behavior:**
- No ARIA attributes on the editable section.
- Screen readers describe the region as a generic landmark.
- Keyboard-only users get no affordance hint beyond the focus ring.

**Proposal:**
- Add `role="textbox"`, `aria-multiline="true"`, and `aria-label={`Markdown section, ${segment.kind}`}` to every editable section.
- Add an a11y regression test that asserts these attributes are present when `canEdit` is true.

## Rendered Markdown line-height fallback teleports the caret

**Severity:** Medium - arrow-down can teleport the caret to the next section mid-paragraph at small font sizes.

`isSelectionAtEditableSectionVisualBoundary` reads `Number.parseFloat(computedStyle.lineHeight)` and falls back to a hardcoded `32` pixels when `lineHeight` is `"normal"` or non-numeric. At a 12px UI font with a ~16px line height, the 32px threshold falsely reports "at boundary" on line 2 of a 3-line paragraph, so the caret jumps to the next section mid-paragraph.

**Current behavior:**
- `Number.parseFloat("normal") === NaN`, triggering the 32-pixel fallback.
- Small font sizes trip the false positive for all but the last line of multi-line paragraphs.
- Caret jumps to the next section before the user has finished the current one.

**Proposal:**
- Read `computedStyle.fontSize` (always numeric) and scale it (`fontSize * 1.6`) as the fallback.
- Add a test that exercises a multi-line paragraph with a small font size and asserts arrow-down stays within the section until the last line.

## DiffPanel.tsx holds 1500 lines of orthogonal utility logic

**Severity:** Medium - the line-diff engine, HTML-to-Markdown serializer, and caret-navigation controller live inside the panel and cannot be tested or reused independently.

`DiffPanel.tsx` now contains ~1,500 lines of three distinct concerns: (a) a Markdown line-diff engine (LCS, anchor rebuilding, greedy fallback, compare-text normalization), (b) an HTML→Markdown serializer (block+inline node walk, table/list/code serialization, whitespace normalization), and (c) a contenteditable caret-navigation micro-controller (selection boundary detection via Range, adjacent-section focus). None of these are panel concerns, and keeping them co-located makes each untestable in isolation and discourages reuse from, e.g., `MarkdownDocumentView.tsx`.

**Current behavior:**
- `DiffPanel.tsx` is responsible for panel rendering AND diff segmentation AND HTML serialization AND caret navigation.
- Unit-testing the serializer requires importing the whole diff panel.
- The single-large-file project exception covers `App.tsx` and `main.rs`, not every new panel.

**Proposal:**
- Extract `ui/src/panels/markdown-diff-segments.ts` for LCS/anchor/segment building.
- Extract `ui/src/markdown-html-to-markdown.ts` for block/inline serialization.
- Optionally extract caret-navigation helpers to `ui/src/panels/editable-markdown-caret.ts`.
- Add dedicated unit tests for each extracted module.

## Diff Markdown links drop source-target metadata

**Severity:** Low - links from rendered Markdown diffs open less precisely than
the same links elsewhere.

`MarkdownContent` can resolve links with line, column, and open-in-new-tab
metadata. The DiffPanel link handler currently passes only the path to
`onOpenPath`, so source anchors like `file.ts#L20C4` lose their target
position.

**Current behavior:**
- Rendered Markdown links in diff view discard line and column metadata.
- Diff links also ignore open-in-new-tab intent.
- Source preview and message cards preserve richer link targets, so behavior is
  inconsistent.

**Proposal:**
- Widen the DiffPanel open callback to accept the full Markdown link target.
- Pass line, column, and open-in-new-tab metadata through App to source-tab
  opening.

## Git diff document API type includes a client-only patch source

**Severity:** Low - the frontend API type is broader than the backend wire
contract.

`GitDiffDocumentSideSource` in `ui/src/api.ts` includes `"patch"`, but the
backend serializes only `head`, `index`, `worktree`, or `empty`. Patch is a
client-side fallback provenance, not a backend API response value.

**Current behavior:**
- The exported API response type accepts a source value the backend does not
  send.
- Exhaustiveness checks against the wire contract are weaker than necessary.

**Proposal:**
- Keep the API response union exact: `head`, `index`, `worktree`, and `empty`.
- Introduce a separate view-model source union that adds `patch` for fallback
  previews.

## Markdown images remain draggable in selectable rendered content

**Severity:** Low - dragging across rendered Markdown containing images can
start native image drag instead of text selection.

Markdown anchors were made non-draggable for selectable document views, but
images still use ReactMarkdown's default draggable `<img>` behavior. This can
interfere with selecting or editing text around images.

**Current behavior:**
- Rendered Markdown images are draggable by default.
- Dragging through selectable or editable Markdown near an image can start a
  native image drag.

**Proposal:**
- Add an `img` component override in `MarkdownContent`.
- Render Markdown images with `draggable={false}`.

## Implementation Tasks

- [ ] P2: Add frontend plumbing coverage for Markdown `documentContent` diff tabs:
  open or refresh a Git diff preview with sample staged/index/worktree
  document content and assert the resulting workspace tab preserves enough
  metadata to render the authoritative Markdown side.
- [ ] P2: Add SourcePanel split-mode Markdown coverage:
  switch to `Split`, edit the mocked editor buffer, assert the preview updates
  from the unsaved buffer, and exercise a document link through
  `onOpenSourceLink`.
- [ ] P2: Add MarkdownContent document-relative link tests:
  cover `documentPath` resolution for `./`, `../`, anchors such as
  `api.md#L10`, and Windows-style workspace roots.
- [ ] P2: Add rendered Markdown diff save serialization coverage:
  edit and save sections containing tables, task/list items, fenced code,
  headings, links, and inline code, then assert the exact Markdown payload.
- [ ] P2: Tighten the rendered Markdown undo/redo test assertion:
  replace the `toBeTruthy()` plus non-null assertion with a concrete
  `HTMLElement` assertion or a user-visible query after editable sections have
  accessible labels.
- [ ] P2: Add backend integration tests for Git diff document content across statuses:
  cover Added, Deleted, Untracked, and Renamed Markdown files in both staged
  and unstaged sections, plus a non-Markdown negative case asserting
  `document_content.is_none()`. Disable `core.autocrlf` in the fixture setup
  so Windows CI is reliable.
- [ ] P2: Add error-path coverage for the Git document-content readers:
  exercise `read_git_object_text`, `read_git_index_text`, and
  `read_git_worktree_text` on missing objects, unknown revisions, and
  disappeared worktree files, and assert the returned error strings mention
  git and are non-empty.
- [ ] P2: Replace innerHTML-based rendered Markdown edit tests with real typing:
  use `userEvent.type` or `fireEvent.beforeInput`+`input` sequences so the
  serializer runs over DOM produced by the real editing pipeline.
- [ ] P2: Stub Range geometry for caret visual-boundary tests:
  spy on `Element.prototype.getBoundingClientRect` and
  `Range.prototype.getClientRects` to return realistic values so
  `isSelectionAtEditableSectionVisualBoundary` actually exercises the
  visual-line math instead of the 32-pixel fallback.
- [ ] P2: Extend undo/redo coverage:
  add multi-edit then multi-undo then re-edit flows, assert the redo stack is
  cleared after new input, and rerender with a new `diffMessageId` to assert
  undo history is cleared on tab identity change.
- [ ] P2: Add tests for the rendered Markdown diff patch-only fallback:
  render without `documentContent` but with a structured Markdown diff and
  assert the "Patch preview" chip and the patch-only note are visible and
  edit buttons are disabled.
- [ ] P2: Add a negative test for Markdown mode visibility:
  render a non-Markdown diff (`.ts`/`.rs`) and assert
  `screen.queryByRole('button', { name: /Markdown/ })` is null.
- [ ] P2: Expand SourcePanel Markdown mode tests beyond preview:
  cover split-mode rendering, Code→Preview→Split→Code round-trip, and the
  mode reset when switching from a Markdown file to a non-Markdown file.
- [ ] P2: Add a test for the MarkdownLinkContext anti-nesting behavior:
  render `` [prefix `lib/models/foo.rs` suffix](https://example.com) `` and
  assert `document.querySelectorAll('a a').length === 0` while inline code
  still renders.
- [ ] P2: Add unit tests for the Markdown line-diff anchor builders:
  exercise `buildMarkdownLineDiffAnchors` and
  `buildGreedyMarkdownLineAnchors` with empty docs, single lines, and
  inputs near the 1,000,000-cell threshold to cover the greedy fallback.
- [ ] P2: Strengthen CRLF rendered Markdown test assertions:
  assert that CR characters are absent from the rendered output and that
  matched lines render exactly once, and add a test where both sides use
  CRLF line endings.
- [ ] P2: Add a render-level test for the round 9 multi-commit pin
  survival in `ui/src/panels/AgentSessionPanel.tsx`. The 11 boundary
  unit tests pin the constants `4` and `72`, but the actual round 8
  Medium bug fix mechanism — that the `useLayoutEffect` no longer
  clears `shouldKeepBottomAfterLayoutRef.current`, so the re-pin
  survives a `setLayoutVersion` → follow-up-commit transition — is
  not exercised by any test. A future refactor that moves the clear
  back into the re-pinning effect (or into the same-commit branch of
  `handleHeightChange`) would leave the 11 boundary tests passing
  while the pin silently regresses. Mount
  `VirtualizedConversationMessageList` (or export it for the test),
  stub `getBoundingClientRect` / jsdom geometry to drive
  `handleHeightChange` with an initial "near bottom" node, fire a
  growing height for a specific message, spy on `node.scrollTop`
  assignments, and assert two successive growth commits produce two
  re-pin writes (not just one). Alternatively, use an integration
  test through `AgentSessionPanel` that dispatches state updates with
  growing message measurements and asserts the viewport remains
  pinned across multiple commits.
- [ ] P2: Tighten `keeps the new-response button scroll correction
  alive for the explicit minAttempts floor` in `ui/src/App.test.tsx`
  to actually pin the EXPLICIT `minAttempts: 8`. The production call
  uses `maxAttempts: 60, minAttempts: 8`, but
  `resolveSettledScrollMinimumAttempts(60)` already returns 8 via the
  default `maxAttempts > 12 ? 8 : 4` branch — so dropping the
  explicit `minAttempts: 8` from the production call would still
  pass. Either use `maxAttempts: 12, minAttempts: 8` in a second
  integration assertion (so the default floor would drop to 4 and
  only the explicit arg keeps it at 8), or drop the integration test
  and rely on the unit tests at 10727-10734 alone — they already pin
  `resolveSettledScrollMinimumAttempts` cleanly and the integration
  harness adds cost without catching the regression the title claims.
- [ ] P2: Decide the fate of `isScrollContainerAtBottom` in
  `ui/src/panels/AgentSessionPanel.tsx:3140`. The function is exported
  and has 4 boundary unit tests but zero production callers — the
  only references inside `AgentSessionPanel.tsx` are two comments
  describing the round 8 pre-fix behavior. Either delete the helper
  and its tests (the historical-bug explanation can stay as a
  comment), or add a brief comment on the helper itself explaining
  why a dead export is intentionally retained as a documentation
  anchor. Three reviewers flagged this independently.
- [ ] P2: Optionally tree-shake `setAppTestHooksForTests` out of
  production builds in `ui/src/App.tsx`. The hook is currently safe
  (observer-only callback receiving hard-coded `"resolve" | "reject"`
  label arguments with ignored return values, gated on optional
  chaining behind the `isMountedRef.current` guard, cleared in
  `afterEach`), but it ships unconditionally in the production
  bundle. A future hook field that receives richer arguments
  (`StateResponse`, session text, draft attachments) would start
  leaking sensitive data into any code running in the page through
  the same export pattern. Either wrap the export in
  `if (import.meta.env.MODE === "test")` / a separate `App.testing.ts`
  entry, or add a code comment requiring all current and future
  fields to accept only non-sensitive label arguments.
- [ ] P2: Export the `AppTestHooks` type in `ui/src/App.tsx:893`. The
  type is currently a local declaration that
  `setAppTestHooksForTests` annotates, forcing test consumers to
  either duplicate the shape inline or use
  `Parameters<typeof setAppTestHooksForTests>[0]`. Free type-surface
  improvement with no runtime cost.
- [ ] P2: Add `expect(resolveSettledScrollMinimumAttempts(0)).toBe(0)`
  to `ui/src/App.test.tsx:10727-10734`. With `maxAttempts = 0`, the
  default branch yields 4 but `Math.min(4, 0) = 0`, so the helper
  returns 0 — which would bypass the entire attempt-count floor. Not
  used today, but a minimal `max=0` edge assertion would pin the
  invariant so a future refactor that special-cased the floor (e.g.
  `Math.max(1, ...)`) would have a test to flip.

## Known Design Limitations

These are deliberate design tradeoffs, not bugs, but are recorded here so
they stay visible to future contributors and can be revisited if the
tradeoff space changes.

### Terminal commands have no production watchdog

Terminal commands intentionally run without a production timeout. The terminal
panel is used for long-lived foreground workflows such as `flutter run`, dev
servers, watch tasks, and REPL-like tools, so a watchdog would terminate
commands users expect to keep alive.

**Consequence:** a running command holds its terminal concurrency permit until
it exits or the stream is disconnected. The streamed local path now observes
SSE disconnects and kills the local process tree, but ordinary JSON terminal
runs and remote command lifetime are still governed by the command/backend
itself rather than a TermAl watchdog.

**Mitigations already in place:**
- Local and remote terminal commands have separate concurrency caps.
- Captured output and live stream buffers are bounded.
- The no-timeout behavior is documented at the local and remote terminal
  launch sites in `src/api.rs`.

### Remote terminal stream worker thread can stay parked on a stalled remote

`InterruptibleRemoteStreamReader::spawn` in `src/remote.rs` wraps the
blocking remote HTTP body read in a dedicated OS thread that pushes chunks
into an `mpsc::sync_channel(1)`. The main forwarding loop reads from the
channel with `recv_timeout(10ms)` and observes the cancellation flag
between polls, so the user-visible "4-in-flight remote permit" path is
correctly released on client disconnect — the forwarder returns
`terminal stream client disconnected`, drops its end of the channel,
releases the semaphore permit, and exits. The spawned reader thread,
however, is still parked inside `source.read(&mut scratch)` until the
reqwest body read finally returns (a byte arrives, the socket closes, or
an error fires). Because the backend intentionally builds its
`BlockingHttpClient` without a body read timeout (so legitimate long
streams can keep producing output), a remote that holds its socket open
without emitting bytes will pin the reader thread and its TCP socket
until the remote finally closes the connection.

**Consequence:** repeated client-disconnect cycles against a stalled
remote accumulate detached reader threads + sockets, one per disconnect,
until the remote eventually closes its side. Each thread holds a
~2-8 MB stack and a single TCP connection. The bound is set by the
remote's own keepalive / TCP timeout behaviour, not by TermAl.

**Accepted tradeoff.** The three real fixes — setting a per-read body
timeout on reqwest (not supported by the blocking API), moving the read
onto an async `tokio::select!` with a cancellation future (a large
rewrite of the remote bridge), or manually `dup`ing the raw socket fd
and closing it from outside (unsafe, platform-specific, bypasses reqwest
encapsulation) — are all strictly larger than the bounded dormant-thread
cost. This mirrors the existing "Unix terminal clean-exit cleanup is a
no-op" limitation below: both are bounded native-thread leaks that wait
on an external event (grandchild pipe close, remote socket close), and
both are left as-is because the cleanup machinery needed to plug them
would be more invasive than the leak they prevent.

**Mitigations already in place:**
- The forwarder returns promptly on cancellation via the adapter's
  `recv_timeout` poll, so the user-visible semaphore permit is released
  immediately and new commands are not blocked by dormant reader threads.
- `read_remote_stream_response` re-checks the cancellation flag between
  reads, so a reader thread that finally gets a byte from the remote
  exits on the next loop iteration rather than continuing to buffer.
- `InterruptibleRemoteStreamReader::spawn_unblocks_on_cancellation` and
  `interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts`
  pin the two sides of the contract so a future edit that breaks either
  the adapter poll or the spawn path fails a specific regression test.

### Unix terminal clean-exit cleanup is a no-op

On Unix, `TerminalProcessTree::cleanup_after_shell_exit` is intentionally
a no-op on the success path. Once `wait_for_shared_child_exit_timeout` has
reaped the shell, the kernel is free to recycle the shell's PID (and
therefore its process group id), so calling `libc::killpg(process.id(),
SIGKILL)` would race PID reuse and could SIGKILL an unrelated local
process group. Rust's stdlib `Child::send_signal` guards against this
same hazard by early-returning once the child has been reaped.

**Consequence:** a command like `sleep 999 & echo done` will return
successfully, release its terminal-command permit, and leave the
backgrounded `sleep` running outside TermAl's accounting. The backgrounded
grandchild re-parents to init (PID 1), so it is owned by the OS rather
than leaked in TermAl itself, but it is not bounded by the terminal
command timeout or the 429 semaphore.

**Mitigations already in place:**
- On Windows the path is completely covered: the Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` terminates every process assigned
  to it when the shell exits, so backgrounded grandchildren are killed.
- On Unix, the per-stream reader-join timeout
  (`TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`, 5s) bounds how long the terminal
  command waits for each stdio reader to finish even if a backgrounded
  grandchild still holds the inherited stdout/stderr pipes.
  `run_terminal_shell_command_with_timeout` joins stdout and stderr
  **sequentially**, so the pathological success-path reader-join phase
  is up to ~10s (5s per stream), not 5s. That phase runs *after* the
  child wait returns, so on the local path the total worst-case wall
  clock is `TERMINAL_COMMAND_TIMEOUT` (60s child wait) + ~10s (reader
  join), or approximately 70s. On the remote-proxy path the same ~70s
  inner budget fits inside the 90s `REMOTE_TERMINAL_COMMAND_TIMEOUT`
  envelope with ~20s of slack. Tuning `TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`
  should account for both budgets and the 2x sequential multiplier.
- Reader threads write into an `Arc<Mutex<TerminalOutputBuffer>>` shared
  with the main thread (see `read_capped_terminal_output_into` and
  `join_terminal_output_reader` in `src/api.rs`). Each reader signals
  completion via a `sync_channel(1)` so `join_terminal_output_reader`
  blocks in `recv_timeout` instead of polling -- the happy-path wake is
  event-driven, not tick-driven. On a reader-join timeout, the main
  thread snapshots whatever prefix the reader already accumulated and
  returns it marked `output_truncated = true`. Without the shared
  buffer, the main thread dropped the `JoinHandle` and returned
  `String::new()`, so any `echo done` output that had already been
  buffered was silently discarded even though the foreground shell
  produced it. The detached reader thread still runs until the
  backgrounded grandchild closes its inherited pipe end, but no data
  captured up to the timeout is lost.
- The timeout path still calls `killpg` (reserved for the pre-reap window,
  where `process.id()` is guaranteed to still refer to our process), with
  an additional `try_wait` defense against a narrower race where
  `wait_for_shared_child_exit_timeout`'s detached waiter thread reaps the
  shell between `recv_timeout` returning and the kill running.

**Residual cost:** the detached reader thread stays alive until the
backgrounded grandchild closes its inherited pipe end (i.e. until it
exits). Repeating long-lived background commands can therefore accumulate
native threads for as long as each grandchild survives. This is a bounded
leak -- the thread exits when its target exits, no data captured up to
the timeout is lost, and on Windows the Job Object prevents the scenario
entirely -- and closing it would require platform-specific pipe-
interruption primitives that are not currently worth the added
complexity.

**Possible future strategies** (none currently implemented because the
tradeoff isn't obviously worth the complexity):
- Linux-only: use `pidfd_open` for a stable process handle and
  `pidfd_send_signal` for race-free kills. Doesn't help macOS, and doesn't
  give us a group handle anyway.
- Install `PR_SET_CHILD_SUBREAPER` on the TermAl process so backgrounded
  grandchildren re-parent to TermAl rather than init, then track them
  explicitly. Linux-only and complicates the whole process model.
- Use cgroups v2 on Linux for a process-group handle that isn't tied to a
  recyclable PID. Requires root or unified cgroups and still Linux-only.
- Unix-only: `dup` the pipe fds before moving them into the reader
  threads, keep the duplicates in the main thread, and `close` them on
  reader-join timeout. This would force the blocking `read` to return
  `EBADF` / `EOF` and let the detached thread unwind immediately,
  eliminating the thread-accumulation residual cost. Adds platform-
  specific code and nontrivial fd plumbing.

### Terminal 429 peek/resolve race drifts local-vs-remote counters

`run_terminal_command` calls `state.terminal_request_is_remote(...)`
under the state lock to decide which permit (local or remote) to
acquire, then drops the lock, acquires the chosen permit, and only later
resolves the full scope via `remote_scope_for_request` inside
`run_blocking_api`. If a caller's `projectId` is local at the peek but
its remote binding flips between the two calls, the local permit is
consumed for a request that then fails deep inside the blocking task --
or vice versa. Both sides fail closed (mismatches return safely as
`ApiError`), but the 429 counters can transiently diverge from what is
actually in flight on each budget.

**Accepted tradeoff.** Closing this race would require snapshotting the
full resolution (scope, not just a boolean) before acquiring the permit,
which means running `ensure_remote_project_binding` -- a blocking
`reqwest::send` on the first-time-bind path -- on the async worker
thread. The round-99 refactor moved that call onto the blocking pool for
async-safety, and reintroducing it on the async worker is a strictly
worse tradeoff than the rare 429-counter asymmetry it would fix. The
race is documented in a large inline comment in `run_terminal_command`
right above the `terminal_request_is_remote` peek so future readers do
not chase the asymmetric counters as a bug.

### Windows `resume_terminal_process_threads` snapshots every system thread

`resume_terminal_process_threads` on Windows calls
`CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)` on every terminal
command, which enumerates every thread on the entire system (typically
2-5k on a dev workstation, more on busy servers). The function then
iterates to find the subset belonging to the child process.

**Accepted tradeoff.** The `TH32CS_SNAPTHREAD` snapshot kind does not
accept a process-id filter (the `pid` parameter is only honored by the
module snapshot kinds), so there is no cheap way to narrow the snapshot
at the Win32 API layer. Capturing the primary thread handle directly
from `CreateProcess` via `PROCESS_INFORMATION.hThread` and calling
`ResumeThread` on just that one handle would work, but it requires
bypassing `std::process::Child`'s encapsulation -- either with a
crate-level extension trait or a direct `CreateProcess` call that
mirrors stdlib's stdio plumbing. That is a substantially larger
refactor than the roughly 10 microseconds the snapshot costs in practice,
so the current implementation is left as-is with a prominent comment
documenting the reason.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
