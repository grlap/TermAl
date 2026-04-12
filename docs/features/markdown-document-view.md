# Feature Brief: Markdown Document View

Backlog source: proposed feature brief; not yet linked from `docs/bugs.md`.

## Problem

TermAl can edit normal files and can inspect diffs, but Markdown is still treated mostly as plain
source text in file and diff workflows. Agent messages already render Markdown, but that renderer is
not available as a document preview for `.md` files or Markdown diffs.

This creates a gap when reviewing docs, plans, READMEs, changelogs, prompts, and agent-generated
Markdown:

- the source editor is accurate but hard to read
- the diff view shows text changes but not the rendered document impact
- editing a Markdown file or a Markdown diff has no live rendered feedback
- advanced GitHub-flavored Markdown features are not validated in the document workflow

## Goals

- Add a better Markdown view for normal source files.
- Add a better Markdown view for diff previews.
- Support editing normal Markdown files with live preview.
- Support editing Markdown diff targets with live preview.
- Reuse the current save, stale-file, conflict, and rebase protections.
- Keep the Markdown renderer shared so agent messages, file previews, and diff previews do not drift.

## Non-goals for v1

- No WYSIWYG editor in the first version.
- No custom Markdown dialect editor.
- No remote image proxy in the first version unless security or CORS forces it.
- No full rendered visual diff engine in the first version.
- No notebook-style executable code blocks.

## Core experience

### Normal Markdown files

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
- Disk-change and stale-save protections continue to use the existing source-file pipeline.

### Markdown diffs

When the active diff target is Markdown, add a Markdown-aware view:

- `All`
- `Changes`
- `Markdown`
- `Edit`
- `Raw`

Rules:

- `Markdown` renders the post-patch document when file content is available.
- `Edit` keeps the current editable buffer and can show a rendered preview.
- If only unified diff text is available, the UI should explain that rendered preview needs the
  source file content.
- The initial version may render the final document without inline rendered-change highlighting.
- A later version can highlight changed rendered blocks.

## Markdown Feature Requirements

### Required for v1

- [ ] Paragraphs, headings, emphasis, strong text, inline code, and blockquotes.
- [ ] Ordered and unordered lists.
- [ ] Task lists with `- [ ]` and `- [x]`.
- [ ] Tables, including horizontal scrolling for wide tables.
- [ ] Fenced code blocks with syntax highlighting.
- [ ] Inline code styling.
- [ ] Links:
  - [ ] external links
  - [ ] relative workspace links
  - [ ] file links with line and column fragments
  - [ ] heading anchors inside the same document
- [ ] Images:
  - [ ] remote images
  - [ ] relative local images from the workspace
  - [ ] alt text fallback
  - [ ] broken-image fallback
- [ ] Horizontal rules.
- [ ] Escaped Markdown characters.
- [ ] Soft and hard line breaks.
- [ ] Copy button for code blocks.
- [ ] Safe URL handling for links and images.

### Advanced follow-up

- [ ] Frontmatter rendering or metadata panel for YAML/TOML frontmatter.
- [ ] Footnotes.
- [ ] Definition lists if the selected Markdown stack supports them.
- [ ] Strikethrough.
- [ ] Autolinks for raw URLs.
- [ ] Generated table of contents from headings.
- [ ] Stable heading IDs and deep links.
- [ ] Scroll sync between source and preview in split mode.
- [ ] Preserve preview scroll position while editing.
- [ ] Find/search highlighting inside rendered preview.
- [ ] Rendered preview print/export path.
- [ ] Mermaid diagrams.
- [ ] Math blocks and inline math.
- [ ] GitHub-style alerts or admonitions.
- [ ] HTML handling policy:
  - [ ] decide whether raw HTML is escaped, sanitized, or rendered
  - [ ] document the security tradeoff
- [ ] Image sizing controls where Markdown or HTML provides dimensions.
- [ ] Drag and drop or paste image workflow for local Markdown docs.
- [ ] Accessibility pass for rendered documents:
  - [ ] heading order
  - [ ] table semantics
  - [ ] image alt text
  - [ ] keyboard navigation
  - [ ] focus rings for links and interactive task items

## Editing Task Lists

Task lists need a product decision because rendered checkboxes can imply direct editing.

