import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import { useAppSessionActions } from "./app-session-actions";
import type { StateResponse } from "./api";
import type { Project, Session } from "./types";
import type { WorkspaceState } from "./workspace";

function makeSession(id: string, overrides: Partial<Session> = {}): Session {
  return {
    id,
    name: "Session",
    emoji: "S",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.4",
    status: "idle",
    preview: "Ready",
    messages: [],
    ...overrides,
  };
}

function makeStateResponse(revision: number): StateResponse {
  return {
    revision,
    serverInstanceId: "server-a",
    codex: {
      rateLimits: [],
      notices: [],
    },
    agentReadiness: [],
    preferences: {
      defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
    },
    projects: [],
    orchestrators: [],
    workspaces: [],
    sessions: [],
  } as StateResponse;
}

function makeWorkspace(): WorkspaceState {
  return {
    root: {
      type: "pane",
      paneId: "pane-1",
    },
    panes: [
      {
        id: "pane-1",
        tabs: [],
        activeTabId: null,
        activeSessionId: null,
        viewMode: "session",
        lastSessionViewMode: "session",
        sourcePath: null,
      },
    ],
    activePaneId: "pane-1",
  };
}

function makeSessionActionsParams(
  overrides: Partial<Parameters<typeof useAppSessionActions>[0]> = {},
): Parameters<typeof useAppSessionActions>[0] {
  const session = makeSession("session-1");
  const noopSetter = vi.fn();

  return {
    lookups: {
      sessionLookup: new Map([[session.id, session]]),
      projectLookup: new Map(),
      agentReadinessByAgent: new Map(),
      activeSession: session,
      workspace: makeWorkspace(),
    },
    newProjectRootPath: "",
    newProjectRemoteId: "local",
    newProjectUsesLocalRemote: true,
    defaults: {
      defaultCodexApprovalPolicy: "never",
      defaultCodexReasoningEffort: "medium",
      defaultCodexSandboxMode: "workspace-write",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
      defaultCursorMode: "agent",
      defaultGeminiApprovalMode: "default",
    },
    refs: {
      isMountedRef: { current: true },
      latestStateRevisionRef: { current: 5 },
      lastSeenServerInstanceIdRef: { current: "server-a" },
      sessionsRef: { current: [session] },
      projectsRef: { current: [] },
      draftsBySessionIdRef: { current: {} },
      draftAttachmentsBySessionIdRef: { current: {} },
      confirmedUnknownModelSendsRef: { current: new Set() },
      activePromptPollCancelRef: { current: null },
      activePromptPollSessionIdRef: { current: null },
      refreshingSessionModelOptionIdsRef: { current: {} },
      refreshingAgentCommandSessionIdsRef: { current: {} },
    },
    setters: {
      setSessions: noopSetter,
      setWorkspace: noopSetter,
      setRequestError: noopSetter,
      setIsCreating: noopSetter,
      setSendingSessionIds: noopSetter,
      setDraftsBySessionId: noopSetter,
      setDraftAttachmentsBySessionId: noopSetter,
      setIsCreatingProject: noopSetter,
      setNewProjectRootPath: noopSetter,
      setNewProjectRemoteId: noopSetter,
      setSelectedProjectId: noopSetter,
      setStoppingSessionIds: noopSetter,
      setKillingSessionIds: noopSetter,
      setUpdatingSessionIds: noopSetter,
      setSessionSettingNotices: noopSetter,
      setRefreshingSessionModelOptionIds: noopSetter,
      setSessionModelOptionErrors: noopSetter,
      setAgentCommandsBySessionId: noopSetter,
      setRefreshingAgentCommandSessionIds: noopSetter,
      setAgentCommandErrors: noopSetter,
    },
    adoptState: vi.fn(() => false),
    adoptCreatedSessionResponse: vi.fn(() => "adopted" as const),
    clearHydrationMismatchSessionIds: vi.fn(),
    applyControlPanelLayout: (workspace) => workspace,
    reportRequestError: vi.fn(),
    requestActionRecoveryResync: vi.fn(),
    forceSseReconnect: vi.fn(),
    ...overrides,
  };
}

