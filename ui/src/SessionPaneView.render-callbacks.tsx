// SessionPaneView.render-callbacks.tsx
//
// Owns the stable render/action callbacks that SessionPaneView passes into
// AgentSessionPanel. The pane component keeps orchestration state; this hook
// owns repeated card rendering closures and the UI-side delegation card actions
// they expose.
//
// Split out of: ui/src/SessionPaneView.tsx.

import { useCallback, useEffect, useRef } from "react";
import { type OpenPathOptions } from "./api";
import { getErrorMessage } from "./app-utils";
import {
  cancelDelegationCommand,
  getDelegationResultCommand,
  getDelegationStatusCommand,
} from "./delegation-commands";
import {
  formatDelegationResultPrompt,
} from "./delegation-result-prompt";
import {
  CodexPromptSettingsCard,
  ClaudePromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
} from "./prompt-settings-cards";
import { CommandCard, DiffCard, MessageCard } from "./message-cards";
import type { MonacoAppearance } from "./monaco";
import type { RenderMessageCard } from "./panels/VirtualizedConversationMessageList";
import type { ConnectionRetryDisplayState } from "./connection-retry";
import type {
  CommandMessage,
  DelegationStatus,
  DiffMessage,
  Message,
  Session,
  SessionSettingsField,
  SessionSettingsValue,
} from "./types";

export function streamingAssistantTextMessageIdForSession(
  session: Session | null,
) {
  if (session?.status !== "active") {
    return null;
  }
  const latestMessage = session.messages[session.messages.length - 1];
  if (
    latestMessage &&
    latestMessage.author === "assistant" &&
    latestMessage.type === "text"
  ) {
    return latestMessage.id;
  }
  return null;
}

export function shouldPreferStreamingAssistantTextRender(
  message: Message,
  streamingAssistantTextMessageId: string | null,
) {
  return message.id === streamingAssistantTextMessageId;
}

function delegationStatusFallbackLabel(status: string) {
  const trimmed = status.trim();
  return trimmed ? `unrecognized status "${trimmed}"` : "unrecognized status";
}

function cancelDelegationTerminalErrorMessage(
  status: DelegationStatus | string,
) {
  // Cancel returns the server's latest delegation status. Failed means the
  // cancel was a no-op against an errored delegation; completed/canceled are
  // idempotent terminal no-ops, and queued/running mean the request was
  // accepted or is still being reflected by follow-up SSE updates.
  switch (status) {
    case "failed":
      return "Delegation cannot be canceled because it has already failed.";
    case "completed":
    case "canceled":
    case "queued":
    case "running":
      return null;
    default:
      return `Delegation cancel returned ${delegationStatusFallbackLabel(
        status,
      )}. Refresh the session before retrying.`;
  }
}

function delegationChildUnavailableStatusLabel(
  status: DelegationStatus | string,
) {
  switch (status) {
    case "completed":
      return "already completed";
    case "failed":
      return "already failed";
    case "canceled":
      return "already canceled";
    case "queued":
      return "still queued";
    case "running":
      return "still running";
    default:
      return delegationStatusFallbackLabel(status);
  }
}

function insertedDelegationResultStatusMessage(status: DelegationStatus) {
  if (status === "completed") {
    return null;
  }
  return [
    `Delegation result status is ${status};`,
    "inserted output may describe an unsuccessful run.",
  ].join(" ");
}

