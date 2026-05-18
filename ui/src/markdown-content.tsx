// Owns MarkdownContent and the rendered Markdown/Mermaid/KaTeX runtime.
// Deliberately does not own generic message-card routing or card chrome; this
// was split out of `message-cards.tsx` as a pure code move.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import "katex/dist/katex.min.css";
import { getErrorMessage } from "./app-utils";
import { HighlightedCodeBlock } from "./highlighted-code-block";
import {
  buildMarkdownHrefDisplayLabel,
  isExternalMarkdownHref,
  MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE,
  resolveMarkdownFileLinkTarget,
  shouldScrubMarkdownDomHref,
  transformMarkdownLinkUri,
  type MarkdownFileLinkTarget,
} from "./markdown-links";
import { remarkAutolinkBareFileReferences } from "./markdown-bare-file-autolinks";
import {
  areMarkdownLineMarkersEqual,
  getMarkdownLineAttributes,
  normalizeMarkdownStartLineNumber,
  type MarkdownLineAttributes,
  type MarkdownLineMarker,
  type MarkdownSourcePosition,
} from "./markdown-line-markers";
import { splitStreamingMarkdownForRendering } from "./markdown-streaming-split";
import {
  buildMermaidDiagramFrameSrcDoc,
  getMermaidDiagramFrameStyle,
  renderMermaidDiagramWithBundleFallback,
} from "./mermaid-render";
import type { MonacoAppearance } from "./monaco";
import {
  highlightReactNodeText,
  type SearchHighlightTone,
} from "./search-highlight";
import {
  MAX_MATH_EXPRESSIONS_PER_DOCUMENT,
  MAX_MERMAID_DIAGRAMS_PER_DOCUMENT,
  MAX_MERMAID_SOURCE_CHARS,
  countMathExpressions,
  countMermaidFences,
  isMermaidFenceLanguage,
} from "./source-renderers";

let mermaidDiagramIdCounter = 0;

function MermaidDiagram({
  appearance,
  code,
  fillAvailableSpace = false,
  lineAttributes,
  showSourceOnError = true,
}: {
  appearance: MonacoAppearance;
  code: string;
  fillAvailableSpace?: boolean;
  lineAttributes?: MarkdownLineAttributes | null;
  showSourceOnError?: boolean;
}) {
  const [renderState, setRenderState] = useState<
    | { error: null; status: "loading"; svg: null }
    | { error: null; status: "ready"; svg: string }
    | { error: string; status: "error"; svg: null }
  >({ error: null, status: "loading", svg: null });

  useEffect(() => {
    let cancelled = false;
    const diagramId = `termal-mermaid-${++mermaidDiagramIdCounter}`;

    setRenderState({ error: null, status: "loading", svg: null });
    void renderMermaidDiagramWithBundleFallback({
      appearance,
      code,
      diagramId,
    })
      .then(({ svg }) => {
        if (!cancelled) {
          setRenderState({ error: null, status: "ready", svg });
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setRenderState({
            error: getErrorMessage(error),
            status: "error",
            svg: null,
          });
        }
      });

    return () => {
      cancelled = true;
    };
  }, [appearance, code]);

  // Memoise the iframe's srcDoc and style so unrelated parent
  // re-renders (e.g. a post-save diff-view refresh) don't reload the
  // iframe. `buildMermaidDiagramFrameSrcDoc` returns a fresh string
  // every call, so without this memo React sees a new `srcDoc` prop
  // identity on every render and the browser reloads the iframe —
  // a full re-parse and re-paint that visibly flickers the diagram
  // even when the SVG content is unchanged. Keyed on the SVG string
  // so an actual re-render (different diagram) still propagates.
  //
  // These useMemo hooks sit BEFORE the error-branch early return so
  // they run in the same order on every render (rules of hooks); the
  // value is null/undefined when no SVG is available and the
  // ready-branch JSX below guards on `iframeSrcDoc !== null` before
  // mounting the iframe.
  const readySvg = renderState.status === "ready" ? renderState.svg : null;
  const iframeSrcDoc = useMemo(
    () =>
      readySvg === null
        ? null
        : buildMermaidDiagramFrameSrcDoc(readySvg, {
            fitToFrame: fillAvailableSpace,
          }),
    [fillAvailableSpace, readySvg],
  );
  const iframeStyle = useMemo(
    () => {
      if (readySvg === null) {
        return undefined;
      }
      return getMermaidDiagramFrameStyle(readySvg, {
        fitToFrame: fillAvailableSpace,
      });
    },
    [fillAvailableSpace, readySvg],
  );

  if (renderState.status === "error") {
    return (
      <div
        className="mermaid-diagram-fallback"
        contentEditable={false}
        data-markdown-serialization="skip"
      >
        <p className="support-copy">
          Mermaid render failed: {renderState.error}
        </p>
        {showSourceOnError ? (
          <HighlightedCodeBlock
            className="code-block"
            code={code}
            language="mermaid"
            lineAttributes={lineAttributes}
            showCopyButton
          />
        ) : null}
      </div>
    );
  }

  return (
    <div
      {...(lineAttributes ?? {})}
      className={`mermaid-diagram-block${renderState.status === "loading" ? " mermaid-diagram-loading" : ""}`}
      contentEditable={false}
      data-markdown-serialization="skip"
      role="img"
      aria-label="Mermaid diagram"
    >
      {iframeSrcDoc !== null ? (
        <iframe
          className="mermaid-diagram-frame"
          data-testid="mermaid-frame"
          sandbox=""
          srcDoc={iframeSrcDoc}
          style={iframeStyle}
          title="Mermaid diagram preview"
        />
      ) : (
        <p className="support-copy">Rendering Mermaid diagram...</p>
      )}
    </div>
  );
}

