import { act, render, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SessionPaneView } from "./SessionPaneView";
import {
  resetSessionStoreForTesting,
  upsertSessionStoreSession,
} from "./session-store";
import type { Session } from "./types";
import type { WorkspacePane } from "./workspace";

const scrollMockState = vi.hoisted(() => ({
  params: [] as Array<{
    activeSession: Session | null;
    deferContentScrollEffects: boolean;
    visibleContentSignature: string;
    visibleLastMessageAuthor: string | undefined;
  }>,
}));

vi.mock("./SessionPaneView.scroll", async () => {
  const React = await import("react");

  return {
    useSessionPaneScrollState: (params: {
      activeSession: Session | null;
      deferContentScrollEffects: boolean;
      visibleContentSignature: string;
      visibleLastMessageAuthor: string | undefined;
    }) => {
      scrollMockState.params.push(params);
      return {
        handleConversationSearchItemMount: vi.fn(),
        handleMessageStackScroll: vi.fn(),
        handleMessageStackTouchStart: vi.fn(),
        handleMessageStackUserScrollIntent: vi.fn(),
        liveTailPinned: true,
        messageStackRef: React.createRef<HTMLElement>(),
        newResponseIndicatorLabel: "New response",
        scrollMessageStackByPage: vi.fn(),
        scrollMessageStackToBoundary: vi.fn(),
        scrollSessionMessageStackByPageJump: vi.fn(),
        showNewResponseIndicator: false,
      };
    },
  };
});

vi.mock("./panels/AgentSessionPanel", async () => {
  const { useSessionRecordSnapshot } = await import("./session-store");

  return {
    AgentSessionPanel: (props: { activeSessionId: string | null }) => {
      const activeSession = useSessionRecordSnapshot(props.activeSessionId);
      return (
        <div data-testid="agent-session-panel">
          <div>{props.activeSessionId}</div>
          {activeSession?.messages.map((message) =>
            message.type === "text" ? (
              <div key={message.id}>{message.text}</div>
            ) : null,
          )}
        </div>
      );
    },
    AgentSessionPanelFooter: () => <div data-testid="agent-session-footer" />,
  };
});

vi.mock("./panels/PaneTabs", () => ({
  PaneTabs: () => <div data-testid="pane-tabs" />,
}));

function latestScrollParams() {
  return scrollMockState.params[scrollMockState.params.length - 1];
}

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "Session One",
    emoji: "AI",
    agent: "Codex",
    workdir: "/repo",
    projectId: null,
    model: "gpt-5",
    status: "active",
    preview: "Working",
    messagesLoaded: true,
    pendingPrompts: [],
    messages: [
      {
        id: "message-user-1",
        type: "text",
        timestamp: "10:00",
        author: "you",
        text: "Build it.",
      },
    ],
    ...overrides,
  };
}

function makePane(sessionId: string): WorkspacePane {
  return {
    id: "pane-1",
    tabs: [{ id: "tab-session-1", kind: "session", sessionId }],
    activeTabId: "tab-session-1",
    activeSessionId: sessionId,
    viewMode: "session",
    lastSessionViewMode: "session",
    sourcePath: null,
  };
}

function makeSessionPaneViewProps(
  session: Session,
  overrides: Partial<ComponentProps<typeof SessionPaneView>> = {},
): ComponentProps<typeof SessionPaneView> {
  const props: ComponentProps<typeof SessionPaneView> = {
    pane: makePane(session.id),
    codexState: {},
    projectLookup: new Map(),
    remoteLookup: new Map(),
    delegationWaits: [],
    sessionLookup: new Map([[session.id, session]]),
    isActive: true,
    isLoading: false,
    isSending: false,
    isStopping: false,
    isKilling: false,
    isUpdating: false,
    isRefreshingModelOptions: false,
    modelOptionsError: null,
    agentCommands: [],
    hasLoadedAgentCommands: true,
    isRefreshingAgentCommands: false,
    agentCommandsError: null,
    sessionSettingNotice: null,
    paneShouldStickToBottomRef: { current: {} },
    paneScrollPositionsRef: { current: {} },
    paneContentSignaturesRef: { current: {} },
    paneMessageContentSignaturesRef: { current: {} },
    forceSessionScrollToBottomRef: { current: {} },
    pendingScrollToBottomRequest: null,
    windowId: "window-1",
    draggedTab: null,
    getKnownDraggedTab: () => null,
    editorAppearance: "light",
    editorFontSizePx: 13,
    onActivatePane: vi.fn(),
    onSelectTab: vi.fn(),
    onCloseTab: vi.fn(),
    onSplitPane: vi.fn(),
    onTabDragStart: vi.fn(),
    onTabDragEnd: vi.fn(),
    onTabDrop: vi.fn(),
    onPaneViewModeChange: vi.fn(),
    onOpenSourceTab: vi.fn(),
    onOpenDiffPreviewTab: vi.fn(),
    onOpenGitStatusDiffPreviewTab: vi.fn(),
    onOpenFilesystemTab: vi.fn(),
    onOpenGitStatusTab: vi.fn(),
    onOpenTerminalTab: vi.fn(),
    onOpenInstructionDebuggerTab: vi.fn(),
    onOpenCanvasTab: vi.fn(),
    onUpsertCanvasSessionCard: vi.fn(),
    onRemoveCanvasSessionCard: vi.fn(),
    onSetCanvasZoom: vi.fn(),
    onPaneSourcePathChange: vi.fn(),
    onOpenConversationFromDiff: vi.fn(),
    onInsertReviewIntoPrompt: vi.fn(),
    onDraftCommit: vi.fn(),
    onDraftAttachmentsAdd: vi.fn(),
    onDraftAttachmentRemove: vi.fn(),
    onComposerError: vi.fn(),
    onSend: vi.fn(() => true),
    onCancelQueuedPrompt: vi.fn(),
    onApprovalDecision: vi.fn(),
    onUserInputSubmit: vi.fn(),
    onMcpElicitationSubmit: vi.fn(),
    onCodexAppRequestSubmit: vi.fn(),
    onStopSession: vi.fn(),
    onKillSession: vi.fn(),
    onRenameSessionRequest: vi.fn(),
    onScrollToBottomRequestHandled: vi.fn(),
    onSessionSettingsChange: vi.fn(),
    onArchiveCodexThread: vi.fn(),
    onCompactCodexThread: vi.fn(),
    onForkCodexThread: vi.fn(),
    onRefreshSessionModelOptions: vi.fn(),
    onRefreshAgentCommands: vi.fn(),
    onCreateConversationMarker: vi.fn(),
    onDeleteConversationMarker: vi.fn(),
    onRollbackCodexThread: vi.fn(),
    onUnarchiveCodexThread: vi.fn(),
    onOrchestratorStateUpdated: vi.fn(),
    renderControlPanel: () => <div />,
    renderControlPanelPaneBarStatus: () => null,
    renderControlPanelPaneBarActions: () => <div />,
    workspaceFilesChangedEvent: null,
    backendConnectionState: "connected",
  };

  return { ...props, ...overrides };
}

