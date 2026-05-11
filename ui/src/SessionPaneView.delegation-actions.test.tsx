import type { ComponentProps } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SessionPaneView } from "./SessionPaneView";
import type {
  ParallelAgentsMessage,
  Project,
  RemoteConfig,
  Session,
} from "./types";
import type { WorkspacePane } from "./workspace";

const delegationActionMockState = vi.hoisted(() => ({
  autoOpenDelegationOnLayout: false,
}));

const delegationCommandMocks = vi.hoisted(() => ({
  cancelDelegationCommand: vi.fn(),
  getDelegationResultCommand: vi.fn(),
  getDelegationStatusCommand: vi.fn(),
}));

vi.mock("./panels/AgentSessionPanel", async () => {
  const React = await import("react");

  const delegationMessage: ParallelAgentsMessage = {
    id: "delegation-message",
    type: "parallelAgents",
    timestamp: "10:00",
    author: "assistant",
    agents: [
      {
        id: "delegation-running",
        source: "delegation",
        status: "running",
        title: "Delegated reviewer",
      },
      {
        id: "delegation-completed",
        source: "delegation",
        status: "completed",
        title: "Completed reviewer",
      },
    ],
  };

  const noop = () => undefined;

  function AgentSessionPanel(props: {
    renderMessageCard: (...args: unknown[]) => React.ReactElement;
  }) {
    const card = props.renderMessageCard(
      delegationMessage,
      false,
      noop,
      noop,
      noop,
      noop,
    );
    const cardProps = React.isValidElement(card)
      ? (card.props as {
          onOpenParallelAgentSession?: unknown;
          onInsertParallelAgentResult?: unknown;
          onCancelParallelAgent?: unknown;
        })
      : {};

    React.useLayoutEffect(() => {
      if (
        delegationActionMockState.autoOpenDelegationOnLayout &&
        typeof cardProps.onOpenParallelAgentSession === "function"
      ) {
        delegationActionMockState.autoOpenDelegationOnLayout = false;
        void (
          cardProps.onOpenParallelAgentSession as (
            delegationId: string,
          ) => Promise<void>
        )("delegation-running");
      }
    }, [cardProps.onOpenParallelAgentSession]);

    return (
      <div data-testid="agent-session-panel">
        {typeof cardProps.onOpenParallelAgentSession === "function" ? (
          <button type="button">Open delegation child</button>
        ) : null}
        {typeof cardProps.onInsertParallelAgentResult === "function" ? (
          <button type="button">Insert delegation result</button>
        ) : null}
        {typeof cardProps.onCancelParallelAgent === "function" ? (
          <button type="button">Cancel delegation</button>
        ) : null}
      </div>
    );
  }

  function AgentSessionPanelFooter(props: {
    activeSessionId: string | null;
    canSpawnDelegation: boolean;
    onSpawnDelegation?: (sessionId: string, prompt: string) => Promise<boolean>;
  }) {
    return (
      <div data-testid="agent-session-panel-footer">
        {props.canSpawnDelegation && props.onSpawnDelegation ? (
          <button
            type="button"
            onClick={() => {
              void props.onSpawnDelegation?.(
                props.activeSessionId ?? "",
                "Delegate this review.",
              );
            }}
          >
            Delegate
          </button>
        ) : null}
      </div>
    );
  }

  return {
    AgentSessionPanel,
    AgentSessionPanelFooter,
  };
});

vi.mock("./delegation-commands", () => delegationCommandMocks);

vi.mock("./panels/PaneTabs", () => ({
  PaneTabs: () => <div data-testid="pane-tabs" />,
}));

function makeProject(overrides: Partial<Project> = {}): Project {
  return {
    id: "project-local",
    name: "Local Project",
    rootPath: "C:/repo",
    remoteId: null,
    ...overrides,
  };
}

function makeRemote(overrides: Partial<RemoteConfig> = {}): RemoteConfig {
  return {
    id: "local",
    name: "Local",
    transport: "local",
    enabled: true,
    host: null,
    port: null,
    user: null,
    ...overrides,
  };
}

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "Session One",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/repo",
    projectId: "project-local",
    model: "gpt-5",
    status: "idle",
    preview: "Ready",
    messagesLoaded: true,
    pendingPrompts: [],
    messages: [],
    ...overrides,
  };
}