describe("useAppSessionActions", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("clears the acted session hydration mismatch on stale same-instance action success", async () => {
    const state = {
      ...makeStateResponse(4),
      sessions: [makeSession("session-1", { sessionMutationStamp: 2 })],
    };
    vi.spyOn(api, "renameSession").mockResolvedValue(state);
    const params = makeSessionActionsParams();
    params.refs.sessionsRef.current = [
      makeSession("session-1", { sessionMutationStamp: 2 }),
    ];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleRenameSession("session-1", "Renamed"),
    ).resolves.toBe(true);

    expect(params.adoptState).toHaveBeenCalledWith(state, undefined);
    expect(params.clearHydrationMismatchSessionIds).toHaveBeenCalledWith([
      "session-1",
    ]);
    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
  });

  it("recovers instead of reporting stale same-instance success without target evidence", async () => {
    const state = {
      ...makeStateResponse(4),
      sessions: [makeSession("session-1", { sessionMutationStamp: 2 })],
    };
    vi.spyOn(api, "renameSession").mockResolvedValue(state);
    const params = makeSessionActionsParams();
    params.refs.sessionsRef.current = [
      makeSession("session-1", { sessionMutationStamp: 1 }),
    ];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleRenameSession("session-1", "Renamed"),
    ).resolves.toBe(false);

    expect(params.clearHydrationMismatchSessionIds).not.toHaveBeenCalled();
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
  });

  it("threads the acted session id into session-scoped action recovery", async () => {
    const state = {
      ...makeStateResponse(6),
      serverInstanceId: "server-b",
    };
    vi.spyOn(api, "renameSession").mockResolvedValue(state);
    const params = makeSessionActionsParams();
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleRenameSession("session-1", "Renamed"),
    ).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    // Cross-instance recovery must also recreate the EventSource so live
    // assistant deltas land on the new backend instead of the dead
    // pre-restart connection. Mirrors the `handleSend` mismatch branch;
    // see bugs.md "Cross-instance non-send action recovery does not
    // force SSE recreation".
    expect(params.forceSseReconnect).toHaveBeenCalled();
  });

  it("does not force an SSE recreate when same-instance action recovery rejects without server-instance change", async () => {
    // Stale same-instance rejection (the response carries the same
    // server instance the tab already adopted but lacks target
    // evidence): action recovery resync still fires to reconcile
    // metadata, but the EventSource is presumed healthy because the
    // backend did NOT restart. Forcing a recreate here would tear down
    // a live stream during routine same-instance staleness.
    const state = {
      ...makeStateResponse(4),
      sessions: [makeSession("session-1", { sessionMutationStamp: 2 })],
    };
    vi.spyOn(api, "renameSession").mockResolvedValue(state);
    const params = makeSessionActionsParams();
    params.refs.sessionsRef.current = [
      makeSession("session-1", { sessionMutationStamp: 1 }),
    ];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleRenameSession("session-1", "Renamed"),
    ).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
  });

  it("threads the originating pane into session-scoped action recovery", async () => {
    const state = {
      ...makeStateResponse(6),
      serverInstanceId: "server-b",
    };
    vi.spyOn(api, "renameSession").mockResolvedValue(state);
    const params = makeSessionActionsParams();
    params.lookups.workspace = {
      root: {
        id: "split-1",
        type: "split",
        direction: "row",
        ratio: 0.5,
        first: { type: "pane", paneId: "pane-current" },
        second: { type: "pane", paneId: "pane-origin" },
      },
      panes: [
        {
          id: "pane-current",
          tabs: [],
          activeTabId: null,
          activeSessionId: null,
          viewMode: "session",
          lastSessionViewMode: "session",
          sourcePath: null,
        },
        {
          id: "pane-origin",
          tabs: [
            {
              id: "tab-session-1",
              kind: "session",
              sessionId: "session-1",
            },
          ],
          activeTabId: "tab-session-1",
          activeSessionId: "session-1",
          viewMode: "session",
          lastSessionViewMode: "session",
          sourcePath: null,
        },
      ],
      activePaneId: "pane-current",
    };
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleRenameSession("session-1", "Renamed"),
    ).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: "pane-origin",
      allowUnknownServerInstance: true,
    });
  });

  it("keeps failed queued-prompt cancel refresh passive", async () => {
    const originalError = new Error("cancel failed");
    const staleState = {
      ...makeStateResponse(4),
      sessions: [makeSession("session-1", { sessionMutationStamp: 2 })],
    };
    vi.spyOn(api, "cancelQueuedPrompt").mockRejectedValue(originalError);
    vi.spyOn(api, "fetchState").mockResolvedValue(staleState);
    const adoptState = vi.fn(() => false);
    const reportRequestError = vi.fn();
    const params = makeSessionActionsParams({
      adoptState,
      reportRequestError,
    });
    const actions = useAppSessionActions(params);

    await actions.handleCancelQueuedPrompt("session-1", "prompt-1");

    expect(adoptState).toHaveBeenCalledWith(staleState);
    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
    expect(reportRequestError).toHaveBeenCalledWith(originalError);
  });

  it("reads live sessions after stale same-instance settings success", async () => {
    const previousSession: Session = {
      ...makeSession("session-1"),
      model: "gpt-5.4",
      reasoningEffort: "high",
      sessionMutationStamp: 1,
      modelOptions: [
        {
          label: "GPT Small",
          value: "gpt-small",
          supportedReasoningEfforts: ["low"],
        },
      ],
    };
    const staleSession: Session = {
      ...previousSession,
      model: "gpt-small",
      reasoningEffort: "high",
      sessionMutationStamp: 2,
    };
    const liveSession: Session = {
      ...previousSession,
      model: "gpt-small",
      reasoningEffort: "low",
      sessionMutationStamp: 2,
    };
    const staleState = {
      ...makeStateResponse(4),
      sessions: [staleSession],
    };
    vi.spyOn(api, "updateSessionSettings").mockResolvedValue(staleState);
    const setSessionSettingNotices = vi.fn();
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([
      [previousSession.id, previousSession],
    ]);
    params.lookups.activeSession = previousSession;
    params.refs.sessionsRef.current = [liveSession];
    params.setters.setSessionSettingNotices = setSessionSettingNotices;
    const actions = useAppSessionActions(params);

    await actions.handleSessionSettingsChange(
      "session-1",
      "model",
      "gpt-small",
    );

    const noticeUpdater = setSessionSettingNotices.mock.calls[0]?.[0] as
      | ((current: Record<string, string>) => Record<string, string>)
      | undefined;
    expect(noticeUpdater?.({})).toEqual({
      "session-1":
        "GPT Small only supports low reasoning, so TermAl reset effort from high to low.",
    });
  });

  it("reads response sessions after adopted settings success", async () => {
    const previousSession: Session = {
      ...makeSession("session-1"),
      model: "gpt-5.4",
      reasoningEffort: "high",
      modelOptions: [
        {
          label: "GPT Small",
          value: "gpt-small",
          supportedReasoningEfforts: ["low"],
        },
      ],
    };
    const responseSession: Session = {
      ...previousSession,
      model: "gpt-small",
      reasoningEffort: "low",
    };
    const staleLocalSession: Session = {
      ...previousSession,
      model: "gpt-small",
      reasoningEffort: "high",
    };
    const state = {
      ...makeStateResponse(6),
      sessions: [responseSession],
    };
    vi.spyOn(api, "updateSessionSettings").mockResolvedValue(state);
    const setSessionSettingNotices = vi.fn();
    const params = makeSessionActionsParams({
      adoptState: vi.fn(() => true),
    });
    params.lookups.sessionLookup = new Map([
      [previousSession.id, previousSession],
    ]);
    params.lookups.activeSession = previousSession;
    params.refs.sessionsRef.current = [staleLocalSession];
    params.setters.setSessionSettingNotices = setSessionSettingNotices;
    const actions = useAppSessionActions(params);

    await actions.handleSessionSettingsChange(
      "session-1",
      "model",
      "gpt-small",
    );

    const noticeUpdater = setSessionSettingNotices.mock.calls[0]?.[0] as
      | ((current: Record<string, string>) => Record<string, string>)
      | undefined;
    expect(noticeUpdater?.({})).toEqual({
      "session-1":
        "GPT Small only supports low reasoning, so TermAl reset effort from high to low.",
    });
  });

  it("reports project-scoped stale same-instance creation success when the project exists locally", async () => {
    const project: Project = {
      id: "project-1",
      name: "Project",
      rootPath: "/repo",
      remoteId: "local",
    };
    const state = {
      ...makeStateResponse(4),
      projects: [project],
    };
    vi.spyOn(api, "createProject").mockResolvedValue({
      projectId: project.id,
      state,
    });
    const setSelectedProjectId = vi.fn();
    const params = makeSessionActionsParams({
      newProjectRootPath: "/repo",
    });
    params.lookups.projectLookup = new Map();
    params.refs.projectsRef.current = [project];
    params.setters.setSelectedProjectId = setSelectedProjectId;
    const actions = useAppSessionActions(params);

    await expect(actions.handleCreateProject()).resolves.toBe(true);

    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
    expect(setSelectedProjectId).toHaveBeenCalledWith(project.id);
  });

  it("recovers project-scoped stale same-instance creation when local evidence is missing", async () => {
    const state = {
      ...makeStateResponse(4),
      projects: [],
    };
    vi.spyOn(api, "createProject").mockResolvedValue({
      projectId: "project-1",
      state,
    });
    const setSelectedProjectId = vi.fn();
    const params = makeSessionActionsParams({
      newProjectRootPath: "/repo",
    });
    params.lookups.projectLookup = new Map();
    params.refs.projectsRef.current = [];
    params.setters.setSelectedProjectId = setSelectedProjectId;
    const actions = useAppSessionActions(params);

    await expect(actions.handleCreateProject()).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      allowUnknownServerInstance: true,
    });
    expect(setSelectedProjectId).not.toHaveBeenCalled();
  });
});