function renderSessionPaneView(
  session: Session,
  overrides: Partial<ComponentProps<typeof SessionPaneView>> = {},
) {
  return render(
    <SessionPaneView {...makeSessionPaneViewProps(session, overrides)} />,
  );
}

describe("SessionPaneView session-store synchronization", () => {
  afterEach(() => {
    resetSessionStoreForTesting();
    scrollMockState.params = [];
    vi.clearAllMocks();
  });

  it("uses the store-backed session for scroll signatures before broad session props update", async () => {
    const staleSession = makeSession();
    const freshSession = makeSession({
      preview: "Assistant replied.",
      messages: [
        ...staleSession.messages,
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Done.",
        },
      ],
    });
    upsertSessionStoreSession({
      session: staleSession,
      committedDraft: "",
      draftAttachments: [],
    });

    let rendered: ReturnType<typeof renderSessionPaneView> | null = null;
    await act(async () => {
      rendered = renderSessionPaneView(staleSession);
    });
    expect(latestScrollParams()?.activeSession?.messages).toHaveLength(1);
    expect(latestScrollParams()?.deferContentScrollEffects).toBe(false);

    await act(async () => {
      upsertSessionStoreSession({
        session: freshSession,
        committedDraft: "",
        draftAttachments: [],
      });
      await Promise.resolve();
    });

    await waitFor(() => {
      const latestParams = latestScrollParams();
      expect(latestParams?.activeSession).toBe(freshSession);
      expect(latestParams?.deferContentScrollEffects).toBe(true);
      expect(latestParams?.activeSession?.messages).toHaveLength(2);
      expect(latestParams?.visibleLastMessageAuthor).toBe("assistant");
      expect(latestParams?.visibleContentSignature).toContain(
        "message-assistant-1",
      );
      expect(rendered!.getByText("Done.")).toBeInTheDocument();
    });

    await act(async () => {
      rendered!.rerender(
        <SessionPaneView {...makeSessionPaneViewProps(freshSession)} />,
      );
      await Promise.resolve();
    });
    await waitFor(() => {
      const latestParams = latestScrollParams();
      expect(latestParams?.activeSession).toBe(freshSession);
      expect(latestParams?.deferContentScrollEffects).toBe(false);
    });

    act(() => {
      rendered?.unmount();
    });
  });

  it("uses the fallback session tab when the pane active session is missing from props", async () => {
    const missingSession = makeSession({
      id: "session-missing",
      messages: [
        {
          id: "message-missing",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Missing session should not become active.",
        },
      ],
    });
    const fallbackSession = makeSession({
      id: "session-fallback",
      messages: [
        {
          id: "message-fallback",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Fallback session is active.",
        },
      ],
    });
    const pane: WorkspacePane = {
      id: "pane-1",
      tabs: [
        { id: "tab-missing", kind: "session", sessionId: missingSession.id },
        { id: "tab-fallback", kind: "session", sessionId: fallbackSession.id },
      ],
      activeTabId: "tab-missing",
      activeSessionId: missingSession.id,
      viewMode: "session",
      lastSessionViewMode: "session",
      sourcePath: null,
    };
    upsertSessionStoreSession({
      session: missingSession,
      committedDraft: "",
      draftAttachments: [],
    });
    upsertSessionStoreSession({
      session: fallbackSession,
      committedDraft: "",
      draftAttachments: [],
    });

    let rendered: ReturnType<typeof renderSessionPaneView> | null = null;
    await act(async () => {
      rendered = renderSessionPaneView(fallbackSession, {
        pane,
        sessionLookup: new Map([[fallbackSession.id, fallbackSession]]),
      });
    });

    await waitFor(() => {
      const latestParams = latestScrollParams();
      expect(latestParams?.activeSession).toBe(fallbackSession);
      expect(latestParams?.deferContentScrollEffects).toBe(false);
      expect(rendered!.getByText("Fallback session is active.")).toBeInTheDocument();
    });

    act(() => {
      rendered?.unmount();
    });
  });
});
