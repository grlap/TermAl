# Feature Brief: Streaming-Aware Markdown Rendering

## Status

Active. Lands in the current tree.

`MarkdownContent` (`ui/src/message-cards.tsx`) gains an `isStreaming`
prop (default `false`). When set, an in-flight trailing structural
block — pipe-table, fenced code block, or `$$ ... $$` math display
block — is rendered as plain text in a styled
`<pre class="markdown-streaming-fragment">` placeholder until the
block closes. The settled prefix continues through the existing
`react-markdown` + `remark-gfm` + `remark-math` + `rehype-katex`
pipeline. Once the block settles, the next `textDelta` re-runs the
splitter and the placeholder snaps to canonical Markdown rendering.

The gating site (`MessageCard` in the same file) decides whether a
streaming assistant message has Markdown structure worth rendering
through this pipeline; if not, it stays on
`StreamingAssistantTextShell`'s plain-text fast path.

Settled callers — history bubbles, source-renderer previews, the
rendered Markdown viewer (`docs/features/markdown-document-view.md`),
and rendered Git diff regions — leave `isStreaming` at its default
`false` and get the existing pipeline unchanged.

## Problem

Streaming assistant replies arrive as a sequence of `textDelta` events.
Each delta extends the message text by some bytes. CommonMark/GFM
rendering is intrinsically structural:

- A pipe-table doesn't commit to a `<table>` until it sees `header +
  separator + body row + trailing blank line`.
- A fenced code block doesn't render correctly until the closing fence
  arrives.
- A `$$ ... $$` display-math block doesn't typeset until the closer
  lands.

Without intervention, the user sees visibly-broken intermediate shapes
flicker on every chunk:

- Raw `| Col | Col |` text rendered as a paragraph until the separator
  row arrives.
- A `<table>` with mismatched cell counts as cells stream in.
- An open fenced code block that consumes the rest of the message body
  until the closer arrives.
- A `$$` block whose content is consumed by `remark-math` and
  re-rendered with each delta.

The user-visible result is a transcript that constantly reflows during
streaming — distracting and harder to read, especially on slower
devices. Tables and math are the most common offenders because the
canonical shape is large and the partial shape looks broken.

## Solution

A small pure module owns the partial-block detection:
`ui/src/markdown-streaming-split.ts`. Its single export,
`splitStreamingMarkdownForRendering(markdown)`, returns
`{ settled, pending }`. The cut policy:

1. Walk lines tracking three structural states:
   - `inFence` — opened by a `` ``` `` or `~~~` line, closed by the
     next matching marker.
   - `inMath` — opened by `$$` on its own line, closed by the next
     `$$` on its own line.
   - `tableStart` — any line starting with `|`; reset on a blank
     line, a non-pipe text line, **or when a fence or `$$` block
     opens** (because the candidate pipe lines were re-interpreted
     as something else, so the table tracking would be a false
     positive).
2. Cut at the **earliest** open-block start. Anything before the cut
   is `settled` and safe for `react-markdown`. Anything after is
   `pending` and renders as plain text.
3. Boundary newlines live at the end of `settled` so callers can
   reconstruct the original via plain `settled + pending`
   concatenation, regardless of which half is empty.

Settled callers pass `isStreaming={false}` (the default) and short-
circuit the splitter — `settled === markdown` and `pending === ""`
for any input.

## Pipeline Integration

`MarkdownContent` runs the splitter inside a `useMemo` keyed on
`[isStreaming, markdown]`. Three downstream concerns key on
`settledMarkdown` rather than the raw `markdown`:

- The Mermaid diagram count
  (`MAX_MERMAID_DIAGRAMS_PER_DOCUMENT = 20`).
- The KaTeX math expression count
  (`MAX_MATH_EXPRESSIONS_PER_DOCUMENT = 100`).
- The line-marker observer scan (`[data-markdown-line-start]`
  elements). The observer's `useEffect` is intentionally scoped to
  the inputs that change the marker set (`settledMarkdown`,
  `normalizedStartLineNumber`, `showLineNumbers`); see the source
  comment in `MarkdownContent` for why `documentPath` /
  `workspaceRoot` / the source-link callback are deliberately
  excluded (avoids O(markers) teardown churn on unrelated context
  changes).

