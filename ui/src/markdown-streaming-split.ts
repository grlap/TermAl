// Streaming-aware Markdown splitter. Owned by streaming assistant
// rendering (see `MarkdownContent` in `ui/src/message-cards.tsx`):
// when a partial Markdown payload contains an unclosed fenced code
// block, an unclosed math display block, or a pipe-table that has
// not yet been terminated by a blank line, those trailing bytes
// render visibly broken (raw `| ... |` text, table rows with
// mismatched cell counts, runaway code blocks). This module finds
// the boundary between the settled prefix and the in-flight
// trailing block so the renderer can pass the prefix through
// `react-markdown` (full GFM rendering) and render the trailing
// fragment as plain text until it settles.
//
// What this owns:
//   - `splitStreamingMarkdownForRendering(markdown)` — pure
//     function returning `{ settled, pending }`. `settled` is the
//     prefix that is safe to render as Markdown; `pending` is the
//     trailing fragment that should be rendered as raw text. For
//     a fully-settled input both halves are well-defined: `settled`
//     is the entire input, `pending` is empty.
//
// What it deliberately does NOT own:
//   - The actual rendering of either half — lives in
//     `message-cards.tsx::MarkdownContent`. That site decides how
//     to display the pending fragment (currently a styled `<pre>`
//     via `.markdown-streaming-fragment`).
//   - Inline-level partial markup (`**bold`, `` `code ``, partial
//     `[link]`, etc.) — CommonMark renders those as literal text
//     until closed, which is acceptable visual behavior; we don't
//     defer for them.
//   - The `isStreaming` gating that decides whether to apply this
//     split at all — that lives at the call site so static
//     callers (settled history, source-renderer previews, diff
//     views) get the existing pipeline unchanged.
//
// New file in this commit; not split out of an existing module.

/**
 * Split a (possibly in-flight) Markdown payload into a settled
 * prefix and a trailing pending fragment.
 *
 * Three structural Markdown blocks render visibly broken when
 * partial and are detected here:
 *   1. Pipe-tables  — a sequence of lines starting with `|` that
 *      has not been closed by a blank line. GFM only commits to a
 *      `<table>` once it has seen header + separator + at least
 *      one body row terminated by a newline; mid-stream rows can
 *      have mismatched cell counts.
 *   2. Fenced code blocks (```...``` and ~~~...~~~) without a
 *      matching closing fence — everything after the opener
 *      renders as runaway code/markdown.
 *   3. Math display blocks (`$$...$$` on its own line) without a
 *      matching closing `$$` — everything after the opener gets
 *      consumed by `remark-math`.
 *
 * The returned `pending` may include all three (e.g., a fenced
 * code block opened during the table's pending region). The cut
 * line is the *earliest* of the three open-block start positions.
 *
 * Pure: depends only on the input string; safe to memoize on
 * `markdown` identity.
 */
export function splitStreamingMarkdownForRendering(
  markdown: string,
): { settled: string; pending: string } {
  if (markdown.length === 0) {
    return { settled: "", pending: "" };
  }

  const lines = markdown.split("\n");
  let inFence = false;
  let fenceOpenLineIndex = -1;
  let fenceMarkerChar: "`" | "~" | null = null;
  let fenceMarkerLength = 0;
  let inMath = false;
  let mathOpenLineIndex = -1;
  let tableStartLineIndex = -1;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();

    // Fenced code blocks (``` or ~~~). Track the opener so only
    // CommonMark-compatible closers with the same marker character
    // and enough length can close it.
    if (!inMath) {
      const fenceMatch = trimmed.match(/^(`{3,}|~{3,})(.*)$/);
      if (fenceMatch) {
        const marker = fenceMatch[1];
        const markerChar = marker.startsWith("`") ? "`" : "~";
        if (!inFence) {
          inFence = true;
          fenceOpenLineIndex = i;
          fenceMarkerChar = markerChar;
          fenceMarkerLength = marker.length;
          // Opening a fence cancels any in-flight table tracking;
          // the table didn't actually exist (its lines were
          // re-interpreted as something else).
          tableStartLineIndex = -1;
        } else if (
          markerChar === fenceMarkerChar &&
          marker.length >= fenceMarkerLength &&
          fenceMatch[2].trim() === ""
        ) {
          inFence = false;
          fenceOpenLineIndex = -1;
          fenceMarkerChar = null;
          fenceMarkerLength = 0;
        }
        continue;
      }
    }
    if (inFence) {
      // Inside a fence: nothing else applies. The fence body can
      // contain blank lines, pipes, `$$`, and they're all literal
      // code content.
      continue;
    }

    // Math display block delimiter (`$$` on its own line). We
    // intentionally only honor the standalone-line form; inline
    // `$$ ... $$` on one line settles on that same line and is
    // not visibly broken when partial.
    if (trimmed === "$$") {
      if (!inMath) {
        inMath = true;
        mathOpenLineIndex = i;
        tableStartLineIndex = -1;
      } else {
        inMath = false;
        mathOpenLineIndex = -1;
      }
      continue;
    }
    if (inMath) {
      continue;
    }

    // Blank line: closes any active pipe-table. A new table can
    // start after the blank.
    if (trimmed === "") {
      tableStartLineIndex = -1;
      continue;
    }

    // Pipe-line: starts or continues a potential pipe-table block.
    // We use a generous detector — any line starting with `|` —
    // because GFM's actual table-recognition rule (header +
    // separator + body) is structural and requires lookahead. The
    // generous detector means we'll defer some trailing lines that
    // GFM wouldn't have rendered as a table anyway (e.g., a single
    // `| not actually a table` line); that's harmless because they
    // re-render correctly once the trailing `\n` makes them part
    // of a settled paragraph.
    if (/^\s*\|/.test(line)) {
      if (tableStartLineIndex === -1) {
        tableStartLineIndex = i;
      }
      continue;
    }

    // Regular text line. If a pipe-table was being tracked, the
    // non-pipe line ends it (the table is done; this line is part
    // of a new paragraph). Don't defer.
    tableStartLineIndex = -1;
  }

  // The cut line is the earliest open-block start. Any block that
  // opened and then closed before end-of-input is fully settled.
  let cutLine = lines.length;
  if (inFence && fenceOpenLineIndex !== -1) {
    cutLine = Math.min(cutLine, fenceOpenLineIndex);
  }
  if (inMath && mathOpenLineIndex !== -1) {
    cutLine = Math.min(cutLine, mathOpenLineIndex);
  }
  if (tableStartLineIndex !== -1) {
    cutLine = Math.min(cutLine, tableStartLineIndex);
  }

  if (cutLine >= lines.length) {
    return { settled: markdown, pending: "" };
  }

  // The boundary newline between the last settled line and the
  // first pending line lives at the end of `settled`, so callers
  // can reconstruct the original via plain `settled + pending`
  // concatenation regardless of which half is empty. When
  // `cutLine === 0` the settled half is empty and there is no
  // boundary to preserve.
  const settledBody = lines.slice(0, cutLine).join("\n");
  const settled = cutLine > 0 ? `${settledBody}\n` : "";
  const pending = lines.slice(cutLine).join("\n");
  return { settled, pending };
}
