import type { ComponentProps } from "react";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CreateDelegationRequest } from "./api";
import {
  clickAndSettle,
  createActWrappedAnimationFrameMocks,
  withSuppressedActWarnings,
} from "./app-test-harness";
import { SessionPaneView } from "./SessionPaneView";
import {
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
} from "./session-store";
import type {
  AgentCommand,
  DelegationSummary,
  Project,
  RemoteConfig,
  Session,
} from "./types";
import type { WorkspacePane } from "./workspace";
import type {
  SpawnDelegationCommandResult,
} from "./delegation-commands";

const spawnDelegationCommandMock = vi.hoisted(() => vi.fn());
const resolveAgentCommandMock = vi.hoisted(() => vi.fn());

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();

  return {
    ...actual,
    resolveAgentCommand: resolveAgentCommandMock,
  };
});

vi.mock("./delegation-commands", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./delegation-commands")>();

  return {
    ...actual,
    spawnDelegationCommand: spawnDelegationCommandMock,
  };
});

vi.mock("./panels/AgentSessionPanel", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./panels/AgentSessionPanel")>();

  return {
    ...actual,
    AgentSessionPanel: () => <div data-testid="agent-session-panel" />,
  };
});

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

function makeDelegationSummary(
  overrides: Partial<DelegationSummary> = {},
): DelegationSummary {
  return {
    id: "delegation-1",
    parentSessionId: "session-1",
    childSessionId: "child-session-1",
    mode: "reviewer",
    status: "running",
    title: "Review this change",
    agent: "Codex",
    model: "gpt-5",
    writePolicy: { kind: "readOnly" },
    createdAt: "2026-05-08T10:00:00Z",
    startedAt: null,
    completedAt: null,
    result: null,
    ...overrides,
  };
}

function completedSpawnResult(): SpawnDelegationCommandResult {
  return {
    outcome: "completed",
    delegationId: "delegation-1",
    childSessionId: "child-session-1",
    delegation: makeDelegationSummary(),
    childSession: {
      id: "child-session-1",
      name: "Delegated reviewer",
      emoji: "AI",
      agent: "Codex",
      model: "gpt-5",
      status: "active",
      parentDelegationId: "delegation-1",
    },
    revision: 7,
    serverInstanceId: "server-1",
  };
}

function errorSpawnResult(message: string): SpawnDelegationCommandResult {
  return {
    outcome: "error",
    revision: null,
    serverInstanceId: null,
    error: {
      kind: "spawn-failed",
      name: "ApiRequestError",
      message,
      apiErrorKind: "request-failed",
      status: 409,
      restartRequired: null,
    },
  };
}

function renderSessionPaneView({
  session,
  draft,
  agentCommands = [],
  projects = [makeProject()],
  remotes = [makeRemote()],
  expectComposer = true,
}: {
  session: Session;
  draft: string;
  agentCommands?: AgentCommand[];
  projects?: Project[];
  remotes?: RemoteConfig[];
  expectComposer?: boolean;
}) {
  syncComposerSessionsStore({
    sessions: [session],
    draftsBySessionId: { [session.id]: draft },
    draftAttachmentsBySessionId: {},
  });

  const onDraftCommit = vi.fn();
  const onComposerError = vi.fn();
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
    agentCommands,
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
    onDraftCommit,
    onDraftAttachmentsAdd: vi.fn(),
    onDraftAttachmentRemove: vi.fn(),
    onComposerError,
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

  render(<SessionPaneView {...props} />);

  return {
    onComposerError,
    onDraftCommit,
    textarea: expectComposer
      ? (screen.getByLabelText(`Message ${session.name}`) as HTMLTextAreaElement)
      : (null as unknown as HTMLTextAreaElement),
  };
}

