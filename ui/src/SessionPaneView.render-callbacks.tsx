// SessionPaneView.render-callbacks.tsx
//
// Owns the stable render callbacks that SessionPaneView passes into
// AgentSessionPanel. The pane component keeps orchestration state; this hook
// owns only repeated card rendering closures.
//
// Split out of: ui/src/SessionPaneView.tsx.

import { useCallback } from "react";
import {
  CodexPromptSettingsCard,
  ClaudePromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
} from "./prompt-settings-cards";
import { CommandCard, DiffCard, MessageCard } from "./message-cards";
import type { OpenPathOptions } from "./api";
import type { MonacoAppearance } from "./monaco";
import type { RenderMessageCard } from "./panels/VirtualizedConversationMessageList";
import type { ConnectionRetryDisplayState } from "./connection-retry";
import type {
  CommandMessage,
  DiffMessage,
  Session,
  SessionSettingsField,
  SessionSettingsValue,
} from "./types";

type UseSessionRenderCallbacksParams = {
  activeSession: Session | null;
  activeSessionSearchMatchItemKey: string | undefined;
  editorAppearance: MonacoAppearance;
  getConnectionRetryDisplayState: (
    messageId: string,
  ) => ConnectionRetryDisplayState | undefined;
  isRefreshingModelOptions: boolean;
  latestAssistantMessageId: string | null;
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
  modelOptionsError,
  onArchiveCodexThread,
  onCompactCodexThread,
  onForkCodexThread,
  onOpenDiffPreviewTab,
  onOpenSourceTab,
  onRefreshSessionModelOptions,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  paneId,
  sessionFindQuery,
  sessionSettingNotice,
}: UseSessionRenderCallbacksParams) {
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
          activeSession?.status === "active" &&
          message.id === latestAssistantMessageId
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
      latestAssistantMessageId,
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
