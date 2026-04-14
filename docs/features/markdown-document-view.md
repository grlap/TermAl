# Feature Spec: Markdown Document View

## Status

Planned.

Agent messages already render Markdown through the shared message renderer. Normal `.md` and `.mdx`
files, and especially Markdown files opened through Git diff, still need a dedicated document
preview workflow.

Git diff support is a v1 requirement. A Markdown viewer that works only for normal source files is
not complete enough for TermAl's review workflow.

## Problem

TermAl can edit files and inspect diffs, but Markdown is still treated mostly as source text outside
agent messages. That makes docs, READMEs, plans, changelogs, prompts, and generated Markdown harder
to review.

The highest-value workflow is Git diff:

- staged documentation changes should render as the document that is staged for commit
- unstaged documentation changes should render as the current working tree document
- reviewers need a readable document preview while still being able to inspect exact changed lines
- patch-only previews must be clearly labeled so users do not confuse partial context for the full
  document

## Existing Behavior To Preserve

The Git diff selection semantics are already correct and must not change:

- `unstaged` compares index -> working tree
- `staged` compares `HEAD` -> index
- untracked files are treated as new files in the unstaged section
- the raw unified diff remains the source of truth for exact line changes

The Markdown viewer must follow the same comparison semantics. In particular, staged Markdown preview
must render the index version, not a later working tree version that may include additional unstaged
edits.

## Goals

- Add a rendered Markdown view for normal source files.
- Add a rendered Markdown view for Git diff previews.
- Make staged and unstaged Markdown previews match the existing Git section semantics exactly.
- Support live preview while editing normal Markdown files.
- Support preview while editing Markdown diff targets.
- Reuse the current save, stale-file, conflict, and rebase protections.
- Reuse the current Markdown renderer so agent messages, file previews, and diff previews do not
  drift.

## Non-goals For V1

- No WYSIWYG editor.
- No full rendered visual diff engine with inline rendered block annotations.
- No notebook-style executable code blocks.
- No broad Markdown dialect work beyond GitHub-flavored Markdown already used by messages.
- No change to the current Git staged/unstaged diff semantics.
- No requirement to render every advanced GitHub extension in v1.

## Product Model

Markdown has two separate viewing contexts:

1. Source document preview for an actual file buffer.
2. Diff document preview for the before/after sides of a Git or agent patch.

These contexts can share the renderer, but they cannot share all data loading rules. A source
document preview renders the current editor buffer. A Git diff preview renders a Git comparison side.

## Normal Markdown Files

When the active source file is Markdown, show a view switcher:

- `Code`
- `Preview`
- `Split`

Rules:

- `Code` keeps the current Monaco editor.
- `Preview` renders the current editor buffer.
- `Split` shows Monaco and rendered preview side by side.
- Dirty state is still based on the source buffer.
- Save still writes Markdown source text.
- Disk-change, stale-save, conflict, compare, and rebase protections continue to use the existing
  source-file pipeline.
- Preview uses the unsaved editor value, not the last-loaded file content.
- The last selected Markdown source mode can be remembered per tab or pane, but `Code` is an
  acceptable first default.

## Git Markdown Diffs

When the active diff target is Markdown, add a Markdown-aware view to the existing diff panel:

- `All`
- `Changes`
- `Markdown`
- `Edit`
- `Raw`

Rules:

- Show `Markdown` only for `.md` and `.mdx` targets, or when the resolved language is `markdown`.
- `Raw` continues to show the exact unified diff.
- `Changes` continues to show structured line changes and review comments.
- `All` continues to show the code diff editor.
- `Edit` continues to edit the applicable live file buffer.
- `Markdown` renders document sides for review; it does not replace `Changes` for exact line-level
  inspection.

### Git Side Semantics

The rendered Markdown document must match the selected Git section:

| Section | Before Side | After Side |
| --- | --- | --- |
| `unstaged` | index | working tree |
| `staged` | `HEAD` | index |
| untracked in `unstaged` | empty document | working tree |
| added in `staged` | empty document | index |

This matters most when a file has both staged and unstaged changes. The staged Markdown preview must
not render the working tree after side, because that may include edits that are not staged for commit.

### Required Diff Data Contract

Markdown diff view needs document-side content, not only patch text.

The implementation should use the same source of truth already used for correct Git file comparison.
If the current file-view pipeline already exposes before/after side contents, the Markdown diff view
should consume that pipeline directly.

If only unified diff text is available at the diff panel boundary, add a narrow document-side data
contract to the existing Git diff response or a companion endpoint:

```ts
type GitDiffDocumentSide = {
  content: string;
  source: "head" | "index" | "worktree" | "empty" | "patch";
};

type GitDiffDocumentContent = {
  before: GitDiffDocumentSide;
  after: GitDiffDocumentSide;
  isCompleteDocument: boolean;
  note?: string | null;
};
```