describe("SessionPaneView composer delegation click-through", () => {
  let animationFrameSpies: Array<{ mockRestore: () => void }> = [];

  beforeEach(() => {
    spawnDelegationCommandMock.mockReset();
    resolveAgentCommandMock.mockReset();
    resetSessionStoreForTesting();
    const {
      cancelAnimationFrameMock,
      requestAnimationFrameMock,
    } = createActWrappedAnimationFrameMocks();
    animationFrameSpies = [
      vi
        .spyOn(window, "requestAnimationFrame")
        .mockImplementation(requestAnimationFrameMock),
      vi
        .spyOn(window, "cancelAnimationFrame")
        .mockImplementation(cancelAnimationFrameMock),
    ];
  });

  afterEach(() => {
    cleanup();
    act(() => {
      resetSessionStoreForTesting();
    });
    animationFrameSpies.forEach((spy) => spy.mockRestore());
    animationFrameSpies = [];
  });

  it("spawns from the active parent session and clears the draft on success", async () => {
    const draft = "  Review\nthis change  ";
    const session = makeSession({ id: "session-parent" });
    spawnDelegationCommandMock.mockResolvedValue(completedSpawnResult());

    const { onComposerError, onDraftCommit, textarea } = renderSessionPaneView({
      session,
      draft,
    });

    expect(textarea.value).toBe(draft);
    await clickAndSettle(screen.getByRole("button", { name: "Delegate" }));

    await waitFor(() => {
      expect(spawnDelegationCommandMock).toHaveBeenCalledWith(
        "session-parent",
        {
          title: "Review this change",
          prompt: "Review\nthis change",
          agent: "Codex",
          model: "gpt-5",
          mode: "reviewer",
          writePolicy: { kind: "readOnly" },
        } satisfies CreateDelegationRequest,
      );
    });
    await waitFor(() => {
      expect(textarea.value).toBe("");
    });
    expect(onDraftCommit).toHaveBeenCalledWith("session-parent", "");
    expect(onComposerError).toHaveBeenCalledWith(null);
  });

  it("passes command-owned isolated worktree defaults through to spawn", async () => {
    const session = makeSession({ id: "session-parent" });
    const reviewLocalCommand: AgentCommand = {
      kind: "promptTemplate",
      name: "review-local",
      description: "Review staged and unstaged changes.",
      content: "Review local changes.",
      source: ".claude/commands/review-local.md",
    };
    resolveAgentCommandMock.mockResolvedValue({
      name: "review-local",
      source: ".claude/commands/review-local.md",
      kind: "promptTemplate",
      visiblePrompt: "/review-local",
      expandedPrompt: "Review local changes.",
      title: "Review staged and unstaged changes.",
      delegation: {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    });
    spawnDelegationCommandMock.mockResolvedValue(completedSpawnResult());

    const { onDraftCommit, textarea } = renderSessionPaneView({
      session,
      draft: "/rev",
      agentCommands: [reviewLocalCommand],
    });

    expect(textarea.value).toBe("/rev");
    expect(
      screen.getByRole("option", { name: /\/review-local/ }),
    ).toBeInTheDocument();

    await withSuppressedActWarnings(async () => {
      await clickAndSettle(screen.getByRole("button", { name: "Delegate" }));

      await waitFor(() => {
        expect(resolveAgentCommandMock).toHaveBeenCalledWith(
          "session-parent",
          "review-local",
          {
            arguments: "",
            intent: "delegate",
          },
        );
      });
      await waitFor(() => {
        expect(spawnDelegationCommandMock).toHaveBeenCalledWith(
          "session-parent",
          {
            title: "Review staged and unstaged changes.",
            prompt: "Review local changes.",
            agent: "Codex",
            model: "gpt-5",
            mode: "reviewer",
            writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
          } satisfies CreateDelegationRequest,
        );
      });
      expect(onDraftCommit).toHaveBeenCalledWith("session-parent", "");
    });
  });

  it("preserves the draft and surfaces composer errors when spawning fails", async () => {
    const draft = "Review the failure path.";
    const session = makeSession({ id: "session-parent" });
    spawnDelegationCommandMock.mockResolvedValue(
      errorSpawnResult("Delegation spawn failed."),
    );

    const { onComposerError, onDraftCommit, textarea } = renderSessionPaneView({
      session,
      draft,
    });

    await clickAndSettle(screen.getByRole("button", { name: "Delegate" }));

    await waitFor(() => {
      expect(spawnDelegationCommandMock).toHaveBeenCalledWith(
        "session-parent",
        expect.objectContaining({
          prompt: draft,
          mode: "reviewer",
          writePolicy: { kind: "readOnly" },
        }),
      );
    });
    await waitFor(() => {
      expect(onComposerError).toHaveBeenCalledWith(
        "Delegation spawn failed.",
      );
    });
    expect(textarea.value).toBe(draft);
    expect(onDraftCommit).not.toHaveBeenCalled();
  });

  it("preserves the draft and skips spawn when delegation availability fails", async () => {
    const draft = "Review after project load.";
    const session = makeSession({
      id: "session-parent",
      projectId: "project-missing",
    });

    const { onComposerError, onDraftCommit, textarea } = renderSessionPaneView({
      session,
      draft,
      projects: [],
    });

    await clickAndSettle(screen.getByRole("button", { name: "Delegate" }));

    expect(spawnDelegationCommandMock).not.toHaveBeenCalled();
    expect(onComposerError).toHaveBeenCalledWith(
      "Delegations are unavailable until the session project is loaded.",
    );
    expect(textarea.value).toBe(draft);
    expect(onDraftCommit).not.toHaveBeenCalled();
  });

  it("hides composer controls for delegated child sessions while keeping transcript find available", () => {
    const session = makeSession({
      id: "child-session-1",
      name: "Delegated reviewer",
      parentDelegationId: "delegation-1",
      messages: [
        {
          id: "message-1",
          type: "text",
          timestamp: "10:00:00",
          author: "assistant",
          text: "Review result",
        },
      ],
    });

    renderSessionPaneView({
      session,
      draft: "This should not be editable here.",
      expectComposer: false,
    });

    expect(screen.getByRole("button", { name: "Find" })).toBeInTheDocument();
    expect(screen.queryByLabelText(`Message ${session.name}`)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delegate" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Send" })).not.toBeInTheDocument();
  });
});
