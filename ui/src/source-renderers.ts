// Shared renderer model for source-backed previews (see
// `docs/features/source-renderers.md`). Keeps the DETECTION layer
// separate from the RENDERING layer: panels use this module to ask
// "are there any renderable regions in this file?" and "where are
// they in the source?" without depending on `message-cards.tsx`
// internals. Rendering itself still lives in `MarkdownContent` /
// `MermaidDiagram` etc. — this module only produces metadata.
//
// Phase 2 scope: Markdown file detection, Mermaid fence detection
// (tagged ` ```mermaid ` blocks), and math detection (inline `$...$`,
// block `$$...$$`, and fenced ` ```math `/`latex`/`tex`/`katex`).
// Rust doc-comment parsing is Phase 5 and not in this module yet.
// Dedicated file types (`.mmd`, `.mermaid`, `.tex`) are recognized
// at the language-detection layer but only surface a whole-file
// region when the file contents match the renderer's expectations.

/**
 * Render-budget constants shared across the registry and the
 * rendering path in `message-cards.tsx`. Moved here so Source /
 * Diff panels can inspect budgets without importing from
 * `message-cards.tsx` (which pulls in React and the full Mermaid /
 * KaTeX plugin chain).
 */
export const MAX_MERMAID_SOURCE_CHARS = 50_000;
export const MAX_MERMAID_DIAGRAMS_PER_DOCUMENT = 20;
export const MAX_MATH_EXPRESSIONS_PER_DOCUMENT = 100;

export type SourceRendererKind = "markdown" | "mermaid" | "math";
export type SourceRendererMode =
  | "source"
  | "diff"
  | "diff-edit"
  | "markdown-diff";

export interface SourceRenderContext {
  /** Full path or `null` for untitled buffers. Used for extension-
   *  based language detection. */
  path: string | null;
  /** Language slug, if the caller already resolved one (e.g. Monaco
   *  language, Git diff enrichment). `null` falls back to extension-
   *  derived detection. */
  language: string | null;
  /** The source text to inspect. Callers should pass the CURRENT
   *  edit buffer (not the saved-on-disk content) so previews reflect
   *  unsaved edits. */
  content: string;
  /** Context for the caller — lets future renderers tune behavior
   *  (e.g. editable vs read-only) without threading new flags. */
  mode: SourceRendererMode;
}

export interface SourceRenderableRegion {
  /** Stable id for React keys. Content-derived so a re-detection
   *  pass produces identical ids for unchanged regions. */
  id: string;
  renderer: SourceRendererKind;
  /** 1-based source line where the region starts (inclusive). */
  sourceStartLine: number;
  /** 1-based source line where the region ends (inclusive). */
  sourceEndLine: number;
  /** Raw source text of the region (including any fence markers for
   *  fenced-block regions). */
  sourceText: string;
  /** Text the renderer should consume. For ` ```mermaid ` fences
   *  this is the inner code without fence markers; for inline
   *  `$...$` math this is the math body without dollar signs. */
  displayText: string;
  /** Whether the region's source can be edited in the current mode
   *  (e.g. working-tree diff side) or should be read-only. Diff
   *  panels use this to disable edits on the "before" side. */
  editable: boolean;
}

/** Narrow predicate used by the Markdown code-block renderer to
 *  decide whether a given ` ``` ` fence is a Mermaid diagram. */
export function isMermaidFenceLanguage(language: string | null): boolean {
  return language?.trim().toLowerCase() === "mermaid";
}

/** Narrow predicate for the four math fence aliases accepted by
 *  Phase 1 (see `docs/features/source-renderers.md` §Math). */
export function isMathFenceLanguage(language: string | null): boolean {
  const normalized = language?.trim().toLowerCase();
  return (
    normalized === "math" ||
    normalized === "latex" ||
    normalized === "tex" ||
    normalized === "katex"
  );
}

/** O(n) lexical Mermaid-fence counter. Public for callers that only
 *  need the count (e.g. the Markdown renderer's per-document budget
 *  check). Same algorithm as the historic `countMermaidMarkdownFences`
 *  in `message-cards.tsx` — moved here so the Source panel can
 *  short-circuit the expensive region scan when it just needs the
 *  budget answer. */
