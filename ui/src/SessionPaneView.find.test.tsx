import { act, fireEvent, render } from "@testing-library/react";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SessionPaneView } from "./SessionPaneView";
import type { Session } from "./types";
import type { WorkspacePane } from "./workspace";

vi.mock("./SessionPaneView.scroll", async () => {
  const React = await import("react");

  return {
    useSessionPaneScrollState: () => ({
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
    }),
  };
});

vi.mock("./panels/AgentSessionPanel", () => ({
  AgentSessionPanel: (props: { activeSessionId: string | null }) => (
    <div data-testid="agent-session-panel">{props.activeSessionId}</div>
  ),
  AgentSessionPanelFooter: () => <div data-testid="agent-session-footer" />,
}));

vi.mock("./panels/PaneTabs", () => ({
  PaneTabs: () => <div data-testid="pane-tabs" />,
}));

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
): ComponentProps<typeof SessionPaneView> {
  return {
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
}

describe("SessionPaneView find navigation", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("keeps session-find navigation on the stepped match", async () => {
    const session = makeSession({
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Find alpha in the first prompt.",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Find alpha in the assistant reply.",
        },
      ],
    });

    let rendered: ReturnType<typeof render> | null = null;
    await act(async () => {
      rendered = render(<SessionPaneView {...makeSessionPaneViewProps(session)} />);
    });

    fireEvent.click(rendered!.getByRole("button", { name: "Find" }));
    const input = rendered!.getByPlaceholderText("Find in session");
    await act(async () => {
      fireEvent.change(input, { target: { value: "alpha" } });
      await Promise.resolve();
    });
    expect(rendered!.getByText("1 of 2")).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(rendered!.getByRole("button", { name: "Next" }));
      await Promise.resolve();
    });
    expect(rendered!.getByText("2 of 2")).toBeInTheDocument();

    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
      await Promise.resolve();
    });
    expect(rendered!.getByText("1 of 2")).toBeInTheDocument();

    act(() => {
      rendered?.unmount();
    });
  });
});