Expected behavior:

- Git-backed staged diff returns `before.source = "head"` and `after.source = "index"`.
- Git-backed unstaged diff returns `before.source = "index"` and `after.source = "worktree"`.
- Untracked or added files use `before.source = "empty"`.
- Agent patch previews may omit Git-backed sides and fall back to patch reconstruction.
- Patch reconstruction is allowed only as a labeled fallback, not as a silent full-document preview.

### Markdown Diff View UX

Inside `Markdown`, render one document-shaped change view.

Rules:

- Do not show `Changes`, `After`, or `Before` sub-modes inside the Markdown view.
- Unchanged Markdown renders as normal document content in order.
- Deleted Markdown renders inline in a red section at the point where it was removed.
- Added Markdown renders inline in a green section at the point where it lands.
- When a changed block has both deleted and added content, render the red deleted section first and
  the green added section second.
- The result should read top-to-bottom like one document, not like a side-by-side comparison.

Optional follow-up:

- `Split` rendered before/after view for wide panes.
- Smarter block-aware grouping for complex Markdown structures like tables and nested lists.

Status labels:

- Show `Rendered Markdown`.
- Show `Staged` or `Unstaged` when opened from Git status.
- Show `Full document` when side content is complete.
- Show `Patch preview` when reconstructed from unified diff only.
- Show `One document` when the rendered red/green change view is active.

Patch-only fallback text:

> Rendered from patch context only. Unchanged document sections outside the diff are omitted.

Do not render raw unified diff text as Markdown.

## Agent Markdown Diffs

Agent diff cards often provide only `filePath`, `summary`, `diff`, and `changeType`. For those cards:

- If a live file path is available and patch expansion succeeds, render a full post-patch document.
- If expansion fails or no live file is available, render a patch-only Markdown preview with the
  fallback label.
- Do not claim staged/unstaged semantics for agent diffs unless the diff came from Git status.
- Keep `Raw` available for exact patch inspection.

## Editing Markdown Diff Targets

`Edit` mode keeps the existing diff editing behavior:

- Load the live editable file buffer.
- Preserve stale-save and rebase protections.
- Save writes Markdown source text.
- For Markdown files, optionally offer `Code`, `Preview`, and `Split` inside edit mode.
- The edit preview renders `editValue`, not a Git side snapshot.

Important distinction:

- `Markdown` mode is for reviewing before/after diff sides.
- `Edit` mode is for editing the current live file.

For staged Git diffs, editing the live file can include unstaged content. The UI should keep the
`Staged` review view and live edit view conceptually separate.

## Markdown Feature Requirements

Required for v1:

- Paragraphs, headings, emphasis, strong text, inline code, and blockquotes.
- Ordered and unordered lists.
- Task lists with `- [ ]` and `- [x]`.
- Tables with horizontal scrolling for wide tables.
- Fenced code blocks with syntax highlighting.
- Inline code styling.
- External links.
- Relative workspace links.
- File links with line and column fragments.
- Heading anchors inside the same document.
- Horizontal rules.
- Escaped Markdown characters.
- Soft and hard line breaks.
- Copy button for code blocks.
- Safe URL handling for links and images.

Images:

- Remote images can render with normal browser loading.
- Relative local images should resolve against the Markdown file's directory.
- Missing or blocked images should show a compact fallback with alt text and source path.
- Large images must be constrained to the document width.

If local image loading needs a backend endpoint, it can be delivered after the text-first Git diff
workflow. The viewer must still behave correctly when images cannot be loaded.

Advanced follow-up:

- Frontmatter rendering or metadata panel for YAML/TOML frontmatter.
- Footnotes.
- Definition lists if supported cleanly.
- Generated table of contents from headings.
- Stable heading IDs and deep links.
- Scroll sync between source and preview in split mode.
- Preserve preview scroll position while editing.
- Find/search highlighting inside rendered preview.
- Rendered preview print/export path.
- Mermaid diagrams.
- Math blocks and inline math.
- GitHub-style alerts or admonitions.
- Raw HTML policy: escaped, sanitized, or rendered.
- Image sizing controls where Markdown or HTML provides dimensions.
- Drag and drop or paste image workflow for local Markdown docs.

## Task Lists

Rendered task checkboxes are read-only in v1.

Rules:

- Editing happens in source mode.
- In split mode, source edits update the preview live.
- Checking a rendered box does not mutate source text in v1.

Future behavior can allow clickable rendered task checkboxes, but only when the viewer is backed by an
editable source buffer. Patch previews and Git side snapshots should remain read-only.

## Links

Markdown document links should reuse the existing source-link behavior from message Markdown.

Supported targets:

