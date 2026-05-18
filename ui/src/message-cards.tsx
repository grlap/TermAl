import {
  memo,
  useEffect,
  useMemo,
  useState,
} from "react";
import { ExpandedPromptPanel } from "./ExpandedPromptPanel";
import {
  CheckIcon,
  CollapseIcon,
  CopyIcon,
  ExpandIcon,
  PreviewIcon,
} from "./message-card-icons";
import {
  MessageAttachmentList,
  MessageMeta,
  promptCommandMetaLabel,
} from "./message-card-meta";
import { copyTextToClipboard } from "./clipboard";
import { buildDiffPreviewModel } from "./diff-preview";
import { DeferredHeavyContent } from "./deferred-heavy-content";
import { FileChangesCard } from "./file-changes-card";
import { DeferredHighlightedCodeBlock } from "./highlighted-code-block";
import { MarkdownContent } from "./markdown-content";
import {
  CodexAppRequestCard,
  McpElicitationRequestCard,
  UserInputRequestCard,
} from "./message-input-request-cards";
import type { MarkdownFileLinkTarget } from "./markdown-links";
import {
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "./path-display";
import {
  containsSearchMatch,
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type {
  ApprovalDecision,
  ApprovalMessage,
  CommandMessage,
  DiffMessage,
  JsonValue,
  MarkdownMessage,
  McpElicitationAction,
  Message,
  ParallelAgentsMessage,
  SubagentResultMessage,
  TextMessage,
  ThinkingMessage,
} from "./types";
import {
  getErrorMessage,
  imageAttachmentSummaryLabel,
  mapCommandStatus,
  renderDecision,
} from "./app-utils";
import {
  parseConnectionRetryNotice,
  type ConnectionRetryDisplayState,
} from "./connection-retry";
import { ConnectionRetryCard } from "./connection-retry-card";
import {
  buildMarkdownPreviewText,
  estimateMarkdownBlockHeight,
  measureTextBlock,
} from "./deferred-render";
import { ParallelAgentsCard } from "./parallel-agents-card";
import type { MonacoAppearance } from "./monaco";
import {
  MessageNavigationButtons,
} from "./panels/conversation-navigation";

// Markdown qualifies as "heavy" — and therefore goes through
// `DeferredMarkdownContent`'s IntersectionObserver-gated render path —
// when EITHER the line count OR the character count crosses these
// thresholds. Both gates are OR'd, so a long single-line table or a
// short-but-very-wide block still qualifies.
//
// Tuning history:
//   - 1800 chars / 24 lines: original conservative gate, sized so that
//     even mid-size assistant explanations (a few paragraphs of prose,
//     no code) deferred. This made fast scrolling feel laggy because
//     ordinary message bodies blanked out and re-painted on dwell.
//   - 9000 chars / 24 lines (current): only genuinely heavy content
//     (long-form docs, large markdown tables, dumped logs in a fenced
//     block, big mermaid sources) goes through the deferred path;
//     typical assistant prose paints synchronously like plain text and
//     never flickers during fast scroll.
//
// Mermaid note: any ```mermaid``` fence inside the body triggers a
// real mermaid render in a sandboxed iframe (`MermaidDiagram` →
// `renderTermalMermaidDiagram`), capped at
// `MAX_MERMAID_DIAGRAMS_PER_DOCUMENT` (20). The first render of a
// non-trivial flowchart can take hundreds of ms, so messages with
// embedded mermaid are exactly the case the deferred wrapper exists
// to protect — even if the surrounding prose is short, the character
// count of the source fence usually pushes the body past this gate.
//
// If you raise these further, weigh the cost: every non-deferred
// markdown body re-runs the full remark/rehype/KaTeX/highlight pipeline
// on each mount/unmount the virtualizer performs. The deferred wrapper
// exists specifically to keep those pipelines off the hot path while
// the user is still scrolling. The cooldown that gates the deferred
// path between scroll inputs lives in
// `panels/VirtualizedConversationMessageList.tsx` as
// `DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS` — keep these two tunings in
// mind as a pair.
const HEAVY_MARKDOWN_CHARACTER_THRESHOLD = 9000;
const HEAVY_MARKDOWN_LINE_THRESHOLD = 24;

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
export { areMarkdownLineMarkersEqual } from "./markdown-line-markers";
export { MarkdownContent } from "./markdown-content";
export { MessageMetaMarkerMenuProvider } from "./message-meta-marker-menu-context";

export const MessageCard = memo(
  function MessageCard({
    appearance = "dark",
    message,
    onOpenDiffPreview,
    onOpenSourceLink,
    preferImmediateHeavyRender = false,
    isStreamingAssistantTextMessage = false,
    onApprovalDecision,
    onUserInputSubmit,
    onMcpElicitationSubmit = NOOP_MCP_ELICITATION_SUBMIT,
    onCodexAppRequestSubmit = NOOP_CODEX_APP_REQUEST_SUBMIT,
    onOpenParallelAgentSession,
    onInsertParallelAgentResult,
    onCancelParallelAgent,
    parallelAgentActionsEnabled = true,
    searchQuery = "",
    searchHighlightTone = "match",
    isLatestAssistantMessage = true,
    connectionRetryDisplayState,
    workspaceRoot = null,
  }: {
    appearance?: MonacoAppearance;
    message: Message;
    onOpenDiffPreview?: (message: DiffMessage) => void;
    onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
    preferImmediateHeavyRender?: boolean;
    isStreamingAssistantTextMessage?: boolean;
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
    onUserInputSubmit: (
      messageId: string,
      answers: Record<string, string[]>,
    ) => void;
    onMcpElicitationSubmit?: (
      messageId: string,
      action: McpElicitationAction,
      content?: JsonValue,
    ) => void;
    onCodexAppRequestSubmit?: (messageId: string, result: JsonValue) => void;
    onOpenParallelAgentSession?: (agentId: string) => Promise<void> | void;
    onInsertParallelAgentResult?: (agentId: string) => Promise<void> | void;
    onCancelParallelAgent?: (agentId: string) => Promise<void> | void;
    parallelAgentActionsEnabled?: boolean;
    searchQuery?: string;
    searchHighlightTone?: SearchHighlightTone;
    // When false, `ConnectionRetryCard` renders the resolved (static, past-tense)
    // variant because later assistant output exists and the reconnect obviously
    // succeeded. Defaults to true so tests and callers that have not opted in
    // keep the pre-existing "live spinner" behaviour.
    isLatestAssistantMessage?: boolean;
    connectionRetryDisplayState?: ConnectionRetryDisplayState;
    workspaceRoot?: string | null;
  }) {
    switch (message.type) {
      case "text": {
        const connectionRetryNotice =
          message.author === "assistant"
            ? parseConnectionRetryNotice(message.text)
            : null;
        const commandLabel =
          message.author === "you"
            ? promptCommandMetaLabel(message.text, message.expandedText)
            : null;
        // Assistant text uses one stable render pipeline:
        // `<DeferredMarkdownContent>` wraps `<MarkdownContent>` for both
        // streaming and settled messages, regardless of whether the body is
        // prose or structured Markdown. Earlier revisions swapped component
        // types based on per-message structure detection and a host fast-path
        // flag, which re-mounted the rendered subtree mid-stream and at turn
        // end. The single pipeline keeps the subtree stable while the
        // `isStreaming` flag still gates partial-block deferral and immediate
        // heavy-content activation.
        //
        // Non-assistant messages (user, system) skip this branch
        // entirely and render as plain text (see the `:` arm of the
        // outer ternary further below).
        const shouldRenderStreamingAssistantText =
          isStreamingAssistantTextMessage &&
          message.author === "assistant" &&
          searchQuery.trim().length === 0;

        if (connectionRetryNotice) {
          const retryDisplayState =
            connectionRetryDisplayState ??
            (isLatestAssistantMessage ? "live" : "resolved");
          return (
            <ConnectionRetryCard
              meta={
                <MessageMeta
                  author={message.author}
                  timestamp={message.timestamp}
                />
              }
              notice={connectionRetryNotice}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
              displayState={retryDisplayState}
            />
          );
        }

        // User prompts get inline ⬆ / ⬇ navigation so a long conversation can
        // be walked one prompt at a time without manual scrolling. Assistant
        // text intentionally does not — the user wanted to step between their
        // own questions, not the agent's replies.
        const showUserPromptNavigation = message.author === "you";
        return (
          <article className={`message-card bubble bubble-${message.author}`}>
            <MessageMeta
              author={message.author}
              timestamp={message.timestamp}
              trailing={
                <>
                  {commandLabel ? (
                    <span className="message-meta-tag">{commandLabel}</span>
                  ) : null}
                  {showUserPromptNavigation ? (
                    <MessageNavigationButtons
                      kind="userPrompt"
                      messageId={message.id}
                    />
                  ) : null}
                </>
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
              <DeferredMarkdownContent
                appearance={appearance}
                isStreaming={shouldRenderStreamingAssistantText}
                markdown={message.text}
                onOpenSourceLink={onOpenSourceLink}
                preferImmediateRender={preferImmediateHeavyRender}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
                workspaceRoot={workspaceRoot}
              />
            ) : message.text ? (
              <>
                <p className="plain-text-copy">
                  {renderHighlightedText(
                    message.text,
                    searchQuery,
                    searchHighlightTone,
                  )}
                </p>
                {message.expandedText ? (
                  <ExpandedPromptPanel
                    expandedText={message.expandedText}
                    storageKey={message.id}
                    searchQuery={searchQuery}
                    searchHighlightTone={searchHighlightTone}
                  />
                ) : null}
              </>
            ) : (
              <p className="support-copy">
                {imageAttachmentSummaryLabel(message.attachments?.length ?? 0)}
              </p>
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
            preferImmediateHeavyRender={preferImmediateHeavyRender}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
            workspaceRoot={workspaceRoot}
          />
        );
      case "command":
        return (
          <CommandCard
            message={message}
            preferImmediateHeavyRender={preferImmediateHeavyRender}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
          />
        );
      case "diff":
        return (
          <DiffCard
            message={message}
            onOpenPreview={() => onOpenDiffPreview?.(message)}
            preferImmediateHeavyRender={preferImmediateHeavyRender}
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
            preferImmediateHeavyRender={preferImmediateHeavyRender}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
            workspaceRoot={workspaceRoot}
          />
        );
      case "parallelAgents":
        return (
          <ParallelAgentsCard
            message={message}
            onOpenAgentSession={onOpenParallelAgentSession}
            onInsertAgentResult={onInsertParallelAgentResult}
            onCancelAgent={onCancelParallelAgent}
            actionsEnabled={parallelAgentActionsEnabled}
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
            preferImmediateHeavyRender={preferImmediateHeavyRender}
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
            preferImmediateHeavyRender={preferImmediateHeavyRender}
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
  },
  (previous, next) => {
    const previousParallelActionsEnabled =
      previous.parallelAgentActionsEnabled !== false;
    const nextParallelActionsEnabled =
      next.parallelAgentActionsEnabled !== false;
    const parallelActionPropsEqual =
      previous.message.type !== "parallelAgents" ||
      (!previousParallelActionsEnabled && !nextParallelActionsEnabled) ||
      (previousParallelActionsEnabled === nextParallelActionsEnabled &&
        previous.onOpenParallelAgentSession === next.onOpenParallelAgentSession &&
        previous.onInsertParallelAgentResult ===
          next.onInsertParallelAgentResult &&
        previous.onCancelParallelAgent === next.onCancelParallelAgent);

    return (
      previous.appearance === next.appearance &&
      previous.message === next.message &&
      previous.onOpenDiffPreview === next.onOpenDiffPreview &&
      previous.onOpenSourceLink === next.onOpenSourceLink &&
      previous.preferImmediateHeavyRender === next.preferImmediateHeavyRender &&
      previous.isStreamingAssistantTextMessage ===
        next.isStreamingAssistantTextMessage &&
      previous.onApprovalDecision === next.onApprovalDecision &&
      previous.onUserInputSubmit === next.onUserInputSubmit &&
      previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
      previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
      parallelActionPropsEqual &&
      previous.searchQuery === next.searchQuery &&
      previous.searchHighlightTone === next.searchHighlightTone &&
      previous.isLatestAssistantMessage === next.isLatestAssistantMessage &&
      previous.connectionRetryDisplayState ===
        next.connectionRetryDisplayState &&
      previous.workspaceRoot === next.workspaceRoot
    );
  },
);

/*
 * Streaming-stable assistant markdown wrapper.
 *
 * Always returns the same JSX shape — `<DeferredHeavyContent>` wrapping
 * `<MarkdownContent>` — regardless of whether the message is mid-stream
 * or settled, light or heavy, has a search match or not. This is what
 * gives `<MarkdownContent>` a stable React tree position across the
 * streaming → settled transition and prevents the full subtree remount
 * that previously caused visible flicker (Mermaid diagrams re-rendering,
 * KaTeX nodes re-mounting, code blocks losing scroll position, tables
 * blinking) the moment a turn ended.
 *
 * `isStreaming` is honored two ways:
 *   - It is passed through to `MarkdownContent`, which uses it to gate
 *     the partial-block deferral splitter (`markdown-streaming-split.ts`).
 *   - It forces `preferImmediateRender = true` on the outer
 *     `DeferredHeavyContent`, so streaming content never goes behind
 *     the heavy-content activation gate. The placeholder (used by the
 *     gate when `shouldGate` is true) is correspondingly elided so it
 *     can never appear during streaming.
 *
 * `DeferredHeavyContent`'s `isActivated` state is monotonic (only flips
 * from false → true), so when a heavy message transitions out of
 * streaming, the wrapper stays activated and content stays visible —
 * the parent's `preferImmediateRender` only matters for the initial
 * mount of a settled heavy bubble.
 */
function DeferredMarkdownContent({
  appearance = "dark",
  documentPath = null,
  isStreaming = false,
  markdown,
  onOpenSourceLink,
  preferImmediateRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  documentPath?: string | null;
  isStreaming?: boolean;
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const metrics = useMemo(() => measureTextBlock(markdown), [markdown]);
  const showSearchHighlight = containsSearchMatch(markdown, searchQuery);
  // Heavy-content activation gate: only engages for settled, large,
  // non-search bubbles. Streaming content always renders immediately
  // (the user is watching it being authored). Search results always
  // render immediately (the highlighted match must be visible).
  const shouldGate =
    !isStreaming &&
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_MARKDOWN_LINE_THRESHOLD ||
      markdown.length >= HEAVY_MARKDOWN_CHARACTER_THRESHOLD);
  const effectivePreferImmediateRender = !shouldGate || preferImmediateRender;

  return (
    <DeferredHeavyContent
      estimatedHeight={
        shouldGate ? estimateMarkdownBlockHeight(metrics.lineCount) : 0
      }
      preferImmediateRender={effectivePreferImmediateRender}
      placeholder={
        shouldGate ? (
          <div className="markdown-copy deferred-markdown-placeholder">
            <p className="plain-text-copy">
              {buildMarkdownPreviewText(markdown)}
            </p>
          </div>
        ) : null
      }
    >
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        isStreaming={isStreaming}
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </DeferredHeavyContent>
  );
}

function ThinkingCard({
  appearance = "dark",
  message,
  onOpenSourceLink,
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: ThinkingMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const markdown = message.lines.join("\n");

  return (
    <article className="message-card reasoning-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Thinking</div>
      <h3>
        {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
      </h3>
      <DeferredMarkdownContent
        appearance={appearance}
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        preferImmediateRender={preferImmediateHeavyRender}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </article>
  );
}

export function CommandCard({
  message,
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CommandMessage;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [inputExpanded, setInputExpanded] = useState(false);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const [copiedSection, setCopiedSection] = useState<
    "command" | "output" | null
  >(null);
  const hasOutput = message.output.trim().length > 0;
  const displayOutput = hasOutput
    ? message.output
    : message.status === "running"
      ? "Awaiting output\u2026"
      : "No output";
  const canExpandCommand =
    message.command.split("\n").length > 10 || message.command.length > 480;
  const canExpandOutput =
    hasOutput &&
    (message.output.split("\n").length > 10 || message.output.length > 480);
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
          <span
            className={`chip chip-status chip-status-${statusTone} command-status-chip`}
          >
            {message.status}
          </span>
        }
      />
      <div className="card-label command-card-label">Command</div>

      <div className="command-panel">
        <div className="command-row">
          <div className="command-row-label">IN</div>
          <div className="command-row-body">
            <div
              className={`command-input-shell ${isInputExpanded ? "expanded" : "collapsed"}`}
            >
              <DeferredHighlightedCodeBlock
                className="command-text command-text-input"
                code={message.command}
                language={message.commandLanguage ?? "bash"}
                preferImmediateRender={preferImmediateHeavyRender}
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
              aria-label={
                copiedSection === "command" ? "Command copied" : "Copy command"
              }
              title={copiedSection === "command" ? "Copied" : "Copy command"}
            >
              {copiedSection === "command" ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandCommand ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setInputExpanded((open) => !open)}
                aria-label={
                  isInputExpanded ? "Collapse command" : "Expand command"
                }
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
                  preferImmediateRender={preferImmediateHeavyRender}
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
              aria-label={
                copiedSection === "output" ? "Output copied" : "Copy output"
              }
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
                aria-label={
                  isOutputExpanded ? "Collapse output" : "Expand output"
                }
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
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: DiffMessage;
  onOpenPreview: () => void;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const diffStats = useMemo(() => {
    const changeSummary = buildDiffPreviewModel(
      message.diff,
      message.changeType,
    ).changeSummary;
    return {
      addedLineCount:
        changeSummary.changedLineCount + changeSummary.addedLineCount,
      removedLineCount:
        changeSummary.changedLineCount + changeSummary.removedLineCount,
    };
  }, [message.changeType, message.diff]);
  const displayPath = useMemo(
    () => relativizePathToWorkspace(message.filePath, workspaceRoot),
    [message.filePath, workspaceRoot],
  );
  const canExpandDiff =
    message.diff.split("\n").length > 14 || message.diff.length > 900;
  const isExpanded =
    !canExpandDiff || expanded || searchQuery.trim().length > 0;

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
      <div className="card-label">
        {message.changeType === "create" ? "New file" : "File edit"}
      </div>
      <div className="command-panel diff-panel">
        <div className="command-row diff-file-row">
          <div className="command-row-label diff-file-label">
            <span>FILE</span>
            {diffStats.addedLineCount > 0 || diffStats.removedLineCount > 0 ? (
              <div className="diff-file-stats">
                {diffStats.addedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-added">
                    +{diffStats.addedLineCount}
                  </span>
                ) : null}
                {diffStats.removedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-removed">
                    -{diffStats.removedLineCount}
                  </span>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="command-row-body">
            <div
              className="diff-file-path"
              title={
                displayPath !== message.filePath ? message.filePath : undefined
              }
            >
              {renderHighlightedText(
                displayPath,
                searchQuery,
                searchHighlightTone,
              )}
            </div>
            <p className="diff-file-summary">
              {renderHighlightedText(
                message.summary,
                searchQuery,
                searchHighlightTone,
              )}
            </p>
          </div>
        </div>
        <div className="command-row diff-row">
          <div className="command-row-label">DIFF</div>
          <div className="command-row-body">
            <div
              className={`diff-preview-shell ${isExpanded ? "expanded" : "collapsed"}`}
            >
              <DeferredHighlightedCodeBlock
                className="diff-block diff-preview-text"
                code={message.diff}
                language={message.language ?? "diff"}
                pathHint={message.filePath}
                preferImmediateRender={preferImmediateHeavyRender}
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

function MarkdownCard({
  appearance = "dark",
  message,
  onOpenSourceLink,
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: MarkdownMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  return (
    <article className="message-card markdown-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Markdown</div>
      <h3>
        {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
      </h3>
      <DeferredMarkdownContent
        appearance={appearance}
        markdown={message.markdown}
        onOpenSourceLink={onOpenSourceLink}
        preferImmediateRender={preferImmediateHeavyRender}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </article>
  );
}

function SubagentResultCard({
  appearance = "dark",
  message,
  onOpenSourceLink,
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  message: SubagentResultMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isExpanded = expanded || isSearchExpanded;

  return (
    <article
      className={`message-card reasoning-card subagent-result-card${isExpanded ? " is-expanded" : ""}`}
    >
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
            <h3>
              {renderHighlightedText(
                message.title,
                searchQuery,
                searchHighlightTone,
              )}
            </h3>
          </div>
          <DeferredMarkdownContent
            appearance={appearance}
            markdown={message.summary}
            onOpenSourceLink={onOpenSourceLink}
            preferImmediateRender={preferImmediateHeavyRender}
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
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ApprovalMessage;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const decided = message.decision !== "pending";
  const chosen = (d: ApprovalDecision) =>
    message.decision === d ? " chosen" : "";
  const resolvedDecision =
    message.decision === "pending" ? null : message.decision;

  return (
    <article
      className={`message-card approval-card${decided ? " decided" : ""}`}
    >
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Approval</div>
      <h3>
        {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
      </h3>
      <DeferredHighlightedCodeBlock
        className="approval-command"
        code={message.command}
        language={message.commandLanguage ?? "bash"}
        preferImmediateRender={preferImmediateHeavyRender}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
      <p className="support-copy">
        {renderHighlightedText(
          message.detail,
          searchQuery,
          searchHighlightTone,
        )}
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
        <p className="approval-result">
          Decision: {renderDecision(resolvedDecision)}
        </p>
      ) : null}
    </article>
  );
}
