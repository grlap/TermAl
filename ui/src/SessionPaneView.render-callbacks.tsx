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
  const mountedRef = useRef(false);
  const activeSessionIdRef = useRef<string | null>(activeSession?.id ?? null);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  useEffect(() => {
    activeSessionIdRef.current = activeSession?.id ?? null;
  }, [activeSession?.id]);
  const canApplyDelegationActionResult = useCallback(
    (parentSessionId: string) =>
      mountedRef.current && activeSessionIdRef.current === parentSessionId,
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
      return (async () => {
        try {
          const response = await getDelegationStatusCommand(
            parentSessionId,
            delegationId,
          );
          if (!canApplyDelegationActionResult(parentSessionId)) {
            return;
          }
          onOpenConversationFromDiff(response.childSessionId, paneId);
        } catch (error) {
          if (canApplyDelegationActionResult(parentSessionId)) {
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
      return (async () => {
        try {
          const result = await getDelegationResultCommand(
            parentSessionId,
            delegationId,
          );
          if (!canApplyDelegationActionResult(parentSessionId)) {
            return;
          }
          onInsertReviewIntoPrompt(
            parentSessionId,
            paneId,
            formatDelegationResultPrompt(result),
          );
        } catch (error) {
          if (canApplyDelegationActionResult(parentSessionId)) {
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
      return (async () => {
        try {
          await cancelDelegationCommand(parentSessionId, delegationId);
        } catch (error) {
          if (canApplyDelegationActionResult(parentSessionId)) {
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
        onOpenParallelAgentSession={handleOpenParallelAgentSession}
        onInsertParallelAgentResult={handleInsertParallelAgentResult}
        onCancelParallelAgent={handleCancelParallelAgent}
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