- absolute web URLs
- relative workspace paths
- absolute local paths when already present in Markdown
- fragments like `#heading`
- source locations like `src/app.ts#L24`, `src/app.ts:24`, and `src/app.ts#L24C8`

Rules:

- Workspace file links open in TermAl source tabs.
- Relative links resolve against the Markdown document path.
- Diff document links resolve against the changed file's directory.
- Ctrl/Cmd click can open in a new tab when supported by the workspace navigation layer.
- External links open through the browser.
- Unsafe schemes are blocked.

## Rendering Architecture

Create a shared document wrapper around the current message Markdown renderer:

```ts
type MarkdownDocumentViewProps = {
  markdown: string;
  workspaceRoot?: string | null;
  documentPath?: string | null;
  onOpenSourceLink?: (target: {
    path: string;
    line?: number;
    column?: number;
    openInNewTab?: boolean;
  }) => void;
  variant?: "source" | "diff";
  completeness?: "full" | "patch";
  note?: string | null;
};
```

Responsibilities:

- Wrap the existing `MarkdownContent` behavior.
- Resolve links relative to `documentPath`.
- Resolve local images relative to `documentPath` when supported.
- Provide document-level layout, scrolling, and empty states.
- Keep syntax highlighting and table wrappers consistent with message Markdown.
- Display a visible note when content is patch-only.

## Source Panel Integration

Add Markdown view state inside `SourcePanel`:

```ts
type SourceDocumentMode = "code" | "preview" | "split";
```

Rules:

- Only show the mode switcher for Markdown files.
- Preview uses `editorValue`, not stale file state.
- Save, reload, compare, rebase, and stale-write actions remain source-buffer actions.
- Source preview scroll position should be preserved across editor updates when practical.

## Diff Panel Integration

Extend diff view mode:

```ts
type DiffViewMode = "all" | "changes" | "markdown" | "edit" | "raw";
```

Rules:

- Only show `Markdown` for Markdown targets.
- Use Git side document content first when the diff came from Git status.
- Use patch expansion only as a fallback, and label it as patch-only.
- Do not use live worktree content as the staged after-side unless that content is known to represent
  the index side.
- Keep `Raw` and `Changes` reachable from the same panel.
- In edit mode, render `editValue` in a split preview when selected.

Default mode:

- Existing default behavior can remain for non-Markdown files.
- For Markdown Git diff tabs, defaulting to `Markdown` is acceptable once complete side content is
  available.
- If complete side content is unavailable, default to `Changes` or `All` and show why Markdown preview
  is patch-only.

## Testing

Automated tests:

- `MarkdownDocumentView` renders task lists, tables, code blocks, links, and images.
- Unsafe links are blocked.
- Relative file links resolve against `documentPath`.
- Source Markdown preview updates from unsaved `editorValue`.
- Non-Markdown source files do not show Markdown modes.
- Diff Markdown mode appears only for Markdown targets.
- Git unstaged Markdown preview renders index -> working tree.
- Git staged Markdown preview renders `HEAD` -> index, even when the working tree has additional
  unstaged edits.
- Untracked Markdown preview renders empty -> working tree.
- Added staged Markdown preview renders empty -> index.
- Patch-only fallback renders a visible incomplete-preview note.
- Diff edit preview renders `editValue`.
- Wide tables do not overflow the pane.
- Broken images render fallback UI.

Manual checks:

- README with headings, task lists, tables, code, links, and images.
- Large Markdown file performance.
- Dirty source file with split preview, then external disk edit.
- Staged Markdown file with additional unstaged edits in the same file.
- Unstaged Markdown file with staged baseline changes.
- Markdown diff opened from an agent file-edit card.
- Markdown diff edit save, stale save, and rebase flows.
- Keyboard navigation through preview links and controls.

## Delivery Plan

### Phase 1: Shared Preview

- Add `MarkdownDocumentView`.
- Add source-file `Preview` and `Split` modes for Markdown.
- Reuse existing `MarkdownContent` and source-link handling.
- Add tests for links, tables, code, task lists, and unsafe URLs.

### Phase 2: Git Diff Preview

- Add `Markdown` diff mode for Markdown files.
- Wire Markdown diff preview to Git section side content.
- Preserve current staged and unstaged comparison semantics.
- Add patch-only fallback with a visible note.
- Add tests for staged, unstaged, untracked, and added Markdown diffs.

### Phase 3: Editing Enhancements

- Add split preview in Markdown diff edit mode.
- Persist preferred Markdown mode per workspace pane or tab.
- Preserve preview scroll position during editing.
- Add optional scroll sync.

### Phase 4: Rich Markdown Extras

- Add local image loading if not included in v1.
- Add frontmatter handling.
- Add footnotes, alerts, diagrams, and math if the Markdown stack supports them cleanly.
- Decide and implement raw HTML policy.