export function countMermaidFences(markdown: string): number {
  let count = 0;
  for (const region of detectMarkdownFenceRegions(markdown)) {
    if (isMermaidFenceLanguage(region.language)) {
      count += 1;
    }
  }
  return count;
}

/** O(n) lexical math counter: inline `$...$` pairs plus block
 *  `$$...$$` pairs (both same-line and multi-line). Fenced
 *  ` ```math ` blocks are counted by `countMermaidFences`-style
 *  fence detection if the caller needs them separately. Kept
 *  consistent with the region-level `detectRenderableRegions` so
 *  per-document budget checks in the Markdown renderer fire
 *  against the same count that `detectRenderableRegions` would
 *  return. */
export function countMathExpressions(markdown: string): number {
  let count = 0;
  let inFence = false;
  let blockOpen = false;
  for (const line of markdown.split(/\r?\n/)) {
    const fenceMatch = line.match(/^ {0,3}([`~]{3,})/);
    if (fenceMatch) {
      inFence = !inFence;
      continue;
    }
    if (inFence) {
      continue;
    }
    // Multi-line `$$` delimiter on its own line toggles the block
    // state. A closing delimiter ends a block region (count += 1);
    // any inline math on either boundary line is ignored, matching
    // `remark-math`'s tokenization.
    if (/^\s*\$\$\s*$/.test(line)) {
      if (blockOpen) {
        count += 1;
      }
      blockOpen = !blockOpen;
      continue;
    }
    if (blockOpen) {
      continue;
    }
    // Same-line `$$...$$` pairs (e.g. `Block: $$x = 1$$ here.`).
    const sameLineBlocks = line.match(/\$\$[^\n]*?\$\$/g);
    if (sameLineBlocks) {
      count += sameLineBlocks.length;
    }
    // Inline math: `$...$` pairs with no space after the opening
    // `$`. Strip same-line block-math first so `$$x$$` doesn't
    // double-count as two inline `$x$`.
    const inlineScan = line
      .replace(/\$\$[^\n]*?\$\$/g, "")
      .replace(/\$\$/g, "");
    const inlinePairs = inlineScan.match(/\$[^\s$][^$\n]*?\$/g);
    if (inlinePairs) {
      count += inlinePairs.length;
    }
  }
  return count;
}

/** Detects renderable regions given a source context. For Markdown
 *  content, enumerates Mermaid + math fences AND inline / block
 *  math. For dedicated Mermaid files (`.mmd`, `.mermaid`), surfaces
 *  the whole file as a single region. Returns `[]` when no
 *  renderable content is found. */
export function detectRenderableRegions(
  context: SourceRenderContext,
): SourceRenderableRegion[] {
  const kind = detectContentKind(context);
  switch (kind) {
    case "markdown":
      return detectMarkdownRegions(context);
    case "mermaid-file":
      return detectWholeFileMermaidRegion(context);
    case "unknown":
      return [];
  }
}

/** True when the detection layer believes the file has at least one
 *  renderable region. Cheap pre-check the Source panel can use to
 *  decide whether to expose the Preview / Split view-mode buttons at
 *  all. */
export function hasRenderableRegions(context: SourceRenderContext): boolean {
  return detectRenderableRegions(context).length > 0;
}

// ===================================================================
// Internals below. Not exported — callers should use the functions
// above. If a new caller needs more structure, prefer adding a new
// exported helper over reaching into these.
// ===================================================================

type ContentKind = "markdown" | "mermaid-file" | "unknown";

function detectContentKind(context: SourceRenderContext): ContentKind {
  const language = context.language?.trim().toLowerCase();
  const pathExt = extractLowercaseExtension(context.path);

  if (language === "markdown" || language === "md" || pathExt === "md" || pathExt === "markdown") {
    return "markdown";
  }
  if (
    language === "mermaid" ||
    pathExt === "mmd" ||
    pathExt === "mermaid"
  ) {
    return "mermaid-file";
  }
  // Rust (Phase 5), LaTeX (future), other dedicated types land here
  // as a no-op for now.
  return "unknown";
}

function extractLowercaseExtension(path: string | null): string | null {
  if (!path) {
    return null;
  }
  const lastDot = path.lastIndexOf(".");
  if (lastDot < 0 || lastDot === path.length - 1) {
    return null;
  }
  // Handle both `/` and `\` path separators so Windows workspaces
  // compare the same way as POSIX.
  const lastSep = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (lastDot < lastSep) {
    return null;
  }
  return path.slice(lastDot + 1).toLowerCase();
}

interface MarkdownFenceRegion {
  language: string;
  startLine: number; // 1-based
  endLine: number; // 1-based inclusive
  /** Raw text of the fence including open + close marker lines. */
  sourceText: string;
  /** Inner code body without the fence markers (each line's newline
   *  is preserved except the body's trailing blank). */
  body: string;
}

/** Shared fence scanner used by both `countMermaidFences` and the
 *  full region enumerator. A single pass over the lines captures
 *  all fenced blocks with their start / end line numbers, language,
 *  and body. */
function detectMarkdownFenceRegions(markdown: string): MarkdownFenceRegion[] {
  const lines = markdown.split(/\r?\n/);
  const regions: MarkdownFenceRegion[] = [];
  let openFence: {
    language: string;
    markerChar: "`" | "~";
    markerLength: number;
    startLine: number; // 1-based
    bodyLines: string[];
  } | null = null;

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index] ?? "";
    if (openFence) {
      const closeMatch = line.match(/^ {0,3}([`~]{3,})\s*$/);
      const closeMarker = closeMatch?.[1] ?? "";
      if (
        closeMarker.startsWith(openFence.markerChar) &&
        closeMarker.length >= openFence.markerLength
      ) {
        const endLine = index + 1;
        const sourceLines = lines.slice(openFence.startLine - 1, endLine);
        regions.push({
          language: openFence.language,
          startLine: openFence.startLine,
          endLine,
          sourceText: sourceLines.join("\n"),
          body: openFence.bodyLines.join("\n"),
        });
        openFence = null;
        continue;
      }
      openFence.bodyLines.push(line);
      continue;
    }
    const openMatch = line.match(/^ {0,3}([`~]{3,})\s*([^\s`~]*)/);
    if (!openMatch) {
      continue;
    }
    const markerText = openMatch[1] ?? "";
    openFence = {
      language: (openMatch[2] ?? "").trim(),
      markerChar: markerText[0] === "~" ? "~" : "`",
      markerLength: markerText.length,
      startLine: index + 1,
      bodyLines: [],
    };
  }

  return regions;
}

interface MathExpressionRegion {
  mode: "inline" | "block";
  startLine: number;
  endLine: number;
  sourceText: string;
  body: string;
}

/** Locates `$...$` / `$$...$$` regions with source line numbers.
 *  Used by the region enumerator; the cheap
 *  `countMathExpressions` is preferred for budget checks where
 *  line-level metadata isn't needed. */
function detectMathExpressionRegions(markdown: string): MathExpressionRegion[] {
  const regions: MathExpressionRegion[] = [];
  const lines = markdown.split(/\r?\n/);
  let inFence = false;

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const line = lines[lineIndex] ?? "";
    const fenceMatch = line.match(/^ {0,3}([`~]{3,})/);
    if (fenceMatch) {
      inFence = !inFence;
      continue;
    }
    if (inFence) {
      continue;
    }

    const oneBasedLine = lineIndex + 1;

    // Block math: `$$...$$` on a single line. Multi-line block math
    // (`$$` on its own line opening, body, closing `$$`) is handled
    // by a separate pass below to keep the logic readable.
    const singleLineBlockPattern = /\$\$([^\n]*?)\$\$/g;
    let match = singleLineBlockPattern.exec(line);
    while (match) {
      regions.push({
        mode: "block",
        startLine: oneBasedLine,
        endLine: oneBasedLine,
        sourceText: match[0],
        body: match[1] ?? "",
      });
      match = singleLineBlockPattern.exec(line);
    }

    // Inline math on the same line (after removing block-math spans).
    const inlineLine = line
      .replace(/\$\$[^\n]*?\$\$/g, "")
      .replace(/\$\$/g, "");
    const inlinePattern = /\$([^\s$][^$\n]*?)\$/g;
    let inlineMatch = inlinePattern.exec(inlineLine);
    while (inlineMatch) {
      regions.push({
        mode: "inline",
        startLine: oneBasedLine,
        endLine: oneBasedLine,
        sourceText: `$${inlineMatch[1]}$`,
        body: inlineMatch[1] ?? "",
      });
      inlineMatch = inlinePattern.exec(inlineLine);
    }
  }

  // Multi-line block-math pass: walk lines again, pair up standalone
  // `$$` markers. Single-line `$$...$$` was already captured above,
  // so the paired pass only matches lines whose content is EXACTLY
  // `$$` (optionally with leading / trailing whitespace).
  inFence = false;
  let blockOpenLine: number | null = null;
  const blockBodyLines: string[] = [];
  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const line = lines[lineIndex] ?? "";
    const fenceMatch = line.match(/^ {0,3}([`~]{3,})/);
    if (fenceMatch) {
      inFence = !inFence;
      continue;
    }
    if (inFence) {
      continue;
    }
    if (/^\s*\$\$\s*$/.test(line)) {
      const oneBasedLine = lineIndex + 1;
      if (blockOpenLine === null) {
        blockOpenLine = oneBasedLine;
        blockBodyLines.length = 0;
      } else {
        regions.push({
          mode: "block",
          startLine: blockOpenLine,
          endLine: oneBasedLine,
          sourceText: lines
            .slice(blockOpenLine - 1, oneBasedLine)
            .join("\n"),
          body: blockBodyLines.join("\n"),
        });
        blockOpenLine = null;
        blockBodyLines.length = 0;
      }
      continue;
    }
    if (blockOpenLine !== null) {
      blockBodyLines.push(line);
    }
  }

  return regions;
}

function detectMarkdownRegions(
  context: SourceRenderContext,
): SourceRenderableRegion[] {
  const regions: SourceRenderableRegion[] = [];
  const editable = context.mode === "source" || context.mode === "diff-edit";

  for (const fence of detectMarkdownFenceRegions(context.content)) {
    if (isMermaidFenceLanguage(fence.language)) {
      regions.push({
        id: `mermaid:${fence.startLine}:${fence.endLine}:${quickHash(fence.body)}`,
        renderer: "mermaid",
        sourceStartLine: fence.startLine,
        sourceEndLine: fence.endLine,
        sourceText: fence.sourceText,
        displayText: fence.body,
        editable,
      });
      continue;
    }
    if (isMathFenceLanguage(fence.language)) {
      regions.push({
        id: `math:${fence.startLine}:${fence.endLine}:${quickHash(fence.body)}`,
        renderer: "math",
        sourceStartLine: fence.startLine,
        sourceEndLine: fence.endLine,
        sourceText: fence.sourceText,
        displayText: fence.body,
        editable,
      });
    }
  }

  for (const mathRegion of detectMathExpressionRegions(context.content)) {
    regions.push({
      id: `math:${mathRegion.mode}:${mathRegion.startLine}:${mathRegion.endLine}:${quickHash(mathRegion.body)}`,
      renderer: "math",
      sourceStartLine: mathRegion.startLine,
      sourceEndLine: mathRegion.endLine,
      sourceText: mathRegion.sourceText,
      displayText: mathRegion.body,
      editable,
    });
  }

  regions.sort((left, right) => {
    if (left.sourceStartLine !== right.sourceStartLine) {
      return left.sourceStartLine - right.sourceStartLine;
    }
    return left.sourceEndLine - right.sourceEndLine;
  });
  return regions;
}

function detectWholeFileMermaidRegion(
  context: SourceRenderContext,
): SourceRenderableRegion[] {
  const trimmed = context.content.trim();
  if (trimmed.length === 0) {
    return [];
  }
  const totalLines = context.content.split(/\r?\n/).length;
  return [
    {
      id: `mermaid-file:${quickHash(context.content)}`,
      renderer: "mermaid",
      sourceStartLine: 1,
      sourceEndLine: totalLines,
      sourceText: context.content,
      displayText: context.content,
      editable: context.mode === "source" || context.mode === "diff-edit",
    },
  ];
}

/** Tiny FNV-1a-style hash used for stable region ids across
 *  re-detection passes. Not cryptographic — just enough to
 *  distinguish a couple dozen Mermaid/math snippets per document. */
function quickHash(input: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < input.length; index += 1) {
    hash ^= input.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  // Convert to unsigned 32-bit and render as a short hex string.
  return (hash >>> 0).toString(16).padStart(8, "0");
}