function makePane(sessionId: string): WorkspacePane {
  return {
    id: "pane-1",
    tabs: [{ id: `tab-${sessionId}`, kind: "session", sessionId }],
    activeTabId: `tab-${sessionId}`,
    activeSessionId: sessionId,
    viewMode: "session",
    lastSessionViewMode: "session",
    sourcePath: null,
  };
}

function renderSessionPaneView({
  session,
  projects = [],
  remotes = [],
}: {
  session: Session;
  projects?: Project[];
  remotes?: RemoteConfig[];
}) {
  const onOpenConversationFromDiff = vi.fn();
  const props: ComponentProps<typeof SessionPaneView> = {
    pane: makePane(session.id),
    codexState: {},
    projectLookup: new Map(projects.map((project) => [project.id, project])),
    remoteLookup: new Map(remotes.map((remote) => [remote.id, remote])),
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
    onOpenConversationFromDiff,
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

  return {
    ...render(<SessionPaneView {...props} />),
    onOpenConversationFromDiff,
  };
}

function expectDelegationActions(expectedEnabled: boolean) {
  const assertions = [
    "Open delegation child",
    "Insert delegation result",
    "Cancel delegation",
    "Delegate",
  ];

  for (const name of assertions) {
    if (expectedEnabled) {
      expect(screen.getByRole("button", { name })).toBeInTheDocument();
    } else {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  }
}

describe("SessionPaneView delegation action wiring", () => {
  afterEach(() => {
    delegationActionMockState.autoOpenDelegationOnLayout = false;
    delegationCommandMocks.cancelDelegationCommand.mockReset();
    delegationCommandMocks.getDelegationResultCommand.mockReset();
    delegationCommandMocks.getDelegationStatusCommand.mockReset();
  });

  it.each([
    {
      name: "local project session",
      session: makeSession({ projectId: "project-local" }),
      projects: [makeProject({ id: "project-local", remoteId: null })],
      expectedEnabled: true,
    },
    {
      name: "remote project session",
      session: makeSession({ projectId: "project-remote" }),
      projects: [makeProject({ id: "project-remote", remoteId: "ssh-lab" })],
      expectedEnabled: false,
    },
    {
      name: "missing project session without embedded remote ownership",
      session: makeSession({ projectId: "project-missing" }),
      projects: [],
      expectedEnabled: true,
    },
    {
      name: "projectless remote proxy session",
      session: makeSession({ projectId: null, remoteId: "ssh-lab" }),
      projects: [],
      expectedEnabled: false,
    },
    {
      name: "remote-owned session on a local project",
      session: makeSession({
        projectId: "project-local",
        remoteId: "ssh-lab",
      }),
      projects: [makeProject({ id: "project-local", remoteId: null })],
      expectedEnabled: false,
    },
  ])(
    "gates delegation actions for $name",
    ({ session, projects, expectedEnabled }) => {
      renderSessionPaneView({
        session,
        projects,
        remotes: [
          makeRemote(),
          makeRemote({
            id: "ssh-lab",
            name: "SSH Lab",
            transport: "ssh",
            host: "lab.example.test",
          }),
        ],
      });

      expectDelegationActions(expectedEnabled);
    },
  );

  it("keeps the first delegated action valid before passive session effects run", async () => {
    let resolveStatus!: (value: {
      childSessionId: string;
      delegationId: string;
      delegation: Record<string, unknown>;
      revision: number;
      serverInstanceId: string;
      status: string;
    }) => void;
    delegationCommandMocks.getDelegationStatusCommand.mockReturnValue(
      new Promise((resolve) => {
        resolveStatus = resolve;
      }),
    );
    delegationActionMockState.autoOpenDelegationOnLayout = true;

    const { onOpenConversationFromDiff } = renderSessionPaneView({
      session: makeSession({ id: "session-1" }),
      projects: [makeProject({ id: "project-local", remoteId: null })],
    });

    await waitFor(() => {
      expect(
        delegationCommandMocks.getDelegationStatusCommand,
      ).toHaveBeenCalledWith("session-1", "delegation-running");
    });
    resolveStatus({
      childSessionId: "child-session-1",
      delegationId: "delegation-running",
      delegation: {},
      revision: 2,
      serverInstanceId: "server-1",
      status: "running",
    });

    await waitFor(() => {
      expect(onOpenConversationFromDiff).toHaveBeenCalledWith(
        "child-session-1",
        "pane-1",
      );
    });
  });
});