// Renders a `rehype-katex`-produced `<span class="math inline">` or
// `<div class="math">` with the serialization-skip + contentEditable
// discipline required by `docs/features/source-renderers.md` §Rendered
// Markdown Diff Editing. When the per-document math budget is
// exceeded, falls back to displaying the raw source instead of the
// KaTeX output — same shape as `MermaidRenderBudgetFallback` but
// inline-friendly for inline math. The `children` passed in is the
// KaTeX HTML (already rendered by rehype-katex); we just wrap it with
// the skip/readonly attributes, never producing our own KaTeX output.
//
// Per-expression budget: if a single expression's source exceeds the
// character cap, we still render it (KaTeX handles length internally
// and returns a best-effort span) but mark it with a size-exceeded
// className so CSS can visually flag it. A stricter behavior (refuse
// to render) can be added later if needed; for now the soft signal is
// enough because KaTeX's own `maxExpand` and recursion guards prevent
// actual runaway rendering.
function renderSafeKatexElement(
  tag: "span" | "div",
  className: string,
  children: ReactNode,
  extraProps: Record<string, unknown>,
  hasTooManyMathExpressions: boolean,
  mode: "inline" | "block",
) {
  if (hasTooManyMathExpressions) {
    const note =
      mode === "block"
        ? `Math render skipped: document has more than ${MAX_MATH_EXPRESSIONS_PER_DOCUMENT} equations.`
        : "Math render skipped.";
    return (
      <MathRenderBudgetFallback className={className} mode={mode} note={note}>
        {children}
      </MathRenderBudgetFallback>
    );
  }
  // The KaTeX HTML itself is trusted because we configure
  // `trust: false` and `throwOnError: false` on `rehypeKatex`; any
  // malformed expression renders as a red error span rather than
  // arbitrary HTML. The `data-markdown-serialization="skip"` attribute
  // signals the rendered-Markdown diff editor's serializer (see
  // `ui/src/panels/DiffPanel.tsx::shouldSkipMarkdownEditableNode`) to
  // omit the entire subtree when reconstructing the source — the
  // original `$...$` / `$$...$$` source text stays in the document,
  // only the KaTeX presentation layer is skipped.
  const Tag = tag;
  return (
    <Tag
      className={className}
      contentEditable={false}
      data-markdown-serialization="skip"
      {...extraProps}
    >
      {children}
    </Tag>
  );
}

function MathRenderBudgetFallback({
  children,
  className,
  mode,
  note,
}: {
  children: ReactNode;
  className: string;
  mode: "inline" | "block";
  note: string;
}) {
  const Tag = mode === "block" ? "div" : "span";
  return (
    <Tag
      className={`${className} math-render-skipped`}
      contentEditable={false}
      data-markdown-serialization="skip"
      title={note}
    >
      {children}
    </Tag>
  );
}

function MermaidRenderBudgetFallback({
  code,
  lineAttributes,
  message,
  showSource = true,
}: {
  code: string;
  lineAttributes?: MarkdownLineAttributes | null;
  message: string;
  showSource?: boolean;
}) {
  return (
    <div
      className="mermaid-diagram-fallback"
      contentEditable={false}
      data-markdown-serialization="skip"
    >
      <p className="support-copy">{message}</p>
      {showSource ? (
        <HighlightedCodeBlock
          className="code-block"
          code={code}
          language="mermaid"
          lineAttributes={lineAttributes}
          showCopyButton
        />
      ) : null}
    </div>
  );
}