V1 behavior:

- Render checkboxes as read-only in preview mode.
- Editing happens in source mode.
- In split mode, checking boxes in source updates preview live.

Future behavior:

- Allow clicking a rendered task checkbox to update the backing Markdown source.
- Preserve indentation and list marker style.
- Update only the matching task line.
- Avoid direct rendered editing when the preview is based on a diff result that is not currently
  editable.

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
- Ctrl/Cmd click can open in a new tab when supported by the workspace navigation layer.
- External links open through the browser.
- Unsafe schemes are blocked.

## Images

Image support should be useful but controlled.

Rules:

- Relative local images resolve against the Markdown file's directory.
- Remote images render with normal browser loading.
- Missing images show a compact broken-image placeholder with the alt text and source path.
- Very large images are constrained to the document width.
- Image rendering must not resize surrounding layout unexpectedly after load.

Open question:

- Should local images be served through an existing `/api/file` style endpoint, a new asset endpoint,
  or converted to object URLs after fetching bytes?

## Rendering Architecture

Create a shared component:

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
};
```

Responsibilities:

- Wrap the existing `MarkdownContent` behavior.
- Resolve links relative to `documentPath`.
- Resolve local images relative to `documentPath`.
- Provide document-level layout, scrolling, and empty states.
- Keep syntax highlighting and table wrappers consistent with message Markdown.

## Source Panel Integration

Add Markdown view state inside `SourcePanel`:

```ts
type SourceDocumentMode = "code" | "preview" | "split";
```

Rules:

- Only show the mode switcher for Markdown files.
- Default to `Code` for existing behavior.
- Remember the last selected Markdown mode in local UI state or workspace layout.
- Preview uses `editorValue`, not `fileState.content`, so unsaved edits render immediately.
- Save, reload, compare, rebase, and stale-write actions remain source-buffer actions.

## Diff Panel Integration

Extend diff view mode:

```ts
type DiffViewMode = "all" | "changes" | "markdown" | "edit" | "raw";
```

Rules:

- Only show `Markdown` for Markdown targets.
- Prefer rendering the post-patch document from `previewSourceContent` plus the parsed diff.
- In edit mode, render `editValue` in a split preview when the user chooses it.
- If content is unavailable, show a clear unavailable state instead of silently falling back to raw
  diff text.

## Testing

Automated tests:

- [ ] `MarkdownDocumentView` renders task lists, tables, code blocks, links, and images.
- [ ] Unsafe links are blocked.
- [ ] Relative file links resolve against `documentPath`.
- [ ] Source Markdown preview updates from unsaved `editorValue`.
- [ ] Non-Markdown source files do not show Markdown modes.
- [ ] Diff Markdown mode appears only for Markdown targets.
- [ ] Diff edit preview renders `editValue`.
- [ ] Wide tables do not overflow the pane.
- [ ] Broken images render fallback UI.

Manual checks:

- [ ] README with headings, task lists, tables, code, links, and images.
- [ ] Large Markdown file performance.
- [ ] Dirty source file with split preview, then external disk edit.
- [ ] Markdown diff opened from an agent file-edit card.
- [ ] Markdown diff edit save, stale save, and rebase flows.
- [ ] Keyboard navigation through preview links and controls.

## Delivery Plan

### Phase 1: Shared Preview

- Add `MarkdownDocumentView`.
- Add source-file `Preview` and `Split` modes for Markdown.
- Reuse existing `MarkdownContent` and source-link handling.
- Add tests for links, tables, code, and task lists.

### Phase 2: Diff Preview

- Add `Markdown` diff mode for Markdown files.
- Render post-patch content when source content is available.
- Add unavailable state when source content is missing.
- Add tests for Markdown diff mode visibility and rendering.

### Phase 3: Editing Enhancements

- Add split preview in Markdown diff edit mode.
- Persist preferred Markdown mode per workspace pane or tab.
- Preserve preview scroll position during editing.
- Add optional scroll sync.

### Phase 4: Rich Markdown Extras

- Add local image loading.
- Add frontmatter handling.
- Add footnotes, alerts, diagrams, and math if the Markdown stack supports them cleanly.
- Decide and implement raw HTML policy.