This keeps an in-flight unclosed fence or `$$` block from counting
against per-document caps before its real shape is known. It also
means the placeholder's plain text is excluded from line-marker
tracking (it carries no `data-markdown-line-start` attributes).

The pending half is rendered inside
`<pre className="markdown-streaming-fragment">` (styled in
`ui/src/styles.css`), with `aria-busy="true"` to signal in-flight
content to assistive technology. The styling is deliberately calm —
muted background, monospace, slightly reduced opacity — so the
"in-flight, not the final shape" signal is clear without being noisy.

## MessageCard Gate

`StreamingAssistantTextShell` is the existing plain-text fast path
for streaming assistant replies — used when the message has not yet
shown any sign of Markdown structure. `MessageCard` decides which
path to take via `hasRenderableStreamingMarkdown(text)`, which
detects:

- Headings (`# ` … `###### `)
- Lists (unordered, ordered)
- Blockquotes
- Fenced code blocks
- Inline code spans
- Bold (`**...**`, `__...__`)
- Markdown links (`[text](url)`)

If any of these match, the path switches to
`<MarkdownContent isStreaming />`. Otherwise it stays on the plain
`<p>` shell, which has the lowest possible streaming cost.

**Known limitation:** the current gate does not recognize pipe-table
starts (`| Col |`) or standalone `$$` math openers as renderable
Markdown structure. A stream that begins with a bare table or a math
block stays on the plain-text shell until some other Markdown
construct (heading, list, code span, …) lands on the same message,
at which point it flips. Tracked in
[`docs/bugs.md`](../bugs.md) as "Streaming table/math deferral is
bypassed by the production assistant-message gate".

## Test Coverage

- `ui/src/markdown-streaming-split.test.ts` — pure unit tests for the
  splitter. Coverage areas: the empty / plain-paragraph base cases;
  the pipe-table state machine (progressive partial states plus the
  recovery cases — settled-after-blank-line, prose-with-pipes,
  recovery-after-non-pipe); open/closed fences (including pipes
  inside the fence body and tilde fences); open/closed math; the
  multi-block "earliest cut wins" rule; and a round-trip identity
  invariant (`settled + pending === markdown`). The test file is the
  authoritative case list — refer to it when extending the splitter.
- `ui/src/MarkdownContent.test.tsx` — `"isStreaming partial-table
  deferral"` describe block (7 cases): three progressive partial-
  table states (header alone, header + separator, header + separator
  + partial body row) render the placeholder and not a `<table>`;
  the fourth state (trailing blank line settles the block) snaps to
  a real `<table>` and asserts the placeholder is gone; settled
  paragraph + partial table renders both halves; settled callers
  without `isStreaming` are unchanged; unclosed fenced code blocks
  defer too.

## Files

- `ui/src/markdown-streaming-split.ts` — splitter, with header
  comment and JSDoc.
- `ui/src/markdown-streaming-split.test.ts` — splitter unit tests.
- `ui/src/message-cards.tsx` — `MarkdownContent`'s `isStreaming`
  prop integration plus the `MessageCard` gate
  (`hasRenderableStreamingMarkdown`,
  `StreamingAssistantTextShell`).
- `ui/src/MarkdownContent.test.tsx` — integration tests.
- `ui/src/styles.css` — `.markdown-streaming-fragment` styling.

## Related

- [`./markdown-document-view.md`](./markdown-document-view.md) —
  rendered Markdown viewer + Git-diff editor that share
  `MarkdownContent`. Streaming applies only to live assistant
  bubbles; settled viewers and diffs leave `isStreaming` at its
  default `false`.
- [`./markdown-themes-and-styles.md`](./markdown-themes-and-styles.md)
  — Markdown theme + style preferences. The streaming-fragment
  placeholder uses the active code-block colors so it visually
  communicates "in-flight" while staying on the chosen palette.
- [`./source-renderers.md`](./source-renderers.md) — per-region
  renderable previews. The rendered diff view passes
  `isStreaming={false}` (regions are settled by the time the diff
  reaches the viewer).
- [`../bugs.md`](../bugs.md) — active follow-ups, including the
  MessageCard gate limitation and the CommonMark closing-fence-rule
  strictness.