// Mermaid fence detection, math expression counting, and render-
// budget constants now live in `./source-renderers` (Phase 2 of
// `docs/features/source-renderers.md`). This file consumes them
// through the top-of-file import so new callers — Source panel,
// Diff panel for non-Markdown — get the same answers without having
// to re-import through `message-cards.tsx`.

// `remark-math` runs before GFM so the math-token boundary is
// respected when dollar signs appear near pipe-tables (a GFM feature).
// `remark-autolink-bare-file-references` stays last because it mutates
// text nodes and must not swallow math content into autolinks.
const MARKDOWN_REMARK_PLUGINS = [
  remarkMath,
  remarkGfm,
  remarkAutolinkBareFileReferences,
];
// `rehype-katex` runs on the hast tree after the remark → rehype
// conversion that react-markdown@8 performs internally. Options pin
// KaTeX to the subset of behavior the `source-renderers.md` spec
// allows: `trust: false` (no arbitrary HTML via `\href` / `\url`),
// `strict: "ignore"` (render a best-effort approximation instead of
// bailing on unknown macros — we want graceful fallback, not a render
// halt), `output: "html"` (no MathML sibling that doubles the DOM
// cost). `throwOnError: false` makes KaTeX return a red-colored
// error span instead of throwing, so a malformed expression can't
// take down the whole Markdown render.
const MARKDOWN_REHYPE_KATEX_OPTIONS = {
  output: "html" as const,
  strict: "ignore" as const,
  throwOnError: false,
  trust: false,
};
const MarkdownLinkContext = createContext(false);
// Block-level renderers must read this before emitting
// `data-markdown-line-start`. Nested blocks inside list items or blockquotes
// should inherit the outer block's gutter marker instead of adding a second
// marker for the same rendered line.
const MarkdownLineNumberSuppressedContext = createContext(false);

