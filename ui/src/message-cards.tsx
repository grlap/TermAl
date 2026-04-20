import {
  createContext,
  isValidElement,
  memo,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
// Math rendering: `remark-math` parses `$...$` inline and `$$...$$`
// block math at the mdast layer; `rehype-katex` turns the math AST
// nodes into KaTeX HTML at the hast layer. Imported lazily (via the
// top-level import) because Mermaid is the only other renderer with
// per-region heft, and math is common enough across agent output that
// eagerly loading KaTeX is cheaper than a dynamic import on first
// equation. KaTeX CSS is imported so glyphs render correctly without
// the caller needing a separate stylesheet import.
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";
// Renderer registry (Phase 2 of `docs/features/source-renderers.md`).
// We import DETECTION helpers + budget constants from here so the
// Source / Diff panels can reuse the same definitions without
// pulling `message-cards.tsx` into their dependency graph.
import {
  MAX_MATH_EXPRESSIONS_PER_DOCUMENT,
  MAX_MERMAID_DIAGRAMS_PER_DOCUMENT,
  MAX_MERMAID_SOURCE_CHARS,
  countMathExpressions,
  countMermaidFences,
  isMermaidFenceLanguage,
} from "./source-renderers";
import { ExpandedPromptPanel } from "./ExpandedPromptPanel";
import {
  CheckIcon,
  CollapseIcon,
  CopyIcon,
  ExpandIcon,
  PreviewIcon,
} from "./message-card-icons";
import {
  buildMermaidDiagramFrameSrcDoc,
  getMermaidDiagramFrameStyle,
  renderTermalMermaidDiagram,
} from "./mermaid-render";
import { copyTextToClipboard } from "./clipboard";
import { buildDiffPreviewModel } from "./diff-preview";
import { highlightCode } from "./highlight";
import {
  buildMarkdownHrefDisplayLabel,
  isExternalMarkdownHref,
  resolveMarkdownFileLinkTarget,
  transformMarkdownLinkUri,
  type MarkdownFileLinkTarget,
} from "./markdown-links";
import {
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "./path-display";
import {
  containsSearchMatch,
  highlightReactNodeText,
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type {
  ApprovalDecision,
  ApprovalMessage,
  CodexAppRequestMessage,
  CommandMessage,
  FileChangesMessage,
  DiffMessage,
  ImageAttachment,
  JsonValue,
  MarkdownMessage,
  McpElicitationAction,
  McpElicitationPrimitiveSchema,
  McpElicitationRequestMessage,
  Message,
  ParallelAgentsMessage,
  SubagentResultMessage,
  TextMessage,
  ThinkingMessage,
  UserInputQuestion,
  UserInputRequestMessage,
} from "./types";
import {
  formatByteSize,
  getErrorMessage,
  imageAttachmentSummaryLabel,
  mapCommandStatus,
  parseConnectionRetryNotice,
  renderDecision,
  type ConnectionRetryNotice,
} from "./app-utils";
import {
  DEFERRED_RENDER_ROOT_MARGIN_PX,
  buildDeferredPreviewText,
  buildMarkdownPreviewText,
  estimateCodeBlockHeight,
  estimateMarkdownBlockHeight,
  isElementNearRenderViewport,
  measureTextBlock,
  resolveDeferredRenderRoot,
} from "./deferred-render";
import type { MonacoAppearance } from "./monaco";

const HEAVY_CODE_CHARACTER_THRESHOLD = 1400;
const HEAVY_CODE_LINE_THRESHOLD = 28;
const HEAVY_MARKDOWN_CHARACTER_THRESHOLD = 1800;
const HEAVY_MARKDOWN_LINE_THRESHOLD = 24;
const FILE_CHANGES_COLLAPSE_THRESHOLD = 6;
let mermaidDiagramIdCounter = 0;

// Stable no-op defaults for the optional callback props on
// `MessageCard`. NOTE: React's `memo` comparator receives the
// RAW props object as passed by the parent, NOT the destructured
// values — so an omitted optional prop reads as `undefined` on
// both sides and passes the `===` identity check cleanly without
// any help from these constants. Hoisting the defaults to module
// scope is a pure code-quality improvement: the defaults are now
// named, reusable across future call sites, and avoid allocating
// a fresh no-op arrow on every render (a tiny GC win, not a
// memoization fix). See docs/bugs.md → "MessageCard default-prop
// inline arrows" for the original misdiagnosis and the test in
// `MarkdownContent.test.tsx::"skips re-rendering when a parent
// re-renders with identical props and no optional callbacks"`
// that pins the actual memo-hit behaviour for the omitted case.
const NOOP_MCP_ELICITATION_SUBMIT: (
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void = () => {};
const NOOP_CODEX_APP_REQUEST_SUBMIT: (
  messageId: string,
  result: JsonValue,
) => void = () => {};

// Re-exported for backwards compatibility with callers that used to import
// the type from this module before the helpers were split out into
// `./markdown-links`.
export type { MarkdownFileLinkTarget } from "./markdown-links";

type MarkdownSourcePosition = {
  start?: {
    line?: number | null;
  } | null;
  end?: {
    line?: number | null;
  } | null;
} | null;

type MarkdownLineAttributes = {
  "data-markdown-line-start": number;
  "data-markdown-line-range": string;
  title: string;
};

type MarkdownLineMarker = {
  line: number;
  range: string;
  top: number;
};

export const MessageCard = memo(function MessageCard({
  appearance = "dark",
  message,
  onOpenDiffPreview,
  onOpenSourceLink,
  preferImmediateHeavyRender = false,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit = NOOP_MCP_ELICITATION_SUBMIT,
  onCodexAppRequestSubmit = NOOP_CODEX_APP_REQUEST_SUBMIT,
  searchQuery = "",
  searchHighlightTone = "match",
  isLatestAssistantMessage = true,
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: Message;
  onOpenDiffPreview?: (message: DiffMessage) => void;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateHeavyRender?: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: (messageId: string, answers: Record<string, string[]>) => void;
  onMcpElicitationSubmit?: (
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  onCodexAppRequestSubmit?: (messageId: string, result: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  // When false, `ConnectionRetryCard` renders the resolved (static, past-tense)
  // variant because later assistant output exists and the reconnect obviously
  // succeeded. Defaults to true so tests and callers that have not opted in
  // keep the pre-existing "live spinner" behaviour.
  isLatestAssistantMessage?: boolean;
  workspaceRoot?: string | null;
}) {
  switch (message.type) {
    case "text": {
      const connectionRetryNotice =
        message.author === "assistant" ? parseConnectionRetryNotice(message.text) : null;
      const commandLabel =
        message.author === "you"
          ? promptCommandMetaLabel(message.text, message.expandedText)
          : null;

      if (connectionRetryNotice) {
        return (
          <ConnectionRetryCard
            message={message}
            notice={connectionRetryNotice}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
            isLive={isLatestAssistantMessage}
          />
        );
      }

      return (
        <article className={`message-card bubble bubble-${message.author}`}>
          <MessageMeta
            author={message.author}
            timestamp={message.timestamp}
            trailing={
              commandLabel ? <span className="message-meta-tag">{commandLabel}</span> : undefined
            }
          />
          {message.attachments && message.attachments.length > 0 ? (
            <MessageAttachmentList
              attachments={message.attachments}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : null}
          {message.author === "assistant" ? (
            preferImmediateHeavyRender ? (
              <MarkdownContent
                appearance={appearance}
                markdown={message.text}
                onOpenSourceLink={onOpenSourceLink}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
                workspaceRoot={workspaceRoot}
              />
            ) : (
              <DeferredMarkdownContent
                appearance={appearance}
                markdown={message.text}
                onOpenSourceLink={onOpenSourceLink}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
                workspaceRoot={workspaceRoot}
              />
            )
          ) : message.text ? (
            <>
              <p className="plain-text-copy">
                {renderHighlightedText(message.text, searchQuery, searchHighlightTone)}
              </p>
              {message.expandedText ? (
                <ExpandedPromptPanel
                  expandedText={message.expandedText}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : null}
            </>
          ) : (
            <p className="support-copy">{imageAttachmentSummaryLabel(message.attachments?.length ?? 0)}</p>
          )}
        </article>
      );
    }
    case "thinking":
      return (
        <ThinkingCard
          appearance={appearance}
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "command":
      return (
        <CommandCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "diff":
      return (
        <DiffCard
          message={message}
          onOpenPreview={() => onOpenDiffPreview?.(message)}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "markdown":
      return (
        <MarkdownCard
          appearance={appearance}
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "parallelAgents":
      return (
        <ParallelAgentsCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "fileChanges":
      return (
        <FileChangesCard
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "subagentResult":
      return (
        <SubagentResultCard
          appearance={appearance}
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "approval":
      return (
        <ApprovalCard
          message={message}
          onApprovalDecision={onApprovalDecision}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "userInputRequest":
      return (
        <UserInputRequestCard
          message={message}
          onSubmit={onUserInputSubmit}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "mcpElicitationRequest":
      return (
        <McpElicitationRequestCard
          message={message}
          onSubmit={onMcpElicitationSubmit}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "codexAppRequest":
      return (
        <CodexAppRequestCard
          message={message}
          onSubmit={onCodexAppRequestSubmit}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    default:
      return null;
  }
}, (previous, next) =>
  previous.appearance === next.appearance &&
  previous.message === next.message &&
  previous.onOpenDiffPreview === next.onOpenDiffPreview &&
  previous.onOpenSourceLink === next.onOpenSourceLink &&
  previous.preferImmediateHeavyRender === next.preferImmediateHeavyRender &&
  previous.onApprovalDecision === next.onApprovalDecision &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone &&
  previous.isLatestAssistantMessage === next.isLatestAssistantMessage &&
  previous.workspaceRoot === next.workspaceRoot
);

function promptCommandMetaLabel(text: string, expandedText?: string | null) {
  return expandedText && text.trim().startsWith("/") ? "Command" : null;
}

function ConnectionRetryCard({
  message,
  notice,
  searchQuery,
  searchHighlightTone,
  isLive,
}: {
  message: TextMessage;
  notice: ConnectionRetryNotice;
  searchQuery: string;
  searchHighlightTone: SearchHighlightTone;
  // True while this retry notice is still the last assistant-authored message
  // in the session. Flipping to false means later assistant output exists, so
  // the reconnect obviously succeeded — the spinner stops and the copy moves
  // to past tense.
  isLive: boolean;
}) {
  const heading = isLive
    ? "Reconnecting to continue this turn"
    : "Connection recovered";
  const detail = isLive
    ? notice.detail
    : notice.attemptLabel
      ? `Connection dropped briefly; the turn continued after ${notice.attemptLabel.toLowerCase()}.`
      : "Connection dropped briefly; the turn continued after an automatic retry.";
  const chipClassName = isLive
    ? "chip chip-status chip-status-active"
    : "chip chip-status";
  const cardClassName = isLive
    ? "message-card connection-notice-card"
    : "message-card connection-notice-card connection-notice-card-resolved";
  return (
    <article
      className={cardClassName}
      role="status"
      aria-live={isLive ? "polite" : "off"}
    >
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="connection-notice-body">
        {isLive ? (
          <div className="activity-spinner connection-notice-spinner" aria-hidden="true" />
        ) : null}
        <div className="connection-notice-copy">
          <div className="card-label">Connection</div>
          <div className="connection-notice-heading">
            <h3>{heading}</h3>
            {notice.attemptLabel ? (
              <span className={chipClassName}>{notice.attemptLabel}</span>
            ) : null}
          </div>
          <p className="connection-notice-detail">
            {renderHighlightedText(detail, searchQuery, searchHighlightTone)}
          </p>
        </div>
      </div>
    </article>
  );
}

function MessageAttachmentList({
  attachments,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  attachments: ImageAttachment[];
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <div className="message-attachment-list">
      {attachments.map((attachment, index) => (
        <div key={`${attachment.fileName}-${attachment.byteSize}-${index}`} className="message-attachment-chip">
          <strong className="message-attachment-name">
            {renderHighlightedText(attachment.fileName, searchQuery, searchHighlightTone)}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} {"\u00b7"}{" "}
            {renderHighlightedText(attachment.mediaType, searchQuery, searchHighlightTone)}
          </span>
        </div>
      ))}
    </div>
  );
}

function MessageMeta({
  author,
  timestamp,
  trailing,
}: {
  author: string;
  timestamp: string;
  trailing?: ReactNode;
}) {
  const isUser = author === "you";

  return (
    <div className="message-meta">
      <span
        className={`message-meta-author ${isUser ? "message-meta-author-user" : "message-meta-author-agent"}`}
      >
        {isUser ? "You" : "Agent"}
      </span>
      <span className="message-meta-end">
        {trailing}
        <span>{timestamp}</span>
      </span>
    </div>
  );
}

function DeferredHeavyContent({
  children,
  estimatedHeight,
  placeholder,
}: {
  children: ReactNode;
  estimatedHeight: number;
  placeholder: ReactNode;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [isActivated, setIsActivated] = useState(false);

  useLayoutEffect(() => {
    if (isActivated) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    // Inside a virtualized conversation list the outer virtualizer
    // (`VirtualizedConversationMessageList`) already limits mount to
    // cards within its overscan window. Deferring heavy content on
    // top of that just introduces a placeholder-to-real-content
    // height jump when the IntersectionObserver below eventually
    // fires, and that jump cascades into the layout:
    // `ResizeObserver` on the virtualized slot → `handleHeightChange`
    // → `setLayoutVersion` → tops recomputed for every sibling →
    // scroll-position-visible cards shift under the user's cursor.
    // Activate immediately when we can detect we're inside the
    // virtualized wrapper so the card renders at its real height on
    // the very first paint.
    if (node.closest(".virtualized-message-list")) {
      setIsActivated(true);
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    if (isElementNearRenderViewport(node, root, DEFERRED_RENDER_ROOT_MARGIN_PX)) {
      setIsActivated(true);
    }
  }, [isActivated]);

  useEffect(() => {
    if (isActivated) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    if (typeof window === "undefined" || typeof window.IntersectionObserver === "undefined") {
      setIsActivated(true);
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    const observer = new window.IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting || entry.intersectionRatio > 0)) {
          setIsActivated(true);
        }
      },
      {
        root,
        rootMargin: `${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px ${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px`,
        threshold: 0.01,
      },
    );

    observer.observe(node);
    return () => {
      observer.disconnect();
    };
  }, [isActivated]);

  return (
    <div
      ref={containerRef}
      className="deferred-heavy-content"
      style={
        isActivated
          ? undefined
          : ({ "--deferred-min-height": `${estimatedHeight}px` } as CSSProperties)
      }
    >
      {isActivated ? children : placeholder}
    </div>
  );
}

function DeferredHighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
  searchQuery,
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const metrics = useMemo(() => measureTextBlock(code), [code]);
  const showSearchHighlight = containsSearchMatch(code, searchQuery ?? "");
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_CODE_LINE_THRESHOLD || code.length >= HEAVY_CODE_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    );
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateCodeBlockHeight(metrics.lineCount)}
      placeholder={
        <pre className={`${className} syntax-block deferred-code-placeholder`}>
          <code>{buildDeferredPreviewText(code)}</code>
        </pre>
      }
    >
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    </DeferredHeavyContent>
  );
}

function DeferredMarkdownContent({
  appearance = "dark",
  documentPath = null,
  markdown,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  documentPath?: string | null;
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const metrics = useMemo(() => measureTextBlock(markdown), [markdown]);
  const showSearchHighlight = containsSearchMatch(markdown, searchQuery);
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_MARKDOWN_LINE_THRESHOLD ||
      markdown.length >= HEAVY_MARKDOWN_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    );
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateMarkdownBlockHeight(metrics.lineCount)}
      placeholder={
        <div className="markdown-copy deferred-markdown-placeholder">
          <p className="plain-text-copy">{buildMarkdownPreviewText(markdown)}</p>
        </div>
      }
    >
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </DeferredHeavyContent>
  );
}

function HighlightedCodeBlock({
  className,
  code,
  commandHint,
  lineAttributes,
  language,
  pathHint,
  showCopyButton = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  lineAttributes?: MarkdownLineAttributes | null;
  language?: string | null;
  pathHint?: string | null;
  showCopyButton?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [copied, setCopied] = useState(false);
  const showSearchHighlight = containsSearchMatch(code, searchQuery);
  const highlighted = useMemo(
    () =>
      highlightCode(code, {
        commandHint,
        language,
        pathHint,
      }),
    [code, commandHint, language, pathHint],
  );
  const codeLanguage = highlighted.language ?? normalizeCodeLanguageClass(language);

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(code);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <pre
      {...(lineAttributes ?? {})}
      className={`${className} syntax-block${showCopyButton ? " copyable" : ""}`}
    >
      {showCopyButton ? (
        <button
          className={`command-icon-button syntax-copy-button${copied ? " copied" : ""}`}
          type="button"
          onMouseDown={(event) => {
            event.preventDefault();
            event.stopPropagation();
          }}
          onClick={() => void handleCopy()}
          aria-label={copied ? "Code copied" : "Copy code"}
          title={copied ? "Copied" : "Copy code"}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </button>
      ) : null}
      <code className={`hljs${codeLanguage ? ` language-${codeLanguage}` : ""}`}>
        {showSearchHighlight
          ? renderHighlightedText(code, searchQuery, searchHighlightTone)
          : (
            <span dangerouslySetInnerHTML={{ __html: highlighted.html }} />
          )}
      </code>
    </pre>
  );
}

function MermaidDiagram({
  appearance,
  code,
  lineAttributes,
  showSourceOnError = true,
}: {
  appearance: MonacoAppearance;
  code: string;
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
    void import("mermaid")
      .then(async ({ default: mermaid }) => {
        const { svg } = await renderTermalMermaidDiagram(
          mermaid,
          diagramId,
          code,
          appearance,
        );
        if (!cancelled) {
          setRenderState({ error: null, status: "ready", svg });
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setRenderState({ error: getErrorMessage(error), status: "error", svg: null });
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
    () => (readySvg === null ? null : buildMermaidDiagramFrameSrcDoc(readySvg)),
    [readySvg],
  );
  const iframeStyle = useMemo(
    () => (readySvg === null ? undefined : getMermaidDiagramFrameStyle(readySvg)),
    [readySvg],
  );

  if (renderState.status === "error") {
    return (
      <div className="mermaid-diagram-fallback" contentEditable={false} data-markdown-serialization="skip">
        <p className="support-copy">Mermaid render failed: {renderState.error}</p>
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
    <div className="mermaid-diagram-fallback" contentEditable={false} data-markdown-serialization="skip">
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

function normalizeCodeLanguageClass(language: string | null | undefined) {
  const normalized = language?.trim().toLowerCase().replace(/^language-/, "") ?? "";
  return /^[a-z0-9_-]+$/.test(normalized) ? normalized : null;
}

function ThinkingCard({
  appearance = "dark",
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: ThinkingMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const markdown = message.lines.join("\n");

  return (
    <article className="message-card reasoning-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Thinking</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredMarkdownContent
        appearance={appearance}
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </article>
  );
}

export function CommandCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CommandMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [inputExpanded, setInputExpanded] = useState(false);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const [copiedSection, setCopiedSection] = useState<"command" | "output" | null>(null);
  const hasOutput = message.output.trim().length > 0;
  const displayOutput = hasOutput
    ? message.output
    : message.status === "running"
      ? "Awaiting output\u2026"
      : "No output";
  const canExpandCommand =
    message.command.split("\n").length > 10 || message.command.length > 480;
  const canExpandOutput =
    hasOutput && (message.output.split("\n").length > 10 || message.output.length > 480);
  const statusTone = mapCommandStatus(message.status);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isInputExpanded = inputExpanded || isSearchExpanded;
  const isOutputExpanded = outputExpanded || isSearchExpanded;

  useEffect(() => {
    if (!copiedSection) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedSection(null);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedSection]);

  async function handleCopy(section: "command" | "output", text: string) {
    try {
      await copyTextToClipboard(text);
      setCopiedSection(section);
    } catch {
      setCopiedSection(null);
    }
  }

  return (
    <article className="message-card utility-card command-card">
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          <span className={`chip chip-status chip-status-${statusTone} command-status-chip`}>
            {message.status}
          </span>
        }
      />
      <div className="card-label command-card-label">Command</div>

      <div className="command-panel">
        <div className="command-row">
          <div className="command-row-label">IN</div>
          <div className="command-row-body">
            <div className={`command-input-shell ${isInputExpanded ? "expanded" : "collapsed"}`}>
              <DeferredHighlightedCodeBlock
                className="command-text command-text-input"
                code={message.command}
                language={message.commandLanguage ?? "bash"}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copiedSection === "command" ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy("command", message.command)}
              aria-label={copiedSection === "command" ? "Command copied" : "Copy command"}
              title={copiedSection === "command" ? "Copied" : "Copy command"}
            >
              {copiedSection === "command" ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandCommand ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setInputExpanded((open) => !open)}
                aria-label={isInputExpanded ? "Collapse command" : "Expand command"}
                aria-pressed={isInputExpanded}
                title={isInputExpanded ? "Collapse command" : "Expand command"}
              >
                {isInputExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>

        <div className="command-row command-row-output">
          <div className="command-row-label">OUT</div>
          <div className="command-row-body">
            <div
              className={`command-output-shell ${isOutputExpanded ? "expanded" : "collapsed"} ${hasOutput ? "has-output" : "empty"}`}
            >
              {hasOutput ? (
                <DeferredHighlightedCodeBlock
                  className="command-text command-text-output"
                  code={displayOutput}
                  language={message.outputLanguage ?? null}
                  commandHint={message.output ? message.command : null}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : (
                <pre className="command-text command-text-output command-text-placeholder">
                  {displayOutput}
                </pre>
              )}
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copiedSection === "output" ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy("output", message.output)}
              aria-label={copiedSection === "output" ? "Output copied" : "Copy output"}
              title={copiedSection === "output" ? "Copied" : "Copy output"}
              disabled={!message.output}
            >
              {copiedSection === "output" ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandOutput ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setOutputExpanded((open) => !open)}
                aria-label={isOutputExpanded ? "Collapse output" : "Expand output"}
                aria-pressed={isOutputExpanded}
                title={isOutputExpanded ? "Collapse output" : "Expand output"}
              >
                {isOutputExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}

export function DiffCard({
  message,
  onOpenPreview,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: DiffMessage;
  onOpenPreview: () => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const diffStats = useMemo(() => {
    const changeSummary = buildDiffPreviewModel(message.diff, message.changeType).changeSummary;
    return {
      addedLineCount: changeSummary.changedLineCount + changeSummary.addedLineCount,
      removedLineCount: changeSummary.changedLineCount + changeSummary.removedLineCount,
    };
  }, [message.changeType, message.diff]);
  const displayPath = useMemo(
    () => relativizePathToWorkspace(message.filePath, workspaceRoot),
    [message.filePath, workspaceRoot],
  );
  const canExpandDiff = message.diff.split("\n").length > 14 || message.diff.length > 900;
  const isExpanded = !canExpandDiff || expanded || searchQuery.trim().length > 0;

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(message.diff);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <article className="message-card utility-card diff-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">{message.changeType === "create" ? "New file" : "File edit"}</div>
      <div className="command-panel diff-panel">
        <div className="command-row diff-file-row">
          <div className="command-row-label diff-file-label">
            <span>FILE</span>
            {diffStats.addedLineCount > 0 || diffStats.removedLineCount > 0 ? (
              <div className="diff-file-stats">
                {diffStats.addedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-added">+{diffStats.addedLineCount}</span>
                ) : null}
                {diffStats.removedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-removed">-{diffStats.removedLineCount}</span>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="command-row-body">
            <div
              className="diff-file-path"
              title={displayPath !== message.filePath ? message.filePath : undefined}
            >
              {renderHighlightedText(displayPath, searchQuery, searchHighlightTone)}
            </div>
            <p className="diff-file-summary">
              {renderHighlightedText(message.summary, searchQuery, searchHighlightTone)}
            </p>
          </div>
        </div>
        <div className="command-row diff-row">
          <div className="command-row-label">DIFF</div>
          <div className="command-row-body">
            <div className={`diff-preview-shell ${isExpanded ? "expanded" : "collapsed"}`}>
              <DeferredHighlightedCodeBlock
                className="diff-block diff-preview-text"
                code={message.diff}
                language={message.language ?? "diff"}
                pathHint={message.filePath}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className="command-icon-button"
              type="button"
              onClick={onOpenPreview}
              aria-label="Open diff preview"
              title="Open diff preview"
            >
              <PreviewIcon />
            </button>
            <button
              className={`command-icon-button${copied ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy()}
              aria-label={copied ? "Diff copied" : "Copy diff"}
              title={copied ? "Copied" : "Copy diff"}
            >
              {copied ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandDiff ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setExpanded((open) => !open)}
                aria-label={isExpanded ? "Collapse diff" : "Expand diff"}
                aria-pressed={isExpanded}
                title={isExpanded ? "Collapse diff" : "Expand diff"}
              >
                {isExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}

function FileChangesCard({
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: FileChangesMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [copiedPath, setCopiedPath] = useState<string | null>(null);
  const [filesExpanded, setFilesExpanded] = useState(false);
  const canExpandFiles = message.files.length > FILE_CHANGES_COLLAPSE_THRESHOLD;
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isFilesExpanded = !canExpandFiles || filesExpanded || isSearchExpanded;
  const collapseControlLabel = filesExpanded ? "Collapse changed files" : "Expand changed files";

  useEffect(() => {
    if (!copiedPath) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedPath(null);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedPath]);

  async function handleCopyPath(path: string) {
    try {
      await copyTextToClipboard(path);
      setCopiedPath(path);
    } catch {
      setCopiedPath(null);
    }
  }

  function handleOpenPath(
    path: string,
    event: ReactMouseEvent<HTMLButtonElement>,
  ) {
    onOpenSourceLink?.({
      path,
      openInNewTab: event.ctrlKey || event.metaKey,
    });
  }

  return (
    <article className="message-card utility-card file-changes-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Files</div>
      <div className="command-panel file-changes-panel">
        <div className="command-row file-changes-summary-row">
          <div className="command-row-label">TURN</div>
          <div className="command-row-body">
            <p className="file-changes-title">
              {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
            </p>
          </div>
          {canExpandFiles && !isSearchExpanded ? (
            <div className="command-row-actions">
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setFilesExpanded((open) => !open)}
                aria-label={collapseControlLabel}
                aria-expanded={filesExpanded}
                title={collapseControlLabel}
              >
                {isFilesExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            </div>
          ) : null}
        </div>
        {isFilesExpanded ? message.files.map((file) => {
          const displayPath = relativizePathToWorkspace(file.path, workspaceRoot);
          const copied = copiedPath === file.path;

          return (
            <div className="command-row file-change-row" key={`${file.kind}:${file.path}`}>
              <div className="command-row-label">
                <span className={`file-change-kind file-change-kind-${file.kind}`}>
                  {fileChangeKindLabel(file.kind)}
                </span>
              </div>
              <div className="command-row-body">
                <div
                  className="file-change-path"
                  title={displayPath !== file.path ? file.path : undefined}
                >
                  {renderHighlightedText(displayPath, searchQuery, searchHighlightTone)}
                </div>
              </div>
              <div className="command-row-actions">
                <button
                  className="command-icon-button"
                  type="button"
                  onClick={(event) => handleOpenPath(file.path, event)}
                  disabled={!onOpenSourceLink}
                  aria-label={`Open ${displayPath}`}
                  title="Open file"
                >
                  <PreviewIcon />
                </button>
                <button
                  className={`command-icon-button${copied ? " copied" : ""}`}
                  type="button"
                  onClick={() => void handleCopyPath(file.path)}
                  aria-label={copied ? "Path copied" : `Copy ${displayPath}`}
                  title={copied ? "Copied" : "Copy path"}
                >
                  {copied ? <CheckIcon /> : <CopyIcon />}
                </button>
              </div>
            </div>
          );
        }) : null}
      </div>
    </article>
  );
}

function fileChangeKindLabel(kind: FileChangesMessage["files"][number]["kind"]) {
  switch (kind) {
    case "created":
      return "A";
    case "modified":
      return "M";
    case "deleted":
      return "D";
    case "other":
      return "*";
  }
}

function MarkdownCard({
  appearance = "dark",
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: MarkdownMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  return (
    <article className="message-card markdown-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Markdown</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredMarkdownContent
        appearance={appearance}
        markdown={message.markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </article>
  );
}

function parallelAgentsHeading(message: ParallelAgentsMessage) {
  const count = message.agents.length;
  const label = count === 1 ? "agent" : "agents";
  const activeCount = message.agents.filter(
    (agent) => agent.status === "initializing" || agent.status === "running",
  ).length;
  if (activeCount > 0) {
    return `Running ${count} ${label}`;
  }

  const errorCount = message.agents.filter((agent) => agent.status === "error").length;
  if (errorCount > 0) {
    return `${count} ${label} finished with ${errorCount} error${errorCount === 1 ? "" : "s"}`;
  }

  return `${count} ${label} completed`;
}

function parallelAgentsSummary(message: ParallelAgentsMessage) {
  const activeCount = message.agents.filter(
    (agent) => agent.status === "initializing" || agent.status === "running",
  ).length;
  const completedCount = message.agents.filter((agent) => agent.status === "completed").length;
  const errorCount = message.agents.filter((agent) => agent.status === "error").length;

  if (activeCount > 0) {
    const parts = [];
    if (completedCount > 0) {
      parts.push(`${completedCount} done`);
    }
    if (errorCount > 0) {
      parts.push(`${errorCount} failed`);
    }
    parts.push(`${activeCount} active`);
    return parts.join(" \u00b7 ");
  }

  if (errorCount > 0 && completedCount > 0) {
    return `${completedCount} completed \u00b7 ${errorCount} failed`;
  }
  if (errorCount > 0) {
    return `${errorCount} failed`;
  }

  return "All task agents completed.";
}

function parallelAgentStatusLabel(status: ParallelAgentsMessage["agents"][number]["status"]) {
  switch (status) {
    case "initializing":
      return "initializing";
    case "running":
      return "running";
    case "completed":
      return "completed";
    case "error":
      return "failed";
  }
}

function parallelAgentStatusTone(status: ParallelAgentsMessage["agents"][number]["status"]) {
  switch (status) {
    case "initializing":
    case "running":
      return "active";
    case "completed":
      return "idle";
    case "error":
      return "error";
  }
}

function parallelAgentDetail(agent: ParallelAgentsMessage["agents"][number]) {
  if (agent.detail?.trim()) {
    return agent.detail;
  }

  return agent.status === "error" ? "Task failed." : "Initializing...";
}

function ParallelAgentsCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ParallelAgentsMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(false);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const hasActiveAgents = message.agents.some(
    (agent) => agent.status === "initializing" || agent.status === "running",
  );
  useEffect(() => {
    if (hasActiveAgents) {
      setExpanded(true);
    }
  }, [hasActiveAgents]);
  const canCollapse = !hasActiveAgents && !isSearchExpanded;
  const isExpanded = hasActiveAgents || isSearchExpanded || expanded;
  const heading = parallelAgentsHeading(message);
  const summary = parallelAgentsSummary(message);

  return (
    <article className={`message-card reasoning-card parallel-agents-card${isExpanded ? " is-expanded" : ""}`}>
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          canCollapse ? (
            <button
              className="ghost-button parallel-agents-toggle"
              type="button"
              onClick={() => setExpanded((open) => !open)}
              aria-expanded={isExpanded}
            >
              {isExpanded ? "Hide tasks" : "Show tasks"}
            </button>
          ) : undefined
        }
      />
      <div className="card-label parallel-agents-card-label">Parallel agents</div>
      <div className="parallel-agents-header">
        <h3>{renderHighlightedText(heading, searchQuery, searchHighlightTone)}</h3>
        <span className="parallel-agents-summary">{summary}</span>
      </div>
      {isExpanded ? (
        <ol className="parallel-agents-tree">
          {message.agents.map((agent, index) => {
            const isLast = index === message.agents.length - 1;
            return (
              <li
                key={agent.id}
                className={`parallel-agent-row parallel-agent-row-${parallelAgentStatusTone(agent.status)}`}
              >
                <div className="parallel-agent-line">
                  <span className="parallel-agent-branch" aria-hidden="true">
                    {isLast ? "\u2514" : "\u251c"}
                  </span>
                  <div className="parallel-agent-copy">
                    <div className="parallel-agent-title-row">
                      <span className="parallel-agent-title">
                        {renderHighlightedText(agent.title, searchQuery, searchHighlightTone)}
                      </span>
                      <span
                        className={`parallel-agent-status parallel-agent-status-${parallelAgentStatusTone(agent.status)}`}
                      >
                        {parallelAgentStatusLabel(agent.status)}
                      </span>
                    </div>
                    <div className="parallel-agent-detail-row">
                      <span className="parallel-agent-branch-child" aria-hidden="true">
                        {isLast ? " " : "\u2502"}
                      </span>
                      <span className="parallel-agent-detail">
                        {renderHighlightedText(
                          parallelAgentDetail(agent),
                          searchQuery,
                          searchHighlightTone,
                        )}
                      </span>
                    </div>
                  </div>
                </div>
              </li>
            );
          })}
        </ol>
      ) : null}
    </article>
  );
}
function SubagentResultCard({
  appearance = "dark",
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: SubagentResultMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isExpanded = expanded || isSearchExpanded;

  return (
    <article className={`message-card reasoning-card subagent-result-card${isExpanded ? " is-expanded" : ""}`}>
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          <button
            className="ghost-button subagent-result-toggle"
            type="button"
            onClick={() => setExpanded((open) => !open)}
            aria-expanded={isExpanded}
          >
            {isExpanded ? "Hide details" : "Show details"}
          </button>
        }
      />
      <div className="card-label subagent-result-card-label">Thinking</div>
      {isExpanded ? (
        <>
          <div className="subagent-result-header">
            <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
          </div>
          <DeferredMarkdownContent
            appearance={appearance}
            markdown={message.summary}
            onOpenSourceLink={onOpenSourceLink}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
            workspaceRoot={workspaceRoot}
          />
        </>
      ) : null}
    </article>
  );
}

function ApprovalCard({
  message,
  onApprovalDecision,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ApprovalMessage;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const decided = message.decision !== "pending";
  const chosen = (d: ApprovalDecision) => (message.decision === d ? " chosen" : "");
  const resolvedDecision = message.decision === "pending" ? null : message.decision;

  return (
    <article className={`message-card approval-card${decided ? " decided" : ""}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Approval</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredHighlightedCodeBlock
        className="approval-command"
        code={message.command}
        language={message.commandLanguage ?? "bash"}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>
      <div className="approval-actions">
        <button
          className={`approval-button${chosen("accepted")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "accepted")}
          disabled={decided}
        >
          Approve
        </button>
        <button
          className={`approval-button${chosen("acceptedForSession")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "acceptedForSession")}
          disabled={decided}
        >
          Approve for session
        </button>
        <button
          className={`approval-button approval-button-reject${chosen("rejected")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "rejected")}
          disabled={decided}
        >
          Reject
        </button>
      </div>
      {resolvedDecision ? (
        <p className="approval-result">Decision: {renderDecision(resolvedDecision)}</p>
      ) : null}
    </article>
  );
}

type UserInputDraftField = {
  customAnswer: string;
  selectedOption: string;
};

type McpElicitationDraftField = {
  selectedOption: string;
  selections: string[];
  text: string;
};

function buildUserInputDraft(
  questions: UserInputQuestion[],
  submittedAnswers?: Record<string, string[]> | null,
): Record<string, UserInputDraftField> {
  const next: Record<string, UserInputDraftField> = {};
  for (const question of questions) {
    const answer = submittedAnswers?.[question.id]?.[0] ?? "";
    const optionLabels = new Set((question.options ?? []).map((option) => option.label));
    if (optionLabels.has(answer)) {
      next[question.id] = {
        customAnswer: "",
        selectedOption: answer,
      };
      continue;
    }

    next[question.id] = {
      customAnswer: answer === "[secret provided]" ? "" : answer,
      selectedOption: question.isOther && answer ? "__other__" : "",
    };
  }
  return next;
}

function buildUserInputSummary(
  message: UserInputRequestMessage,
  searchQuery: string,
  searchHighlightTone: SearchHighlightTone,
) {
  const submittedAnswers = message.submittedAnswers ?? {};
  return message.questions
    .filter((question) => submittedAnswers[question.id]?.length)
    .map((question) => (
      <div key={question.id} className="user-input-summary-row">
        <div className="user-input-summary-header">
          {renderHighlightedText(question.header, searchQuery, searchHighlightTone)}
        </div>
        <div className="user-input-summary-value">
          {renderHighlightedText(
            submittedAnswers[question.id]!.join(", "),
            searchQuery,
            searchHighlightTone,
          )}
        </div>
      </div>
    ));
}

function UserInputRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: UserInputRequestMessage;
  onSubmit: (messageId: string, answers: Record<string, string[]>) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [draft, setDraft] = useState<Record<string, UserInputDraftField>>(() =>
    buildUserInputDraft(message.questions, message.submittedAnswers),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const pending = message.state === "pending";

  useEffect(() => {
    setDraft(buildUserInputDraft(message.questions, message.submittedAnswers));
    setValidationError(null);
  }, [message.id, message.questions, message.state, message.submittedAnswers]);

  function updateField(questionId: string, nextField: Partial<UserInputDraftField>) {
    setDraft((current) => ({
      ...current,
      [questionId]: {
        customAnswer: current[questionId]?.customAnswer ?? "",
        selectedOption: current[questionId]?.selectedOption ?? "",
        ...nextField,
      },
    }));
  }

  function handleSubmit() {
    const answers: Record<string, string[]> = {};
    for (const question of message.questions) {
      const field = draft[question.id] ?? { customAnswer: "", selectedOption: "" };
      const optionLabels = new Set((question.options ?? []).map((option) => option.label));
      let answer = "";
      if (field.selectedOption && field.selectedOption !== "__other__") {
        answer = field.selectedOption;
      } else {
        answer = field.customAnswer.trim();
      }

      if (!answer) {
        setValidationError(`Answer "${question.header}" before submitting.`);
        return;
      }
      if (optionLabels.size > 0 && !optionLabels.has(answer) && !question.isOther) {
        setValidationError(`"${question.header}" must use one of the provided options.`);
        return;
      }

      answers[question.id] = [answer];
    }

    setValidationError(null);
    onSubmit(message.id, answers);
  }

  return (
    <article className={`message-card user-input-card${pending ? "" : " decided"}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Input request</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>

      <div className="user-input-questions">
        {message.questions.map((question) => {
          const field = draft[question.id] ?? { customAnswer: "", selectedOption: "" };
          const options = question.options ?? [];
          const inputType = question.isSecret ? "password" : "text";
          const usesOther = !!question.isOther;
          const showFreeform = options.length === 0 || field.selectedOption === "__other__";

          return (
            <section key={question.id} className="user-input-question">
              <div className="user-input-question-header">
                {renderHighlightedText(question.header, searchQuery, searchHighlightTone)}
              </div>
              <p className="support-copy">
                {renderHighlightedText(question.question, searchQuery, searchHighlightTone)}
              </p>

              {options.length > 0 ? (
                <div className="user-input-options">
                  {options.map((option) => (
                    <label key={option.label} className="user-input-option">
                      <input
                        type="radio"
                        name={`user-input-${message.id}-${question.id}`}
                        checked={field.selectedOption === option.label}
                        disabled={!pending}
                        onChange={() =>
                          updateField(question.id, {
                            customAnswer: "",
                            selectedOption: option.label,
                          })
                        }
                      />
                      <span>
                        <strong>
                          {renderHighlightedText(option.label, searchQuery, searchHighlightTone)}
                        </strong>
                        <span className="user-input-option-description">
                          {renderHighlightedText(
                            option.description,
                            searchQuery,
                            searchHighlightTone,
                          )}
                        </span>
                      </span>
                    </label>
                  ))}
                  {usesOther ? (
                    <label className="user-input-option">
                      <input
                        type="radio"
                        name={`user-input-${message.id}-${question.id}`}
                        checked={field.selectedOption === "__other__"}
                        disabled={!pending}
                        onChange={() => updateField(question.id, { selectedOption: "__other__" })}
                      />
                      <span>Other</span>
                    </label>
                  ) : null}
                </div>
              ) : null}

              {showFreeform ? (
                <input
                  className="user-input-text"
                  type={inputType}
                  value={field.customAnswer}
                  disabled={!pending}
                  onChange={(event) =>
                    updateField(question.id, { customAnswer: event.target.value })
                  }
                />
              ) : null}
            </section>
          );
        })}
      </div>

      {!pending ? (
        <div className="user-input-summary">
          {buildUserInputSummary(message, searchQuery, searchHighlightTone)}
        </div>
      ) : null}

      {validationError ? <p className="approval-result">{validationError}</p> : null}

      {pending ? (
        <div className="approval-actions">
          <button className="approval-button" type="button" onClick={handleSubmit}>
            Submit answers
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

function isJsonObject(
  value: JsonValue | null | undefined,
): value is { [key: string]: JsonValue | undefined } {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function mcpSingleSelectOptions(schema: McpElicitationPrimitiveSchema) {
  if (schema.type !== "string") {
    return [];
  }
  if (schema.oneOf?.length) {
    return schema.oneOf.map((option) => ({ label: option.title, value: option.const }));
  }
  return (schema.enum ?? []).map((value, index) => ({
    label: schema.enumNames?.[index] ?? value,
    value,
  }));
}

function mcpMultiSelectOptions(schema: McpElicitationPrimitiveSchema) {
  if (schema.type !== "array") {
    return [];
  }
  if (schema.items.anyOf?.length) {
    return schema.items.anyOf.map((option) => ({ label: option.title, value: option.const }));
  }
  return (schema.items.enum ?? []).map((value) => ({ label: value, value }));
}

function buildMcpElicitationDraft(
  message: McpElicitationRequestMessage,
): Record<string, McpElicitationDraftField> {
  if (message.request.mode !== "form") {
    return {};
  }

  const submitted = isJsonObject(message.submittedContent) ? message.submittedContent : {};
  const next: Record<string, McpElicitationDraftField> = {};
  for (const [fieldName, schema] of Object.entries(message.request.requestedSchema.properties)) {
    if (!schema) {
      continue;
    }
    const submittedValue = submitted[fieldName];
    switch (schema.type) {
      case "boolean":
        next[fieldName] = {
          selectedOption:
            typeof submittedValue === "boolean"
              ? submittedValue
                ? "true"
                : "false"
              : "",
          selections: [],
          text: "",
        };
        break;
      case "array":
        next[fieldName] = {
          selectedOption: "",
          selections: Array.isArray(submittedValue)
            ? submittedValue.filter((value): value is string => typeof value === "string")
            : (schema.default ?? []),
          text: "",
        };
        break;
      case "number":
      case "integer":
        next[fieldName] = {
          selectedOption: "",
          selections: [],
          text:
            typeof submittedValue === "number"
              ? String(submittedValue)
              : schema.default !== undefined && schema.default !== null
                ? String(schema.default)
                : "",
        };
        break;
      case "string": {
        const options = mcpSingleSelectOptions(schema);
        const submittedText = typeof submittedValue === "string" ? submittedValue : "";
        next[fieldName] = {
          selectedOption: options.some((option) => option.value === submittedText) ? submittedText : "",
          selections: [],
          text:
            submittedText ||
            (typeof schema.default === "string" ? schema.default : ""),
        };
        break;
      }
    }
  }
  return next;
}

function formatMcpElicitationSummaryValue(value: JsonValue) {
  if (Array.isArray(value)) {
    return value.join(", ");
  }
  if (typeof value === "boolean") {
    return value ? "Yes" : "No";
  }
  if (value === null) {
    return "null";
  }
  if (typeof value === "object") {
    return JSON.stringify(value);
  }
  return String(value);
}

function buildMcpElicitationSummary(
  message: McpElicitationRequestMessage,
  searchQuery: string,
  searchHighlightTone: SearchHighlightTone,
) {
  if (!isJsonObject(message.submittedContent) || message.request.mode !== "form") {
    return null;
  }
  const submittedContent = message.submittedContent;

  return Object.entries(message.request.requestedSchema.properties)
    .filter(([fieldName]) => submittedContent[fieldName] !== undefined)
    .map(([fieldName, schema]) => {
      const value = submittedContent[fieldName];
      if (!schema || value === undefined) {
        return null;
      }
      return (
        <div key={fieldName} className="user-input-summary-row">
          <div className="user-input-summary-header">
            {renderHighlightedText(schema.title ?? fieldName, searchQuery, searchHighlightTone)}
          </div>
          <div className="user-input-summary-value">
            {renderHighlightedText(
              formatMcpElicitationSummaryValue(value),
              searchQuery,
              searchHighlightTone,
            )}
          </div>
        </div>
      );
    });
}

function McpElicitationRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: McpElicitationRequestMessage;
  onSubmit: (messageId: string, action: McpElicitationAction, content?: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [draft, setDraft] = useState<Record<string, McpElicitationDraftField>>(() =>
    buildMcpElicitationDraft(message),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const pending = message.state === "pending";

  useEffect(() => {
    setDraft(buildMcpElicitationDraft(message));
    setValidationError(null);
  }, [message]);

  function updateField(fieldName: string, nextField: Partial<McpElicitationDraftField>) {
    setDraft((current) => ({
      ...current,
      [fieldName]: {
        selectedOption: current[fieldName]?.selectedOption ?? "",
        selections: current[fieldName]?.selections ?? [],
        text: current[fieldName]?.text ?? "",
        ...nextField,
      },
    }));
  }

  function handleSubmit(action: McpElicitationAction) {
    if (message.request.mode !== "form" || action !== "accept") {
      setValidationError(null);
      onSubmit(message.id, action);
      return;
    }

    const required = new Set(message.request.requestedSchema.required ?? []);
    const content: Record<string, JsonValue> = {};

    for (const [fieldName, schema] of Object.entries(message.request.requestedSchema.properties)) {
      if (!schema) {
        continue;
      }
      const field = draft[fieldName] ?? { selectedOption: "", selections: [], text: "" };
      switch (schema.type) {
        case "string": {
          const options = mcpSingleSelectOptions(schema);
          const hasOptions = options.length > 0;
          const rawValue = hasOptions ? field.selectedOption : field.text.trim();
          if (!rawValue) {
            if (required.has(fieldName)) {
              setValidationError(`Answer "${schema.title ?? fieldName}" before accepting.`);
              return;
            }
            break;
          }
          if (hasOptions && !options.some((option) => option.value === rawValue)) {
            setValidationError(`"${schema.title ?? fieldName}" must use one of the provided options.`);
            return;
          }
          const valueLength = Array.from(rawValue).length;
          if (schema.minLength != null && valueLength < schema.minLength) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at least ${schema.minLength} characters.`,
            );
            return;
          }
          if (schema.maxLength != null && valueLength > schema.maxLength) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at most ${schema.maxLength} characters.`,
            );
            return;
          }
          content[fieldName] = rawValue;
          break;
        }
        case "number":
        case "integer": {
          const rawValue = field.text.trim();
          if (!rawValue) {
            if (required.has(fieldName)) {
              setValidationError(`Answer "${schema.title ?? fieldName}" before accepting.`);
              return;
            }
            break;
          }
          const numericValue = Number(rawValue);
          if (!Number.isFinite(numericValue)) {
            setValidationError(`"${schema.title ?? fieldName}" must be a valid number.`);
            return;
          }
          if (schema.type === "integer" && !Number.isInteger(numericValue)) {
            setValidationError(`"${schema.title ?? fieldName}" must be a whole number.`);
            return;
          }
          if (schema.minimum != null && numericValue < schema.minimum) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at least ${schema.minimum}.`,
            );
            return;
          }
          if (schema.maximum != null && numericValue > schema.maximum) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at most ${schema.maximum}.`,
            );
            return;
          }
          content[fieldName] = numericValue;
          break;
        }
        case "boolean": {
          if (!field.selectedOption) {
            if (required.has(fieldName)) {
              setValidationError(`Answer "${schema.title ?? fieldName}" before accepting.`);
              return;
            }
            break;
          }
          content[fieldName] = field.selectedOption === "true";
          break;
        }
        case "array": {
          const options = mcpMultiSelectOptions(schema);
          if (field.selections.length === 0) {
            if (required.has(fieldName) || (schema.minItems ?? 0) > 0) {
              setValidationError(`Choose at least one option for "${schema.title ?? fieldName}".`);
              return;
            }
            break;
          }
          if (!field.selections.every((selection) => options.some((option) => option.value === selection))) {
            setValidationError(`"${schema.title ?? fieldName}" must use one of the provided options.`);
            return;
          }
          if (schema.minItems != null && field.selections.length < schema.minItems) {
            setValidationError(
              `"${schema.title ?? fieldName}" must include at least ${schema.minItems} selections.`,
            );
            return;
          }
          if (schema.maxItems != null && field.selections.length > schema.maxItems) {
            setValidationError(
              `"${schema.title ?? fieldName}" must include at most ${schema.maxItems} selections.`,
            );
            return;
          }
          content[fieldName] = field.selections;
          break;
        }
      }
    }

    setValidationError(null);
    onSubmit(message.id, action, content);
  }

  return (
    <article className={`message-card user-input-card${pending ? "" : " decided"}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">MCP input</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>

      {message.request.mode === "url" ? (
        <p className="support-copy">
          <a href={message.request.url} target="_blank" rel="noreferrer">
            {renderHighlightedText(message.request.url, searchQuery, searchHighlightTone)}
          </a>
        </p>
      ) : (
        <div className="user-input-questions">
          {Object.entries(message.request.requestedSchema.properties).map(([fieldName, schema]) => {
            if (!schema) {
              return null;
            }
            const field = draft[fieldName] ?? { selectedOption: "", selections: [], text: "" };
            const label = schema.title ?? fieldName;
            const description = schema.description ?? message.request.message;
            const singleOptions = mcpSingleSelectOptions(schema);
            const multiOptions = mcpMultiSelectOptions(schema);
            return (
              <section key={fieldName} className="user-input-question">
                <div className="user-input-question-header">
                  {renderHighlightedText(label, searchQuery, searchHighlightTone)}
                </div>
                {description ? (
                  <p className="support-copy">
                    {renderHighlightedText(description, searchQuery, searchHighlightTone)}
                  </p>
                ) : null}

                {schema.type === "string" && singleOptions.length > 0 ? (
                  <div className="user-input-options">
                    {singleOptions.map((option) => (
                      <label key={option.value} className="user-input-option">
                        <input
                          type="radio"
                          name={`mcp-elicitation-${message.id}-${fieldName}`}
                          checked={field.selectedOption === option.value}
                          disabled={!pending}
                          onChange={() => updateField(fieldName, { selectedOption: option.value })}
                        />
                        <span>{renderHighlightedText(option.label, searchQuery, searchHighlightTone)}</span>
                      </label>
                    ))}
                  </div>
                ) : null}

                {schema.type === "array" ? (
                  <div className="user-input-options">
                    {multiOptions.map((option) => (
                      <label key={option.value} className="user-input-option">
                        <input
                          type="checkbox"
                          checked={field.selections.includes(option.value)}
                          disabled={!pending}
                          onChange={(event) =>
                            updateField(fieldName, {
                              selections: event.target.checked
                                ? [...field.selections, option.value]
                                : field.selections.filter((value) => value !== option.value),
                            })
                          }
                        />
                        <span>{renderHighlightedText(option.label, searchQuery, searchHighlightTone)}</span>
                      </label>
                    ))}
                  </div>
                ) : null}

                {schema.type === "boolean" ? (
                  <div className="user-input-options">
                    <label className="user-input-option">
                      <input
                        type="radio"
                        name={`mcp-elicitation-${message.id}-${fieldName}`}
                        checked={field.selectedOption === "true"}
                        disabled={!pending}
                        onChange={() => updateField(fieldName, { selectedOption: "true" })}
                      />
                      <span>Yes</span>
                    </label>
                    <label className="user-input-option">
                      <input
                        type="radio"
                        name={`mcp-elicitation-${message.id}-${fieldName}`}
                        checked={field.selectedOption === "false"}
                        disabled={!pending}
                        onChange={() => updateField(fieldName, { selectedOption: "false" })}
                      />
                      <span>No</span>
                    </label>
                  </div>
                ) : null}

                {(schema.type === "number" || schema.type === "integer" || (schema.type === "string" && singleOptions.length === 0)) ? (
                  <input
                    className="user-input-text"
                    type={schema.type === "number" || schema.type === "integer" ? "number" : "text"}
                    value={field.text}
                    min={schema.type === "number" || schema.type === "integer" ? (schema.minimum ?? undefined) : undefined}
                    max={schema.type === "number" || schema.type === "integer" ? (schema.maximum ?? undefined) : undefined}
                    minLength={schema.type === "string" ? (schema.minLength ?? undefined) : undefined}
                    maxLength={schema.type === "string" ? (schema.maxLength ?? undefined) : undefined}
                    disabled={!pending}
                    onChange={(event) => updateField(fieldName, { text: event.target.value })}
                  />
                ) : null}
              </section>
            );
          })}
        </div>
      )}

      {!pending ? (
        <div className="user-input-summary">
          <div className="user-input-summary-row">
            <div className="user-input-summary-header">Decision</div>
            <div className="user-input-summary-value">{message.submittedAction ?? message.state}</div>
          </div>
          {buildMcpElicitationSummary(message, searchQuery, searchHighlightTone)}
        </div>
      ) : null}

      {validationError ? <p className="approval-result">{validationError}</p> : null}

      {pending ? (
        <div className="approval-actions">
          <button className="approval-button" type="button" onClick={() => handleSubmit("accept")}>
            Accept
          </button>
          <button className="approval-button" type="button" onClick={() => handleSubmit("decline")}>
            Decline
          </button>
          <button
            className="approval-button approval-button-reject"
            type="button"
            onClick={() => handleSubmit("cancel")}
          >
            Cancel
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

function formatJsonValueForEditor(value: JsonValue | null | undefined) {
  return JSON.stringify(value ?? {}, null, 2);
}

function CodexAppRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CodexAppRequestMessage;
  onSubmit: (messageId: string, result: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const pending = message.state === "pending";
  const [draft, setDraft] = useState(() => formatJsonValueForEditor(message.submittedResult));
  const [validationError, setValidationError] = useState<string | null>(null);

  useEffect(() => {
    setDraft(formatJsonValueForEditor(message.submittedResult));
    setValidationError(null);
  }, [message]);

  function handleSubmit() {
    try {
      const parsed = JSON.parse(draft) as JsonValue;
      setValidationError(null);
      onSubmit(message.id, parsed);
    } catch {
      setValidationError("Response must be valid JSON.");
    }
  }

  return (
    <article className={`message-card user-input-card${pending ? "" : " decided"}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Codex request</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>

      <div className="user-input-summary">
        <div className="user-input-summary-row">
          <div className="user-input-summary-header">Method</div>
          <div className="user-input-summary-value">{message.method}</div>
        </div>
      </div>

      <div className="codex-request-json-block">
        <div className="user-input-summary-header">Request payload</div>
        <pre>{formatJsonValueForEditor(message.params)}</pre>
      </div>

      {pending ? (
        <label className="codex-request-editor">
          <span className="user-input-summary-header">JSON result</span>
          <textarea
            className="codex-request-textarea"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            spellCheck={false}
          />
        </label>
      ) : (
        <div className="codex-request-json-block">
          <div className="user-input-summary-header">Submitted result</div>
          <pre>{formatJsonValueForEditor(message.submittedResult)}</pre>
        </div>
      )}

      {validationError ? <p className="approval-result">{validationError}</p> : null}

      {pending ? (
        <div className="approval-actions">
          <button className="approval-button" type="button" onClick={handleSubmit}>
            Submit JSON result
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

type MarkdownAstNode = {
  type: string;
  value?: string;
  url?: string;
  children?: MarkdownAstNode[];
};

const MARKDOWN_BARE_FILE_REFERENCE_PATTERN =
  /(^|[\s([{"\'""`])((?:(?:\.\.?[\\/])|(?:\/[A-Za-z]:[\\/])|(?:[A-Za-z]:[\\/])|(?:\\\\)|\/)?(?:[\w.-]+[\\/])*[\w.-]+\.[A-Za-z0-9]{2,12}(?:\.?#L\d+(?:C\d+)?|:\d+(?::\d+)?))(?=$|[\s)\]},"\'"";!?`.])/g;
const MARKDOWN_BARE_FILE_REFERENCE_IGNORED_NODE_TYPES = new Set([
  "code",
  "definition",
  "html",
  "inlineCode",
  "link",
  "linkReference",
]);
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

function remarkAutolinkBareFileReferences() {
  return (tree: MarkdownAstNode) => {
    autolinkMarkdownBareFileReferences(tree);
  };
}

function autolinkMarkdownBareFileReferences(node: MarkdownAstNode) {
  if (!Array.isArray(node.children) || MARKDOWN_BARE_FILE_REFERENCE_IGNORED_NODE_TYPES.has(node.type)) {
    return;
  }

  for (let index = 0; index < node.children.length; index += 1) {
    const child = node.children[index];
    if (child.type === "text" && typeof child.value === "string") {
      const replacement = buildAutolinkedMarkdownTextNodes(child.value);
      if (replacement) {
        node.children.splice(index, 1, ...replacement);
        index += replacement.length - 1;
        continue;
      }
    }

    autolinkMarkdownBareFileReferences(child);
  }
}

function buildAutolinkedMarkdownTextNodes(text: string) {
  MARKDOWN_BARE_FILE_REFERENCE_PATTERN.lastIndex = 0;
  const nodes: MarkdownAstNode[] = [];
  let changed = false;
  let lastIndex = 0;
  let match = MARKDOWN_BARE_FILE_REFERENCE_PATTERN.exec(text);

  while (match) {
    const prefix = match[1] ?? "";
    const reference = match[2] ?? "";
    const matchIndex = match.index;
    const referenceStartIndex = matchIndex + prefix.length;
    if (matchIndex > lastIndex) {
      nodes.push(createMarkdownTextNode(text.slice(lastIndex, matchIndex)));
    }
    if (prefix) {
      nodes.push(createMarkdownTextNode(prefix));
    }
    nodes.push(createMarkdownLinkNode(reference));
    lastIndex = referenceStartIndex + reference.length;
    changed = true;
    match = MARKDOWN_BARE_FILE_REFERENCE_PATTERN.exec(text);
  }

  if (!changed) {
    return null;
  }

  if (lastIndex < text.length) {
    nodes.push(createMarkdownTextNode(text.slice(lastIndex)));
  }

  return nodes.filter((node) => node.type !== "text" || (node.value?.length ?? 0) > 0);
}

function createMarkdownLinkNode(reference: string): MarkdownAstNode {
  return {
    type: "link",
    url: reference,
    children: [createMarkdownTextNode(reference)],
  };
}

function createMarkdownTextNode(value: string): MarkdownAstNode {
  return {
    type: "text",
    value,
  };
}

export function MarkdownContent({
  appearance = "dark",
  documentPath = null,
  markdown,
  onOpenSourceLink,
  preserveMermaidSource = false,
  renderMermaidDiagrams = true,
  searchQuery = "",
  searchHighlightTone = "match",
  showLineNumbers = false,
  startLineNumber = 1,
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  documentPath?: string | null;
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preserveMermaidSource?: boolean;
  renderMermaidDiagrams?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  showLineNumbers?: boolean;
  startLineNumber?: number | null;
  workspaceRoot?: string | null;
}) {
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
  const normalizedStartLineNumber = normalizeMarkdownStartLineNumber(startLineNumber);
  const mermaidDiagramCount = useMemo(
    () => (renderMermaidDiagrams ? countMermaidFences(markdown) : 0),
    [markdown, renderMermaidDiagrams],
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
    () => countMathExpressions(markdown),
    [markdown],
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
            top: Math.round(elementRect.top + elementRect.height / 2 - rootRect.top),
          };
        })
        .filter((marker): marker is MarkdownLineMarker => marker != null);

      setLineMarkers((currentMarkers) =>
        areMarkdownLineMarkersEqual(currentMarkers, nextMarkers) ? currentMarkers : nextMarkers,
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
    // `markdown` and `normalizedStartLineNumber` stay because they
    // DO change the set of `[data-markdown-line-start]` elements
    // the body scans.
  }, [markdown, normalizedStartLineNumber, showLineNumbers]);

  const rendered = useMemo(() => {
    const highlightChildren = (children: ReactNode) =>
      highlightReactNodeText(children, searchQuery, searchHighlightTone);
    const getLineAttributes = (sourcePosition: MarkdownSourcePosition | undefined) =>
      getMarkdownLineAttributes(sourcePosition, normalizedStartLineNumber, showLineNumbers);

    return (
      <ReactMarkdown
        rawSourcePos={showLineNumbers}
        transformLinkUri={transformMarkdownLinkUri}
        components={{
          a: ({ href, children, sourcePosition: _sourcePosition, ...props }) => {
            const isExternalLink = isExternalMarkdownHref(href ?? "");
            const fileLinkTarget = resolveMarkdownFileLinkTarget(href, workspaceRoot, documentPath);
            const displayLabel = buildMarkdownHrefDisplayLabel(href, children, workspaceRoot, documentPath);
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
              if (event.defaultPrevented || !fileLinkTarget || !onOpenSourceLinkRef.current) {
                return;
              }

              event.preventDefault();
              onOpenSourceLinkRef.current({
                ...fileLinkTarget,
                openInNewTab: event.ctrlKey || event.metaKey,
              });
            };

            return (
              <a
                {...props}
                href={href}
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
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            const language = className?.match(/language-([\w-]+)/)?.[1] ?? null;
            const code = String(children).replace(/\n$/, "");
            const inlineFileLinkTarget = inline
              ? resolveMarkdownFileLinkTarget(code, workspaceRoot, documentPath)
              : null;

            if (inline) {
              if (inlineFileLinkTarget && hasOpenSourceLink && !isInsideMarkdownLink) {
                const handleInlineCodeClick = (event: ReactMouseEvent<HTMLAnchorElement>) => {
                  event.preventDefault();
                  onOpenSourceLinkRef.current?.({
                    ...inlineFileLinkTarget,
                    openInNewTab: event.ctrlKey || event.metaKey,
                  });
                };

                return (
                  <a className="inline-code-link" href={code} draggable={false} onClick={handleInlineCodeClick}>
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
              const lineAttributes = suppressLineNumber ? null : getLineAttributes(sourcePosition);
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
                  lineAttributes={lineAttributes}
                />
              );
            }

            return (
              <HighlightedCodeBlock
                className="code-block"
                code={code}
                lineAttributes={suppressLineNumber ? null : getLineAttributes(sourcePosition)}
                language={language}
                showCopyButton
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            );
          },
          p: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <p {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
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
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <li {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                <MarkdownLineNumberSuppressedContext.Provider value>
                  {highlightChildren(children)}
                </MarkdownLineNumberSuppressedContext.Provider>
              </li>
            );
          },
          ul: ({ children, ordered: _ordered, depth: _depth, sourcePosition: _sourcePosition, ...props }) => (
            <ul {...props}>{children}</ul>
          ),
          ol: ({ children, ordered: _ordered, depth: _depth, sourcePosition: _sourcePosition, ...props }) => (
            <ol {...props}>{children}</ol>
          ),
          blockquote: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <blockquote {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                <MarkdownLineNumberSuppressedContext.Provider value>
                  {highlightChildren(children)}
                </MarkdownLineNumberSuppressedContext.Provider>
              </blockquote>
            );
          },
          h1: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <h1 {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                {highlightChildren(children)}
              </h1>
            );
          },
          h2: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <h2 {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                {highlightChildren(children)}
              </h2>
            );
          },
          h3: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <h3 {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                {highlightChildren(children)}
              </h3>
            );
          },
          h4: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <h4 {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                {highlightChildren(children)}
              </h4>
            );
          },
          h5: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <h5 {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
                {highlightChildren(children)}
              </h5>
            );
          },
          h6: ({ children, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <h6 {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}>
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
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <div
                className="markdown-table-scroll"
                {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})}
              >
                <table {...props}>{children}</table>
              </div>
            );
          },
          hr: ({ sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            return (
              <hr {...props} {...(suppressLineNumber ? {} : getLineAttributes(sourcePosition) ?? {})} />
            );
          },
          img: ({ alt, sourcePosition: _sourcePosition, ...props }) => (
            <img {...props} alt={alt ?? ""} draggable={false} />
          ),
          td: ({ children, isHeader: _isHeader, sourcePosition: _sourcePosition, ...props }) => (
            <td {...props}>{highlightChildren(children)}</td>
          ),
          th: ({ children, isHeader: _isHeader, sourcePosition: _sourcePosition, ...props }) => (
            <th {...props}>{highlightChildren(children)}</th>
          ),
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
          span: ({ children, className, node: _node, sourcePosition: _sourcePosition, ...props }) => {
            if (typeof className === "string" && /(^|\s)math(\s|$)/.test(className) && /inline/.test(className)) {
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
          div: ({ children, className, node: _node, sourcePosition, ...props }) => {
            const suppressLineNumber = useContext(MarkdownLineNumberSuppressedContext);
            if (typeof className === "string" && /(^|\s)math(\s|$)/.test(className)) {
              const lineAttributes = suppressLineNumber ? null : getLineAttributes(sourcePosition);
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
        {markdown}
      </ReactMarkdown>
    );
  }, [
    documentPath,
    appearance,
    hasTooManyMathExpressions,
    hasTooManyMermaidDiagrams,
    markdown,
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

  return (
    <div className={`markdown-copy-shell${showLineNumbers ? " markdown-copy-shell-with-line-numbers" : ""}`}>
      {showLineNumbers ? (
        <div className="markdown-line-gutter" aria-hidden="true" contentEditable={false}>
          {lineMarkers.map((marker) => (
            <span
              className="markdown-line-number"
              data-markdown-gutter-line={marker.line}
              key={`${marker.line}:${marker.top}`}
              style={{ top: `${marker.top}px` }}
              title={marker.range === String(marker.line) ? `Line ${marker.line}` : `Lines ${marker.range}`}
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
      </div>
    </div>
  );
}

function normalizeMarkdownStartLineNumber(startLineNumber: number | null) {
  if (startLineNumber == null || !Number.isFinite(startLineNumber) || startLineNumber < 1) {
    return null;
  }

  return Math.floor(startLineNumber);
}

function getMarkdownLineAttributes(
  sourcePosition: MarkdownSourcePosition | undefined,
  startLineNumber: number | null,
  showLineNumbers: boolean,
): MarkdownLineAttributes | null {
  if (!showLineNumbers || startLineNumber == null) {
    return null;
  }

  const lineNumber = getMarkdownRenderedLineNumber(sourcePosition, startLineNumber);
  if (lineNumber == null) {
    return null;
  }

  const rangeLabel = getMarkdownRenderedLineRangeLabel(sourcePosition, startLineNumber);
  const title = rangeLabel === String(lineNumber) ? `Line ${lineNumber}` : `Lines ${rangeLabel}`;

  return {
    "data-markdown-line-start": lineNumber,
    "data-markdown-line-range": rangeLabel,
    title,
  };
}

function getMarkdownRenderedLineNumber(
  sourcePosition: MarkdownSourcePosition | undefined,
  startLineNumber: number,
) {
  const sourceLine = sourcePosition?.start?.line;
  if (typeof sourceLine !== "number" || !Number.isFinite(sourceLine) || sourceLine < 1) {
    return null;
  }

  return startLineNumber + sourceLine - 1;
}

function getMarkdownRenderedLineRangeLabel(
  sourcePosition: MarkdownSourcePosition | undefined,
  startLineNumber: number,
) {
  const start = getMarkdownRenderedLineNumber(sourcePosition, startLineNumber);
  if (start == null) {
    return "";
  }

  const endLine = sourcePosition?.end?.line;
  if (typeof endLine !== "number" || !Number.isFinite(endLine) || endLine < 1) {
    return String(start);
  }

  const end = startLineNumber + endLine - 1;
  return end > start ? `${start}-${end}` : String(start);
}

export function areMarkdownLineMarkersEqual(
  currentMarkers: MarkdownLineMarker[],
  nextMarkers: MarkdownLineMarker[],
) {
  if (currentMarkers.length !== nextMarkers.length) {
    return false;
  }

  return currentMarkers.every((marker, index) => {
    const nextMarker = nextMarkers[index];
    return (
      marker.line === nextMarker?.line &&
      marker.range === nextMarker.range &&
      marker.top === nextMarker.top
    );
  });
}


