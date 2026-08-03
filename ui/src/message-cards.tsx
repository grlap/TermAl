import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { ExpandedPromptPanel } from "./ExpandedPromptPanel";
import {
  CheckIcon,
  CollapseIcon,
  CopyIcon,
  ExpandIcon,
} from "./message-card-icons";
import {
  MessageAttachmentList,
  MessageMeta,
  promptCommandMetaLabel,
} from "./message-card-meta";
import { copyTextToClipboard } from "./clipboard";
import { DeferredMarkdownContent } from "./deferred-markdown-content";
import { MailboxMessageLink } from "./mailbox-message-link";
import {
  DELEGATION_FAN_IN_AUTHOR_LABEL,
  isDelegationFanInText,
} from "./delegation-fan-in";
import { DelegationFanInMessage } from "./delegation-fan-in-message";
import { DiffCard } from "./diff-card";
import { FileChangesCard } from "./file-changes-card";
import { DeferredHighlightedCodeBlock } from "./highlighted-code-block";
import {
  isLongPeerMessage,
  isPeerMessageBatch,
  LongPeerMessage,
  PEER_MESSAGE_BATCH_AUTHOR_LABEL,
} from "./long-peer-message";
import {
  CodexAppRequestCard,
  McpElicitationRequestCard,
  UserInputRequestCard,
} from "./message-input-request-cards";
import type { MarkdownFileLinkTarget } from "./markdown-links";
import {
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
import { ParallelAgentsCard } from "./parallel-agents-card";
import type { MonacoAppearance } from "./monaco";
import { MessageNavigationButtons } from "./panels/conversation-navigation";

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
export { DiffCard } from "./diff-card";
export { isDelegationFanInText } from "./delegation-fan-in";
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
    approvalActionsEnabled = true,
    searchQuery = "",
    searchHighlightTone = "match",
    isLatestAssistantMessage = true,
    connectionRetryDisplayState,
    workspaceRoot = null,
    mailboxViewerSessionId,
    onOpenMailbox,
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
    approvalActionsEnabled?: boolean;
    searchQuery?: string;
    searchHighlightTone?: SearchHighlightTone;
    // When false, `ConnectionRetryCard` renders the resolved (static, past-tense)
    // variant because later assistant output exists and the reconnect obviously
    // succeeded. Defaults to true so tests and callers that have not opted in
    // keep the pre-existing "live spinner" behaviour.
    isLatestAssistantMessage?: boolean;
    connectionRetryDisplayState?: ConnectionRetryDisplayState;
    workspaceRoot?: string | null;
    mailboxViewerSessionId?: string | null;
    onOpenMailbox?: (mailboxId: string) => void;
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
        const isDelegationFanIn =
          message.author === "you" &&
          !message.source &&
          isDelegationFanInText(message.text);
        const isPeerBatch =
          message.author === "you" && isPeerMessageBatch(message.source);
        const shouldCollapsePeerMessage =
          message.author === "you" &&
          (Boolean(message.source) || isPeerBatch) &&
          isLongPeerMessage(message.text);
        // One chip slot: a fan-in is never also a slash command, so `commandLabel`
        // wins if both somehow matched.
        const metaTag =
          commandLabel ?? (isDelegationFanIn ? "Delegation" : null);
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
              sourceName={
                isDelegationFanIn
                  ? DELEGATION_FAN_IN_AUTHOR_LABEL
                  : isPeerBatch
                    ? PEER_MESSAGE_BATCH_AUTHOR_LABEL
                    : message.source?.name
              }
              trailing={
                <>
                  {metaTag ? (
                    <span className="message-meta-tag">{metaTag}</span>
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
            ) : message.source?.kind === "mailbox" ? null : message.text ? (
              isDelegationFanIn ? (
                <DelegationFanInMessage
                  text={message.text}
                  storageKey={message.id}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : shouldCollapsePeerMessage ? (
                <LongPeerMessage
                  text={message.text}
                  storageKey={message.id}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : (
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
              )
            ) : (
              <p className="support-copy">
                {imageAttachmentSummaryLabel(message.attachments?.length ?? 0)}
              </p>
            )}
            {message.source?.kind === "mailbox" &&
            message.source.mailbox &&
            mailboxViewerSessionId &&
            onOpenMailbox ? (
              <MailboxMessageLink
                senderName={message.source.name}
                sessionId={mailboxViewerSessionId}
                source={message.source.mailbox}
                onOpenMailbox={onOpenMailbox}
              />
            ) : null}
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
            actionsEnabled={approvalActionsEnabled}
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
        previous.onOpenParallelAgentSession ===
          next.onOpenParallelAgentSession &&
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
      previous.approvalActionsEnabled === next.approvalActionsEnabled &&
      previous.onUserInputSubmit === next.onUserInputSubmit &&
      previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
      previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
      parallelActionPropsEqual &&
      previous.searchQuery === next.searchQuery &&
      previous.searchHighlightTone === next.searchHighlightTone &&
      previous.isLatestAssistantMessage === next.isLatestAssistantMessage &&
      previous.connectionRetryDisplayState ===
        next.connectionRetryDisplayState &&
      previous.workspaceRoot === next.workspaceRoot &&
      previous.mailboxViewerSessionId === next.mailboxViewerSessionId &&
      previous.onOpenMailbox === next.onOpenMailbox
    );
  },
);

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
  const cardRef = useRef<HTMLElement | null>(null);
  const pendingAutomaticCollapseHeightRef = useRef<number | null>(null);
  const automaticCollapseAnimationRef = useRef<Animation | null>(null);
  const lastObservedCardHeightRef = useRef<number | null>(null);
  const [detailsExpanded, setDetailsExpanded] = useState(
    () => message.status !== "success",
  );
  const [entryExpanded, setEntryExpanded] = useState(
    () => message.status !== "running",
  );
  const [detailsToggled, setDetailsToggled] = useState(false);
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
  const canCollapseDetails = message.status === "success";
  const isDetailsExpanded =
    !canCollapseDetails || detailsExpanded || isSearchExpanded;
  const isInputExpanded = inputExpanded || isSearchExpanded;
  const isOutputExpanded = outputExpanded || isSearchExpanded;
  const commandSummary = firstSingleLine(message.command);
  const summaryMeta = [
    lineCountLabel(countVisibleLines(message.command), "in"),
    hasOutput
      ? lineCountLabel(countVisibleLines(message.output), "out")
      : "No output",
  ].join(" \u00b7 ");

  useLayoutEffect(() => {
    if (entryExpanded) {
      return;
    }

    const frameId = window.requestAnimationFrame(() => {
      setEntryExpanded(true);
    });
    return () => window.cancelAnimationFrame(frameId);
  }, [entryExpanded]);

  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card) {
      return;
    }

    const rememberHeight = () => {
      if (automaticCollapseAnimationRef.current === null) {
        lastObservedCardHeightRef.current = card.getBoundingClientRect().height;
      }
    };
    rememberHeight();
    const ResizeObserverCtor = globalThis.ResizeObserver;
    const resizeObserver =
      typeof ResizeObserverCtor === "function"
        ? new ResizeObserverCtor(rememberHeight)
        : null;
    resizeObserver?.observe(card);
    return () => resizeObserver?.disconnect();
  }, []);

  useLayoutEffect(() => {
    if (message.status !== "success") {
      setDetailsExpanded(true);
      setDetailsToggled(false);
      return;
    }

    if (!detailsToggled && detailsExpanded) {
      const card = cardRef.current;
      const startHeight =
        lastObservedCardHeightRef.current ??
        card?.getBoundingClientRect().height ??
        null;
      pendingAutomaticCollapseHeightRef.current = startHeight;
      if (card && startHeight !== null) {
        // Freeze the last height Chrome actually painted, not the transient
        // success payload height from this commit. The nested layout update
        // can then replace the expanded panel before paint without moving the
        // bottom-pinned transcript up and back down.
        card.style.height = `${startHeight}px`;
        card.style.overflow = "hidden";
      }
      setDetailsExpanded(false);
    }
  }, [detailsExpanded, detailsToggled, message.status]);

  useLayoutEffect(() => {
    if (detailsExpanded) {
      if (pendingAutomaticCollapseHeightRef.current === null) {
        cardRef.current?.style.removeProperty("height");
        cardRef.current?.style.removeProperty("overflow");
      }
      return;
    }

    const card = cardRef.current;
    const startHeight = pendingAutomaticCollapseHeightRef.current;
    pendingAutomaticCollapseHeightRef.current = null;
    if (!card || startHeight === null) {
      return;
    }

    const scrollContainer = card.closest<HTMLElement>(".message-stack");
    const previousScrollTop = scrollContainer?.scrollTop ?? null;
    card.style.height = "";
    card.style.overflow = "";
    const endHeight = card.getBoundingClientRect().height;
    card.style.height = `${startHeight}px`;
    card.style.overflow = "hidden";
    if (scrollContainer && previousScrollTop !== null) {
      scrollContainer.scrollTop = previousScrollTop;
    }
    const reducedMotion =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (
      reducedMotion ||
      Math.abs(startHeight - endHeight) <= 1 ||
      typeof card.animate !== "function"
    ) {
      card.style.height = "";
      card.style.overflow = "";
      return;
    }

    automaticCollapseAnimationRef.current?.cancel();
    const animation = card.animate(
      [
        { height: `${startHeight}px`, overflow: "hidden" },
        { height: `${endHeight}px`, overflow: "hidden" },
      ],
      {
        duration: 220,
        easing: "cubic-bezier(0.4, 0, 0.2, 1)",
      },
    );
    automaticCollapseAnimationRef.current = animation;
    animation.addEventListener(
      "finish",
      () => {
        if (automaticCollapseAnimationRef.current === animation) {
          automaticCollapseAnimationRef.current = null;
          card.style.height = "";
          card.style.overflow = "";
        }
      },
      { once: true },
    );

    return () => {
      if (automaticCollapseAnimationRef.current === animation) {
        automaticCollapseAnimationRef.current = null;
        animation.cancel();
      }
      card.style.height = "";
      card.style.overflow = "";
    };
  }, [detailsExpanded]);

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

  function handleToggleDetails() {
    setDetailsToggled(true);
    setDetailsExpanded((open) => !open);
  }

  return (
    <div
      className={`command-card-entry-shell${entryExpanded ? "" : " is-entering"}`}
    >
      <div className="command-card-entry-shell-content">
        <article
          ref={cardRef}
          className="message-card utility-card command-card"
        >
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
          <div className="command-card-header">
            <div className="card-label command-card-label">Command</div>
            {canCollapseDetails && !isSearchExpanded ? (
              <button
                className="command-icon-button command-card-details-toggle"
                type="button"
                onClick={handleToggleDetails}
                aria-label={
                  isDetailsExpanded
                    ? "Hide command details"
                    : "Show command details"
                }
                aria-expanded={isDetailsExpanded}
                title={
                  isDetailsExpanded
                    ? "Hide command details"
                    : "Show command details"
                }
              >
                {isDetailsExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>

          {isDetailsExpanded ? (
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
                      copiedSection === "command"
                        ? "Command copied"
                        : "Copy command"
                    }
                    title={
                      copiedSection === "command" ? "Copied" : "Copy command"
                    }
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
                      title={
                        isInputExpanded ? "Collapse command" : "Expand command"
                      }
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
                      copiedSection === "output"
                        ? "Output copied"
                        : "Copy output"
                    }
                    title={
                      copiedSection === "output" ? "Copied" : "Copy output"
                    }
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
                      title={
                        isOutputExpanded ? "Collapse output" : "Expand output"
                      }
                    >
                      {isOutputExpanded ? <CollapseIcon /> : <ExpandIcon />}
                    </button>
                  ) : null}
                </div>
              </div>
            </div>
          ) : (
            <div className="command-success-summary">
              <code className="command-success-summary-command">
                {commandSummary}
              </code>
              <span className="command-success-summary-meta">
                {summaryMeta}
              </span>
            </div>
          )}
        </article>
      </div>
    </div>
  );
}