export function MarkdownContent({
  appearance = "dark",
  documentPath = null,
  fillMermaidAvailableSpace = false,
  isStreaming = false,
  markdown,
  onOpenSourceLink,
  preserveMermaidSource = false,
  renderBudgetMathExpressionCount,
  renderBudgetMermaidDiagramCount,
  renderMermaidDiagrams = true,
  searchQuery = "",
  searchHighlightTone = "match",
  showLineNumbers = false,
  startLineNumber = 1,
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  documentPath?: string | null;
  /**
   * Source-preview layout mode. When true, the Markdown shell is allowed
   * to use the full preview width and Mermaid iframes switch to fit mode:
   * wide diagrams shrink to the pane without a horizontal scrollbar while
   * narrow diagrams keep their natural SVG size. This is intentionally a
   * preview-wide layout affordance, not just a Mermaid flag, because the
   * surrounding Markdown copy area must widen with the iframe.
   */
  fillMermaidAvailableSpace?: boolean;
  /**
   * When `true`, an in-flight trailing block (unclosed fenced code
   * block, unclosed `$$` math display block, or pipe-table that has
   * not yet been terminated by a blank line) is rendered as plain
   * text in a `<pre class="markdown-streaming-fragment">` until it
   * settles, instead of being passed through `react-markdown` and
   * flickering through visibly-broken intermediate shapes (raw
   * `| ... |` text, table rows with mismatched cell counts,
   * runaway code blocks). Defaults to `false` so settled callers
   * (history, source-renderer previews, diff views) get the
   * existing pipeline unchanged. See
   * `ui/src/markdown-streaming-split.ts` for the cut policy.
   */
  isStreaming?: boolean;
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preserveMermaidSource?: boolean;
  /**
   * Optional document-level budget counts for callers that split one
   * logical Markdown document across multiple MarkdownContent instances.
   */
  renderBudgetMathExpressionCount?: number;
  renderBudgetMermaidDiagramCount?: number;
  renderMermaidDiagrams?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  showLineNumbers?: boolean;
  startLineNumber?: number | null;
  workspaceRoot?: string | null;
}) {
  // Streaming-aware split. When `isStreaming` is `true`, the
  // splitter walks `markdown` and returns a `settled` prefix safe
  // to pass through `react-markdown` and a `pending` suffix that
  // contains an in-flight trailing pipe-table / fence / `$$` block
  // (rendered as plain text below). When `isStreaming` is `false`
  // (the default for every settled caller — history bubbles, source
  // previews, diff viewers), the splitter is bypassed entirely:
  // `settled === markdown` and `pending === ""` so the existing
  // pipeline runs unchanged with no measurable cost. Memoised on
  // `[isStreaming, markdown]` so settled callers preserve referential
  // identity of `settledMarkdown` across re-renders, keeping
  // downstream memos (mermaid count, math count, ReactMarkdown tree)
  // cache-stable. See `ui/src/markdown-streaming-split.ts` and
  // `docs/features/streaming-markdown.md`.
  //
  // `deferAllBlocks: true` forces the splitter to defer every
  // table/fence/math block (and everything after it) for the entire
  // streaming duration, even if the block looks "closed" at a chunk
  // boundary. Without this, a chunk that lands at "header +
  // separator" arrives looking settled, react-markdown renders it
  // as a pipe-paragraph, the next chunk's body row reverts the
  // splitter to pending, and the bubble flickers MD → ASCII → MD.
  // With `deferAllBlocks: true` the user sees one transition at
  // turn end (when `isStreaming` flips false and the splitter is
  // bypassed) instead of a flicker per chunk boundary.
  const { settled: settledMarkdown, pending: pendingMarkdown } = useMemo(
    () =>
      isStreaming
        ? splitStreamingMarkdownForRendering(markdown, {
            deferAllBlocks: true,
          })
        : { settled: markdown, pending: "" },
    [isStreaming, markdown],
  );
  const pendingMarkdownLooksLikeTable = /^\s*\|/u.test(pendingMarkdown);
  // Preserve the already-rendered prefix across the streaming -> settled flip.
  // The prefix exists only before the first deferred block, so block-level
  // budgets still apply to the remainder. Line-numbered callers need a single
  // source-position document, so they intentionally skip this preservation path.
  const streamingSettledPrefixRef = useRef<string | null>(null);
  const previousStreamingSettledPrefix = streamingSettledPrefixRef.current;
  const shouldPreserveStreamingSettledPrefix =
    !isStreaming &&
    !showLineNumbers &&
    previousStreamingSettledPrefix != null &&
    previousStreamingSettledPrefix.length > 0 &&
    previousStreamingSettledPrefix.length < markdown.length &&
    markdown.startsWith(previousStreamingSettledPrefix);
  const primarySettledMarkdown = shouldPreserveStreamingSettledPrefix
    ? previousStreamingSettledPrefix
    : settledMarkdown;
  const settledRemainderMarkdown = shouldPreserveStreamingSettledPrefix
    ? markdown.slice(previousStreamingSettledPrefix.length)
    : "";
  if (isStreaming) {
    streamingSettledPrefixRef.current =
      settledMarkdown.length > 0 ? settledMarkdown : null;
  } else if (!shouldPreserveStreamingSettledPrefix) {
    streamingSettledPrefixRef.current = null;
  }
  // Keep callback in a ref so the memoized ReactMarkdown output stays stable
  // even when the parent re-renders with a new function reference.  Without
  // this, every re-render regenerates the entire markdown DOM tree, which
  // destroys any active browser text selection.
  const onOpenSourceLinkRef = useRef(onOpenSourceLink);
  onOpenSourceLinkRef.current = onOpenSourceLink;
  // Track presence (not identity) of the callback as a memo dependency so the
  // rendered tree changes structurally when the prop goes from absent to present
  // or vice versa.  The ref handles identity-only changes without rememoizing.
  const hasOpenSourceLink = onOpenSourceLink != null;
  const normalizedStartLineNumber =
    normalizeMarkdownStartLineNumber(startLineNumber);
  // Mermaid / math budgets count only the SETTLED prefix so an
  // in-flight unclosed fence or `$$` block (which lives in
  // `pendingMarkdown` and renders as plain text in the streaming
  // fragment below) does not count against the per-document caps.
  const mermaidDiagramCount = useMemo(
    () =>
      renderBudgetMermaidDiagramCount ??
      (renderMermaidDiagrams ? countMermaidFences(settledMarkdown) : 0),
    [
      renderBudgetMermaidDiagramCount,
      settledMarkdown,
      renderMermaidDiagrams,
    ],
  );
  const hasTooManyMermaidDiagrams =
    mermaidDiagramCount > MAX_MERMAID_DIAGRAMS_PER_DOCUMENT;
  // Per-document math budget. Kept separate from the per-expression
  // budget: the per-document cap is a cheap lexical scan, the per-
  // expression cap is checked inside the math component renderers
  // once `remark-math` has isolated each node. When over-budget,
  // `rehypeKatex` still runs (the plugin list is stable across
  // renders), but its output is replaced by the source-display
  // fallback in the custom `math`/`inlineMath` renderer below.
  const mathExpressionCount = useMemo(
    () =>
      renderBudgetMathExpressionCount ?? countMathExpressions(settledMarkdown),
    [renderBudgetMathExpressionCount, settledMarkdown],
  );
  const hasTooManyMathExpressions =
    mathExpressionCount > MAX_MATH_EXPRESSIONS_PER_DOCUMENT;
  const markdownRootRef = useRef<HTMLDivElement | null>(null);
  const [lineMarkers, setLineMarkers] = useState<MarkdownLineMarker[]>([]);

  useEffect(() => {
    if (!showLineNumbers) {
      setLineMarkers([]);
      return;
    }

    const root = markdownRootRef.current;
    if (!root) {
      setLineMarkers([]);
      return;
    }

    let animationFrameId = 0;
    const updateLineMarkers = () => {
      const rootRect = root.getBoundingClientRect();
      const nextMarkers = Array.from(
        root.querySelectorAll<HTMLElement>("[data-markdown-line-start]"),
      )
        .map((element) => {
          const line = Number(element.dataset.markdownLineStart);
          if (!Number.isFinite(line)) {
            return null;
          }

          const elementRect = element.getBoundingClientRect();
          return {
            line,
            range: element.dataset.markdownLineRange ?? String(line),
            top: Math.round(
              elementRect.top + elementRect.height / 2 - rootRect.top,
            ),
          };
        })
        .filter((marker): marker is MarkdownLineMarker => marker != null);

      setLineMarkers((currentMarkers) =>
        areMarkdownLineMarkersEqual(currentMarkers, nextMarkers)
          ? currentMarkers
          : nextMarkers,
      );
    };
    const scheduleLineMarkerUpdate = () => {
      window.cancelAnimationFrame(animationFrameId);
      animationFrameId = window.requestAnimationFrame(updateLineMarkers);
    };

    scheduleLineMarkerUpdate();

    const ResizeObserverConstructor = window.ResizeObserver;
    const resizeObserver = ResizeObserverConstructor
      ? new ResizeObserverConstructor(scheduleLineMarkerUpdate)
      : null;
    resizeObserver?.observe(root);
    window.addEventListener("resize", scheduleLineMarkerUpdate);

    return () => {
      window.cancelAnimationFrame(animationFrameId);
      resizeObserver?.disconnect();
      window.removeEventListener("resize", scheduleLineMarkerUpdate);
    };
    // Deps intentionally exclude `documentPath`, `hasOpenSourceLink`,
    // and `workspaceRoot`: those only feed the `<a>` renderer in
    // `ReactMarkdown`'s `components` prop (href resolution, click
    // handlers), not the `[data-markdown-line-start]` attributes
    // the ResizeObserver re-queries. Keeping them in the deps
    // would tear down + rebuild the observer on every unrelated
    // appearance or source-link-handler change — O(markers)
    // teardown + re-observe per context change with no benefit.
    // `settledMarkdown` and `normalizedStartLineNumber` stay
    // because they DO change the set of
    // `[data-markdown-line-start]` elements the body scans. The
    // streaming-pending fragment carries no such markers (it is
    // raw text in a `<pre>`), so it is correctly excluded.
  }, [settledMarkdown, normalizedStartLineNumber, showLineNumbers]);

  const renderMarkdownDocument = useCallback((
    documentMarkdown: string,
    key: string,
  ) => {
    const highlightChildren = (children: ReactNode) =>
      highlightReactNodeText(children, searchQuery, searchHighlightTone);
    const getLineAttributes = (
      sourcePosition: MarkdownSourcePosition | undefined,
    ) =>
      getMarkdownLineAttributes(
        sourcePosition,
        normalizedStartLineNumber,
        showLineNumbers,
      );

    return (
      <ReactMarkdown
        key={key}
        rawSourcePos={showLineNumbers}
        transformLinkUri={transformMarkdownLinkUri}
        components={{
          a: ({
            href,
            children,
            sourcePosition: _sourcePosition,
            ...props
          }) => {
            const isExternalLink = isExternalMarkdownHref(href ?? "");
            const fileLinkTarget = resolveMarkdownFileLinkTarget(
              href,
              workspaceRoot,
              documentPath,
            );
            const displayLabel = buildMarkdownHrefDisplayLabel(
              href,
              children,
              workspaceRoot,
              documentPath,
            );
            const scrubDomHref = Boolean(
              fileLinkTarget || shouldScrubMarkdownDomHref(href),
            );
            const domHref = scrubDomHref ? "#" : href;
            const internalLinkHref =
              scrubDomHref && href
                ? { [MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE]: href }
                : undefined;
            // `transformMarkdownLinkUri` returns "" for URIs that
            // `react-markdown` would otherwise neutralize to
            // `javascript:void(0)` (the placeholder React now warns
            // about + plans to block). Render those as a plain
            // `<span>` so the content is still shown but there is
            // no inert same-page-navigate anchor and no
            // `javascript:` URL reaching the DOM.
            if (!href) {
              return (
                <span className={props.className}>
                  <MarkdownLinkContext.Provider value>
                    {highlightChildren(displayLabel ?? children)}
                  </MarkdownLinkContext.Provider>
                </span>
              );
            }
            const handleClick = (event: ReactMouseEvent<HTMLAnchorElement>) => {
              props.onClick?.(event);
              if (event.defaultPrevented) {
                return;
              }

              if (!fileLinkTarget) {
                if (scrubDomHref) {
                  event.preventDefault();
                }
                return;
              }

              event.preventDefault();
              if (!onOpenSourceLinkRef.current) {
                return;
              }
              onOpenSourceLinkRef.current({
                ...fileLinkTarget,
                openInNewTab: event.ctrlKey || event.metaKey,
              });
            };

            return (
              <a
                {...props}
                {...internalLinkHref}
                href={domHref}
                draggable={false}
                target={isExternalLink ? "_blank" : undefined}
                rel={isExternalLink ? "noreferrer" : undefined}
                onClick={handleClick}
              >
                <MarkdownLinkContext.Provider value>
                  {highlightChildren(displayLabel ?? children)}
                </MarkdownLinkContext.Provider>
              </a>
            );
          },
          code: ({ children, className, inline, sourcePosition, ...props }) => {
            const isInsideMarkdownLink = useContext(MarkdownLinkContext);
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            const language = className?.match(/language-([\w-]+)/)?.[1] ?? null;
            const code = String(children).replace(/\n$/, "");
            const inlineFileLinkTarget = inline
              ? resolveMarkdownFileLinkTarget(code, workspaceRoot, documentPath)
              : null;

            if (inline) {
              if (
                inlineFileLinkTarget &&
                hasOpenSourceLink &&
                !isInsideMarkdownLink
              ) {
                const handleInlineCodeClick = (
                  event: ReactMouseEvent<HTMLAnchorElement>,
                ) => {
                  event.preventDefault();
                  onOpenSourceLinkRef.current?.({
                    ...inlineFileLinkTarget,
                    openInNewTab: event.ctrlKey || event.metaKey,
                  });
                };

                return (
                  <a
                    className="inline-code-link"
                    href="#"
                    draggable={false}
                    onClick={handleInlineCodeClick}
                  >
                    <code className={className} {...props}>
                      {highlightChildren(children)}
                    </code>
                  </a>
                );
              }

              return (
                <code className={className} {...props}>
                  {highlightChildren(children)}
                </code>
              );
            }

            if (renderMermaidDiagrams && isMermaidFenceLanguage(language)) {
              const lineAttributes = suppressLineNumber
                ? null
                : getLineAttributes(sourcePosition);
              const budgetMessage =
                code.length > MAX_MERMAID_SOURCE_CHARS
                  ? `Mermaid render skipped: diagram exceeds the ${MAX_MERMAID_SOURCE_CHARS.toLocaleString()} character render budget.`
                  : hasTooManyMermaidDiagrams
                    ? `Mermaid render skipped: document has ${mermaidDiagramCount} diagrams; the render budget is ${MAX_MERMAID_DIAGRAMS_PER_DOCUMENT}.`
                    : null;
              if (budgetMessage) {
                if (preserveMermaidSource) {
                  return (
                    <>
                      <MermaidRenderBudgetFallback
                        code={code}
                        lineAttributes={null}
                        message={budgetMessage}
                        showSource={false}
                      />
                      <HighlightedCodeBlock
                        className="code-block mermaid-source-block"
                        code={code}
                        lineAttributes={lineAttributes}
                        language={language}
                        showCopyButton
                        searchQuery={searchQuery}
                        searchHighlightTone={searchHighlightTone}
                      />
                    </>
                  );
                }

                return (
                  <MermaidRenderBudgetFallback
                    code={code}
                    lineAttributes={lineAttributes}
                    message={budgetMessage}
                  />
                );
              }

              if (preserveMermaidSource) {
                return (
                  <>
                    <MermaidDiagram
                      appearance={appearance}
                      code={code}
                      fillAvailableSpace={fillMermaidAvailableSpace}
                      lineAttributes={null}
                      showSourceOnError={false}
                    />
                    <HighlightedCodeBlock
                      className="code-block mermaid-source-block"
                      code={code}
                      lineAttributes={lineAttributes}
                      language={language}
                      showCopyButton
                      searchQuery={searchQuery}
                      searchHighlightTone={searchHighlightTone}
                    />
                  </>
                );
              }

              return (
                <MermaidDiagram
                  appearance={appearance}
                  code={code}
                  fillAvailableSpace={fillMermaidAvailableSpace}
                  lineAttributes={lineAttributes}
                />
              );
            }

            return (
              <HighlightedCodeBlock
                className="code-block"
                code={code}
                lineAttributes={
                  suppressLineNumber ? null : getLineAttributes(sourcePosition)
                }
                language={language}
                showCopyButton
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            );
          },
          p: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <p
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </p>
            );
          },
          li: ({
            children,
            ordered: _ordered,
            index: _index,
            checked: _checked,
            sourcePosition,
            ...props
          }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <li
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                <MarkdownLineNumberSuppressedContext.Provider value>
                  {highlightChildren(children)}
                </MarkdownLineNumberSuppressedContext.Provider>
              </li>
            );
          },
          ul: ({
            children,
            ordered: _ordered,
            depth: _depth,
            sourcePosition: _sourcePosition,
            ...props
          }) => <ul {...props}>{children}</ul>,
          ol: ({
            children,
            ordered: _ordered,
            depth: _depth,
            sourcePosition: _sourcePosition,
            ...props
          }) => <ol {...props}>{children}</ol>,
          blockquote: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <blockquote
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                <MarkdownLineNumberSuppressedContext.Provider value>
                  {highlightChildren(children)}
                </MarkdownLineNumberSuppressedContext.Provider>
              </blockquote>
            );
          },
          h1: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <h1
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </h1>
            );
          },
          h2: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <h2
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </h2>
            );
          },
          h3: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <h3
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </h3>
            );
          },
          h4: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <h4
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </h4>
            );
          },
          h5: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <h5
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </h5>
            );
          },
          h6: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <h6
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                {highlightChildren(children)}
              </h6>
            );
          },
          strong: ({ children, sourcePosition: _sourcePosition, ...props }) => (
            <strong {...props}>{highlightChildren(children)}</strong>
          ),
          em: ({ children, sourcePosition: _sourcePosition, ...props }) => (
            <em {...props}>{highlightChildren(children)}</em>
          ),
          del: ({ children, sourcePosition: _sourcePosition, ...props }) => (
            <del {...props}>{highlightChildren(children)}</del>
          ),
          table: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <div
                className="markdown-table-scroll"
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              >
                <table {...props}>{children}</table>
              </div>
            );
          },
          hr: ({ sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            return (
              <hr
                {...props}
                {...(suppressLineNumber
                  ? {}
                  : (getLineAttributes(sourcePosition) ?? {}))}
              />
            );
          },
          img: ({ alt, sourcePosition: _sourcePosition, ...props }) => (
            <img {...props} alt={alt ?? ""} draggable={false} />
          ),
          td: ({
            children,
            isHeader: _isHeader,
            sourcePosition: _sourcePosition,
            ...props
          }) => <td {...props}>{highlightChildren(children)}</td>,
          th: ({
            children,
            isHeader: _isHeader,
            sourcePosition: _sourcePosition,
            ...props
          }) => <th {...props}>{highlightChildren(children)}</th>,
          // `remark-math` annotates math nodes with the `math` /
          // `inlineMath` class on the emitted `<span>` / `<div>`.
          // `rehype-katex` then replaces the contents with KaTeX HTML.
          // We intercept the final elements here to (1) enforce the
          // per-expression budget (falling back to raw source display
          // when over budget), (2) stamp `data-markdown-serialization="skip"`
          // so the rendered-Markdown diff editor does not serialize the
          // KaTeX output back into the source buffer, and (3) mark the
          // rendered visual `contentEditable={false}` so
          // `contentEditable`-scoped edit surfaces skip it. Detection
          // is by className rather than a node-type check because
          // react-markdown@8 dispatches all hast elements through the
          // matching tag name (`span` for inline, `div` for block).
          span: ({
            children,
            className,
            node: _node,
            sourcePosition: _sourcePosition,
            ...props
          }) => {
            if (
              typeof className === "string" &&
              /(^|\s)math(\s|$)/.test(className) &&
              /inline/.test(className)
            ) {
              return renderSafeKatexElement(
                "span",
                className,
                children,
                props,
                hasTooManyMathExpressions,
                "inline",
              );
            }
            return (
              <span className={className} {...props}>
                {children}
              </span>
            );
          },
          div: ({
            children,
            className,
            node: _node,
            sourcePosition,
            ...props
          }) => {
            const suppressLineNumber = useContext(
              MarkdownLineNumberSuppressedContext,
            );
            if (
              typeof className === "string" &&
              /(^|\s)math(\s|$)/.test(className)
            ) {
              const lineAttributes = suppressLineNumber
                ? null
                : getLineAttributes(sourcePosition);
              return renderSafeKatexElement(
                "div",
                className,
                children,
                { ...props, ...(lineAttributes ?? {}) },
                hasTooManyMathExpressions,
                "block",
              );
            }
            return (
              <div className={className} {...props}>
                {children}
              </div>
            );
          },
        }}
        rehypePlugins={[[rehypeKatex, MARKDOWN_REHYPE_KATEX_OPTIONS]]}
        remarkPlugins={MARKDOWN_REMARK_PLUGINS}
      >
        {documentMarkdown}
      </ReactMarkdown>
    );
  }, [
    documentPath,
    appearance,
    fillMermaidAvailableSpace,
    hasTooManyMathExpressions,
    hasTooManyMermaidDiagrams,
    mermaidDiagramCount,
    normalizedStartLineNumber,
    preserveMermaidSource,
    renderMermaidDiagrams,
    searchQuery,
    searchHighlightTone,
    showLineNumbers,
    workspaceRoot,
    hasOpenSourceLink,
  ]);
  const renderedPrimary = useMemo(
    () => renderMarkdownDocument(primarySettledMarkdown, "settled-primary"),
    [primarySettledMarkdown, renderMarkdownDocument],
  );
  const renderedRemainder = useMemo(
    () =>
      settledRemainderMarkdown
        ? renderMarkdownDocument(settledRemainderMarkdown, "settled-remainder")
        : null,
    [settledRemainderMarkdown, renderMarkdownDocument],
  );
  const rendered = useMemo(
    () => (
      <>
        {renderedPrimary}
        {renderedRemainder}
      </>
    ),
    [renderedPrimary, renderedRemainder],
  );

  return (
    <div
      className={`markdown-copy-shell${showLineNumbers ? " markdown-copy-shell-with-line-numbers" : ""}${fillMermaidAvailableSpace ? " markdown-copy-shell-fill-mermaid" : ""}`}
    >
      {showLineNumbers ? (
        <div
          className="markdown-line-gutter"
          aria-hidden="true"
          contentEditable={false}
        >
          {lineMarkers.map((marker) => (
            <span
              className="markdown-line-number"
              data-markdown-gutter-line={marker.line}
              key={`${marker.line}:${marker.top}`}
              style={{ top: `${marker.top}px` }}
              title={
                marker.range === String(marker.line)
                  ? `Line ${marker.line}`
                  : `Lines ${marker.range}`
              }
            >
              {marker.line}
            </span>
          ))}
        </div>
      ) : null}
      <div
        className={`markdown-copy${showLineNumbers ? " markdown-copy-with-line-numbers" : ""}`}
        ref={markdownRootRef}
      >
        {rendered}
        {pendingMarkdown.length > 0 ? (
          // In-flight trailing block (unclosed fenced code, unclosed
          // `$$` math display, partial pipe-table). Rendered as plain
          // text so streaming chunks don't briefly appear as
          // raw-`| ... |` text, runaway code blocks, or tables with
          // mismatched cell counts. Snaps to the canonical render
          // once the block closes (a blank line for tables, the
          // matching closing fence for code/math) and the next
          // textDelta fires the memoized re-split.
          <pre
            className={`markdown-streaming-fragment${pendingMarkdownLooksLikeTable ? " markdown-streaming-fragment-table" : ""}`}
            aria-busy="true"
          >
            {pendingMarkdown}
          </pre>
        ) : null}
      </div>
    </div>
  );
}