type UseSessionRenderCallbacksParams = {
  activeSession: Session | null;
  activeSessionSearchMatchItemKey: string | undefined;
  editorAppearance: MonacoAppearance;
  getConnectionRetryDisplayState: (
    messageId: string,
  ) => ConnectionRetryDisplayState | undefined;
  isRefreshingModelOptions: boolean;
  latestAssistantMessageId: string | null;
  streamingAssistantTextMessageId: string | null;
  modelOptionsError: string | null;
  // Local delegation affordances are intentionally all-or-nothing: Open,
  // Insert, and Cancel share the same local-runtime routing gate.
  enableLocalDelegationActions: boolean;
  onArchiveCodexThread: (sessionId: string) => void;
  onCompactCodexThread: (sessionId: string) => void;
  onForkCodexThread: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenSourceTab: (
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: OpenPathOptions,
  ) => void;
  onOpenConversationFromDiff: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => void;
  onInsertReviewIntoPrompt: (
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) => void;
  onComposerError: (message: string | null) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRollbackCodexThread: (sessionId: string, numTurns: number) => void;
  onUnarchiveCodexThread: (sessionId: string) => void;
  paneId: string;
  sessionFindQuery: string;
  sessionSettingNotice: string | null;
};

export function useSessionRenderCallbacks({
  activeSession,
  activeSessionSearchMatchItemKey,
  editorAppearance,
  getConnectionRetryDisplayState,
  isRefreshingModelOptions,
  latestAssistantMessageId,
  streamingAssistantTextMessageId,
  modelOptionsError,
  enableLocalDelegationActions,
  onArchiveCodexThread,
  onCompactCodexThread,
  onForkCodexThread,
  onOpenDiffPreviewTab,
  onOpenSourceTab,
  onOpenConversationFromDiff,
  onInsertReviewIntoPrompt,
  onComposerError,
  onRefreshSessionModelOptions,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  paneId,
  sessionFindQuery,
  sessionSettingNotice,
}: UseSessionRenderCallbacksParams) {
  const activeSessionId = activeSession?.id ?? null;
  const mountedRef = useRef(true);
  const activeSessionIdRef = useRef<string | null>(null);
  const activeSessionGenerationRef = useRef(0);
  if (activeSessionIdRef.current !== activeSessionId) {
    activeSessionIdRef.current = activeSessionId;
    activeSessionGenerationRef.current += 1;
  }
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  // Stable by design: deferred delegation actions must read the latest refs
  // when they settle, not close over the session that rendered the card.
  const canApplyDelegationActionResult = useCallback(
    (parentSessionId: string, operationGeneration: number) =>
      mountedRef.current &&
      activeSessionIdRef.current === parentSessionId &&
      activeSessionGenerationRef.current === operationGeneration,
    [],
  );
  const renderSessionCommandCard = useCallback(
    (message: CommandMessage) => <CommandCard message={message} />,
    [],
  );
  const renderSessionDiffCard = useCallback(
    (message: DiffMessage) => (
      <DiffCard
        message={message}
        onOpenPreview={() =>
          onOpenDiffPreviewTab(
            paneId,
            message,
            activeSession?.id ?? null,
            activeSession?.projectId ?? null,
          )
        }
        workspaceRoot={activeSession?.workdir ?? null}
      />
    ),
    [
      activeSession?.id,
      activeSession?.projectId,
      activeSession?.workdir,
      onOpenDiffPreviewTab,
      paneId,
    ],
  );
  const handleOpenParallelAgentSession = useCallback(
    (delegationId: string) => {
      const parentSessionId = activeSession?.id;
      if (!parentSessionId) {
        return Promise.resolve();
      }
      const operationGeneration = activeSessionGenerationRef.current;
      return (async () => {
        try {
          const response = await getDelegationStatusCommand(
            parentSessionId,
            delegationId,
          );
          if (
            !canApplyDelegationActionResult(
              parentSessionId,
              operationGeneration,
            )
          ) {
            return;
          }
          const childSessionId =
            typeof response.childSessionId === "string"
              ? response.childSessionId.trim()
              : "";
          if (!childSessionId) {
            onComposerError(
              `Delegation child session is unavailable (${delegationChildUnavailableStatusLabel(response.status)}).`,
            );
            return;
          }
          onOpenConversationFromDiff(childSessionId, paneId);
        } catch (error) {
          if (
            canApplyDelegationActionResult(
              parentSessionId,
              operationGeneration,
            )
          ) {
            onComposerError(getErrorMessage(error));
          }
        }
      })();
    },
    [
      activeSession?.id,
      canApplyDelegationActionResult,
      onComposerError,
      onOpenConversationFromDiff,
      paneId,
    ],
  );
  const handleInsertParallelAgentResult = useCallback(
    (delegationId: string) => {
      const parentSessionId = activeSession?.id;
      if (!parentSessionId) {
        return Promise.resolve();
      }
      const operationGeneration = activeSessionGenerationRef.current;
      return (async () => {
        try {
          const result = await getDelegationResultCommand(
            parentSessionId,
            delegationId,
          );
          if (
            !canApplyDelegationActionResult(
              parentSessionId,
              operationGeneration,
            )
          ) {
            return;
          }
          const statusMessage = insertedDelegationResultStatusMessage(
            result.status,
          );
          if (statusMessage) {
            onComposerError(statusMessage);
          }
          onInsertReviewIntoPrompt(
            parentSessionId,
            paneId,
            formatDelegationResultPrompt(result),
          );
        } catch (error) {
          if (
            canApplyDelegationActionResult(
              parentSessionId,
              operationGeneration,
            )
          ) {
            onComposerError(getErrorMessage(error));
          }
        }
      })();
    },
    [
      activeSession?.id,
      canApplyDelegationActionResult,
      onComposerError,
      onInsertReviewIntoPrompt,
      paneId,
    ],
  );
  const handleCancelParallelAgent = useCallback(
    (delegationId: string) => {
      const parentSessionId = activeSession?.id;
      if (!parentSessionId) {
        return Promise.resolve();
      }
      const operationGeneration = activeSessionGenerationRef.current;
      return (async () => {
        try {
          const response = await cancelDelegationCommand(
            parentSessionId,
            delegationId,
          );
          if (
            !canApplyDelegationActionResult(
              parentSessionId,
              operationGeneration,
            )
          ) {
            return;
          }
          const terminalErrorMessage = cancelDelegationTerminalErrorMessage(
            response.status,
          );
          if (terminalErrorMessage !== null) {
            onComposerError(terminalErrorMessage);
          } else {
            onComposerError(null);
          }
        } catch (error) {
          if (
            canApplyDelegationActionResult(
              parentSessionId,
              operationGeneration,
            )
          ) {
            onComposerError(getErrorMessage(error));
          }
        }
      })();
    },
    [activeSession?.id, canApplyDelegationActionResult, onComposerError],
  );
  const renderSessionMessageCard = useCallback<RenderMessageCard>(
    (
      message,
      preferImmediateHeavyRender,
      handleDecision,
      handleUserInput,
      handleMcpElicitation,
      handleCodexAppRequest,
    ) => (
      <MessageCard
        appearance={editorAppearance}
        message={message}
        onOpenDiffPreview={(diffMessage) =>
          onOpenDiffPreviewTab(
            paneId,
            diffMessage,
            activeSession?.id ?? null,
            activeSession?.projectId ?? null,
          )
        }
        onOpenSourceLink={(target) =>
          onOpenSourceTab(
            paneId,
            target.path,
            activeSession?.id ?? null,
            activeSession?.projectId ?? null,
            {
              line: target.line,
              column: target.column,
              openInNewTab: target.openInNewTab,
            },
          )
        }
        preferImmediateHeavyRender={preferImmediateHeavyRender}
        onApprovalDecision={handleDecision}
        onUserInputSubmit={handleUserInput}
        onMcpElicitationSubmit={handleMcpElicitation}
        onCodexAppRequestSubmit={handleCodexAppRequest}
        onOpenParallelAgentSession={
          enableLocalDelegationActions
            ? handleOpenParallelAgentSession
            : undefined
        }
        onInsertParallelAgentResult={
          enableLocalDelegationActions
            ? handleInsertParallelAgentResult
            : undefined
        }
        onCancelParallelAgent={
          enableLocalDelegationActions ? handleCancelParallelAgent : undefined
        }
        searchQuery={
          activeSessionSearchMatchItemKey === `message:${message.id}`
            ? sessionFindQuery
            : ""
        }
        searchHighlightTone={
          activeSessionSearchMatchItemKey === `message:${message.id}`
            ? "active"
            : "match"
        }
        preferStreamingPlainTextRender={
          shouldPreferStreamingAssistantTextRender(
            message,
            streamingAssistantTextMessageId,
          )
        }
        isLatestAssistantMessage={message.id === latestAssistantMessageId}
        connectionRetryDisplayState={getConnectionRetryDisplayState(message.id)}
        workspaceRoot={activeSession?.workdir ?? null}
      />
    ),
    [
      activeSession?.id,
      activeSession?.projectId,
      activeSession?.status,
      activeSession?.workdir,
      activeSessionSearchMatchItemKey,
      editorAppearance,
      enableLocalDelegationActions,
      getConnectionRetryDisplayState,
      handleCancelParallelAgent,
      handleInsertParallelAgentResult,
      handleOpenParallelAgentSession,
      latestAssistantMessageId,
      streamingAssistantTextMessageId,
      onOpenDiffPreviewTab,
      onOpenSourceTab,
      paneId,
      sessionFindQuery,
    ],
  );
  // Keep one prompt-settings renderer for the four agent cards so the panel
  // dispatch stays simple. Codex-only deps can rebuild this callback for other
  // agents, but those inputs change only on explicit settings/thread actions.
  const renderSessionPromptSettings = useCallback(
    (
      panelPaneId: string,
      session: Session,
      panelIsUpdating: boolean,
      handleSettingsChange: (
        sessionId: string,
        field: SessionSettingsField,
        value: SessionSettingsValue,
      ) => void,
    ) => {
      if (session.agent === "Codex") {
        return (
          <CodexPromptSettingsCard
            paneId={panelPaneId}
            session={session}
            isUpdating={panelIsUpdating}
            isRefreshingModelOptions={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
            sessionNotice={
              session.id === activeSession?.id ? sessionSettingNotice : null
            }
            onRequestModelOptions={onRefreshSessionModelOptions}
            onArchiveThread={onArchiveCodexThread}
            onCompactThread={onCompactCodexThread}
            onForkThread={onForkCodexThread}
            onRollbackThread={onRollbackCodexThread}
            onSessionSettingsChange={handleSettingsChange}
            onUnarchiveThread={onUnarchiveCodexThread}
          />
        );
      }

      if (session.agent === "Claude") {
        return (
          <ClaudePromptSettingsCard
            paneId={panelPaneId}
            session={session}
            isUpdating={panelIsUpdating}
            isRefreshingModelOptions={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
            onRequestModelOptions={onRefreshSessionModelOptions}
            onSessionSettingsChange={handleSettingsChange}
          />
        );
      }

      if (session.agent === "Cursor") {
        return (
          <CursorPromptSettingsCard
            paneId={panelPaneId}
            session={session}
            isUpdating={panelIsUpdating}
            isRefreshingModelOptions={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
            onRequestModelOptions={onRefreshSessionModelOptions}
            onSessionSettingsChange={handleSettingsChange}
          />
        );
      }

      if (session.agent === "Gemini") {
        return (
          <GeminiPromptSettingsCard
            paneId={panelPaneId}
            session={session}
            isUpdating={panelIsUpdating}
            isRefreshingModelOptions={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
            onRequestModelOptions={onRefreshSessionModelOptions}
            onSessionSettingsChange={handleSettingsChange}
          />
        );
      }

      return null;
    },
    [
      activeSession?.id,
      isRefreshingModelOptions,
      modelOptionsError,
      onArchiveCodexThread,
      onCompactCodexThread,
      onForkCodexThread,
      onRefreshSessionModelOptions,
      onRollbackCodexThread,
      onUnarchiveCodexThread,
      sessionSettingNotice,
    ],
  );

  return {
    renderSessionCommandCard,
    renderSessionDiffCard,
    renderSessionMessageCard,
    renderSessionPromptSettings,
  };
}