function firstSingleLine(value: string): string {
  const trimmed = value.trim();
  const [firstLine] = trimmed.split(/\r?\n/, 1);
  return firstLine || "Command";
}

function countVisibleLines(value: string): number {
  const trimmed = value.trimEnd();
  if (!trimmed) {
    return 0;
  }
  return trimmed.split(/\r?\n/).length;
}

function lineCountLabel(count: number, direction: "in" | "out"): string {
  return `${count} ${count === 1 ? "line" : "lines"} ${direction}`;
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
  actionsEnabled = true,
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ApprovalMessage;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  actionsEnabled?: boolean;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const decided = message.decision !== "pending";
  const chosen = (d: ApprovalDecision) =>
    message.decision === d ? " chosen" : "";
  const resolvedDecision =
    message.decision === "pending" ? null : message.decision;
  const supportsDecision = (decision: ApprovalDecision) =>
    message.supportedDecisions == null ||
    message.supportedDecisions.includes(decision);

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
        {supportsDecision("accepted") ? (
          <button
            className={`approval-button${chosen("accepted")}`}
            type="button"
            onClick={() => onApprovalDecision(message.id, "accepted")}
            disabled={decided || !actionsEnabled}
          >
            Approve
          </button>
        ) : null}
        {supportsDecision("acceptedForSession") ? (
          <button
            className={`approval-button${chosen("acceptedForSession")}`}
            type="button"
            onClick={() => onApprovalDecision(message.id, "acceptedForSession")}
            disabled={decided || !actionsEnabled}
          >
            Approve for session
          </button>
        ) : null}
        {supportsDecision("rejected") ? (
          <button
            className={`approval-button approval-button-reject${chosen("rejected")}`}
            type="button"
            onClick={() => onApprovalDecision(message.id, "rejected")}
            disabled={decided || !actionsEnabled}
          >
            Reject
          </button>
        ) : null}
      </div>
      {!decided && !actionsEnabled ? (
        <p className="support-copy" role="status">
          Resolve the earlier approval before responding to this request.
        </p>
      ) : null}
      {resolvedDecision ? (
        <p className="approval-result">
          Decision: {renderDecision(resolvedDecision)}
        </p>
      ) : null}
    </article>
  );
}
