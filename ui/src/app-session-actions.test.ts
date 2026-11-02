import { ApiRequestError } from "./api-request";
import { waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import { useAppSessionActions } from "./app-session-actions";
import { withActionSessionAuthority, type TestSessionActionsParams } from "./session-publication-test-fixtures";
import { reconcileSingleSession, reconcileStateSessionSummaries } from "./session-reconcile";
import type { StateResponse } from "./api";
import type { AgentType, ConversationMarker, Project, Session } from "./types";
import type { WorkspaceState } from "./workspace-types";

type TestSession = Session & { messageCount: number; queuePaused: boolean };

function makeSession(
  id: string,
  overrides: Partial<Session> = {},
): TestSession {
  const messages = overrides.messages ?? [];
  return {
    id,
    name: "Session",
    emoji: "S",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.4",
    status: "idle",
    preview: "Ready",
    messages,
    ...overrides,
    messageCount: overrides.messageCount ?? messages.length,
    queuePaused: overrides.queuePaused ?? false,
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
      defaultCodexModel: "default",
      defaultClaudeModel: "default",
      defaultCursorModel: "default",
      defaultGeminiModel: "default",
      defaultKimiModel: "default",
      defaultOpenCodeModel: "default",
      defaultOpenCodeApprovalMode: "ask",
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

function makeConversationMarker(
  overrides: Partial<ConversationMarker> = {},
): ConversationMarker {
  return {
    id: "marker-1",
    sessionId: "session-1",
    kind: "checkpoint",
    name: "Checkpoint",
    body: null,
    color: "#3b82f6",
    messageId: "message-1",
    messageIndexHint: 0,
    endMessageId: null,
    endMessageIndexHint: null,
    createdAt: "2026-05-01 10:00:00",
    updatedAt: "2026-05-01 10:00:00",
    createdBy: "user",
    ...overrides,
  };
}

function makeWorkspace(): WorkspaceState {
  return {
    lastContentPaneId: null,
    lastViewerPaneId: null,
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
  overrides: Partial<TestSessionActionsParams> = {},
): TestSessionActionsParams {
  const session = makeSession("session-1");
  const noopSetter = vi.fn();
  const classifierProjectsRef = { current: [] as Project[] };
  let refs: TestSessionActionsParams["refs"];
  refs = {
    isMountedRef: { current: true },
    latestStateRevisionRef: { current: 5 },
    stateDiffBaselineRevisionRef: { current: null },
    lastSeenServerInstanceIdRef: { current: "server-a" },
    sessionsRef: { current: [session] },
    actionStateClassifierContextRef: {
      current: {
        getSnapshot: () => ({
          revision: refs.latestStateRevisionRef.current,
          serverInstanceId: refs.lastSeenServerInstanceIdRef.current,
          projects: classifierProjectsRef.current,
          sessions: refs.sessionsRef.current,
        }),
      },
    },
    draftsBySessionIdRef: { current: {} },
    draftAttachmentsBySessionIdRef: { current: {} },
    confirmedUnknownModelSendsRef: { current: new Set() },
    activePromptPollCancelRef: { current: null },
    activePromptPollSessionIdRef: { current: null },
    refreshingSessionModelOptionIdsRef: { current: {} },
    configuringClonedSessionIdsRef: { current: {} },
    refreshingAgentCommandSessionIdsRef: { current: {} },
  };

  return withActionSessionAuthority({
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
      defaultCodexModel: "default",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
      defaultClaudeModel: "default",
      defaultCursorModel: "default",
      defaultCursorMode: "agent",
      defaultGeminiApprovalMode: "default",
      defaultGeminiModel: "default",
      defaultKimiModel: "default",
      defaultOpenCodeModel: "default",
      defaultOpenCodeApprovalMode: "ask",
    },
    refs,
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
    requestSessionBottomFollow: vi.fn(),
    forceSseReconnect: vi.fn(() => 42),
    ...overrides,
  });
}

function expectRequestErrorDeferredUpdatesOnly(
  setRequestError: Parameters<
    typeof useAppSessionActions
  >[0]["setters"]["setRequestError"],
) {
  const calls = vi.mocked(setRequestError).mock.calls;
  expect(setRequestError).not.toHaveBeenCalledWith(null);
  expect(calls.length).toBeGreaterThan(0);
  expect(calls.every(([next]) => typeof next === "function")).toBe(true);
  let nextFlags: Record<string, boolean> = { "other-session": true };
  for (const [next] of calls) {
    const applyDeferredUpdate = next as unknown as (
      previousFlags: Record<string, boolean>,
    ) => Record<string, boolean>;
    nextFlags = applyDeferredUpdate(nextFlags);
    expect(nextFlags).toEqual(
      expect.objectContaining({ "other-session": true }),
    );
  }
  expect(nextFlags).toEqual({ "other-session": true });
}

type DefaultModelKey =
  | "defaultClaudeModel"
  | "defaultCodexModel"
  | "defaultCursorModel"
  | "defaultGeminiModel"
  | "defaultKimiModel"
  | "defaultOpenCodeModel";

const MODEL_PICKER_AGENT_CASES = [
  {
    agent: "Kimi",
    defaultModelKey: "defaultKimiModel",
    customModel: "configured-kimi-model",
  },
  {
    agent: "Claude",
    defaultModelKey: "defaultClaudeModel",
    customModel: "claude-opus-4.5",
  },
  {
    agent: "Codex",
    defaultModelKey: "defaultCodexModel",
    customModel: "gpt-5.5",
  },
  {
    agent: "Cursor",
    defaultModelKey: "defaultCursorModel",
    customModel: "cursor-max",
  },
  {
    agent: "Gemini",
    defaultModelKey: "defaultGeminiModel",
    customModel: "gemini-2.5-pro",
  },
  {
    agent: "OpenCode",
    defaultModelKey: "defaultOpenCodeModel",
    customModel: "openai/gpt-5.6-sol",
  },
] satisfies ReadonlyArray<{
  agent: AgentType;
  defaultModelKey: DefaultModelKey;
  customModel: string;
}>;

describe("useAppSessionActions", () => {
  it("applies a creation override only to that session and retains the app default for later creates", async () => {
    const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6,
      serverInstanceId: "server-a",
      session: makeSession("session-new", { agent: "OpenCode" }),
    } as Awaited<ReturnType<typeof api.createSession>>);
    const params = makeSessionActionsParams();
    params.defaults.defaultOpenCodeApprovalMode = "ask";
    const actions = useAppSessionActions(params);
    await actions.handleNewSession({ agent: "OpenCode", opencodeApprovalMode: "auto-approve" });
    await actions.handleNewSession({ agent: "OpenCode" });
    expect(createSessionSpy).toHaveBeenNthCalledWith(1, expect.objectContaining({ opencodeApprovalMode: "auto-approve" }));
    expect(createSessionSpy).toHaveBeenNthCalledWith(2, expect.objectContaining({ opencodeApprovalMode: "ask" }));
    expect(params.defaults.defaultOpenCodeApprovalMode).toBe("ask");
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("requests tail follow before awaiting an approval decision response", async () => {
    let resolveApproval!: (state: StateResponse) => void;
    const submitApprovalSpy = vi
      .spyOn(api, "submitApproval")
      .mockImplementation(
        () =>
          new Promise<StateResponse>((resolve) => {
            resolveApproval = resolve;
          }),
      );
    const params = makeSessionActionsParams({
      adoptState: vi.fn(() => true),
    });
    const actions = useAppSessionActions(params);

    const approval = actions.handleApprovalDecision(
      "session-1",
      "approval-1",
      "accepted",
    );

    expect(params.requestSessionBottomFollow).toHaveBeenCalledWith("session-1");
    expect(submitApprovalSpy).toHaveBeenCalledWith(
      "session-1",
      "approval-1",
      "accepted",
    );

    resolveApproval(makeStateResponse(6));
    await approval;
  });

  it.each(MODEL_PICKER_AGENT_CASES)(
    "sends a configured default model when creating a new $agent session",
    async ({ agent, defaultModelKey, customModel }) => {
      const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
        revision: 6,
        serverInstanceId: "server-a",
        session: makeSession("session-new", { agent, model: customModel }),
      } as Awaited<ReturnType<typeof api.createSession>>);
      const params = makeSessionActionsParams();
      params.defaults[defaultModelKey] = customModel;
      const actions = useAppSessionActions(params);

      await expect(actions.handleNewSession({ agent })).resolves.toBe(true);

      expect(createSessionSpy).toHaveBeenCalledWith(
        expect.objectContaining({
          agent,
          model: customModel,
        }),
      );
    },
  );

  it.each(MODEL_PICKER_AGENT_CASES)(
    "omits mixed-case default model sentinel when creating a new $agent session",
    async ({ agent, defaultModelKey }) => {
      const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
        revision: 6,
        serverInstanceId: "server-a",
        session: makeSession("session-new", { agent }),
      } as Awaited<ReturnType<typeof api.createSession>>);
      const params = makeSessionActionsParams();
      params.defaults[defaultModelKey] = " DEFAULT ";
      const actions = useAppSessionActions(params);

      await expect(actions.handleNewSession({ agent })).resolves.toBe(true);

      expect(createSessionSpy).toHaveBeenCalledWith(
        expect.objectContaining({ agent }),
      );
      expect(createSessionSpy).toHaveBeenCalledWith(
        expect.not.objectContaining({
          model: expect.any(String),
        }),
      );
    },
  );

  it("lets the target backend apply persisted Codex prompt defaults on ordinary create", async () => {
    const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6,
      serverInstanceId: "server-a",
      session: makeSession("session-new"),
    } as Awaited<ReturnType<typeof api.createSession>>);
    const actions = useAppSessionActions(makeSessionActionsParams());

    await expect(
      actions.handleNewSession({ agent: "Codex" }),
    ).resolves.toBe(true);

    const request = createSessionSpy.mock.calls[0]?.[0];
    expect(request).toBeDefined();
    expect(request).not.toHaveProperty("approvalPolicy");
    expect(request).not.toHaveProperty("reasoningEffort");
    expect(request).not.toHaveProperty("sandboxMode");
  });

  it("preserves explicit Codex prompt settings when cloning a session", async () => {
    const source = makeSession("session-source", {
      approvalPolicy: "on-request",
      reasoningEffort: "high",
      sandboxMode: "read-only",
    });
    const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6,
      serverInstanceId: "server-a",
      session: makeSession("session-clone"),
    } as Awaited<ReturnType<typeof api.createSession>>);
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[source.id, source]]);
    params.refs.sessionsRef.current = [source];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCloneSessionFromExisting(source.id),
    ).resolves.toBe(true);

    expect(createSessionSpy).toHaveBeenCalledWith(
      expect.objectContaining({
        approvalPolicy: "on-request",
        reasoningEffort: "high",
        sandboxMode: "read-only",
      }),
    );
  });

  it("preserves OpenCode authority, reasoning variant, and mode when cloning a session", async () => {
    const source = makeSession("session-opencode-source", {
      agent: "OpenCode",
      opencodeApprovalMode: "auto-approve",
      model: "openai/gpt-5.6-sol",
      opencodeModel: "auto",
      opencodeEffort: "high",
      opencodeMode: "plan",
    });
    const clone = makeSession("session-opencode-clone", {
      agent: "OpenCode",
      model: "default",
      opencodeModel: "auto",
      opencodeEffort: "high",
      opencodeMode: "plan",
    });
    const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6,
      serverInstanceId: "server-a",
      session: clone,
      sessionId: clone.id,
    });
    const updateSessionSettingsSpy = vi
      .spyOn(api, "updateSessionSettings")
      .mockResolvedValue({
        ...makeStateResponse(7),
        sessions: [clone],
      });
    vi.spyOn(api, "refreshSessionModelOptions").mockResolvedValue({
      ...makeStateResponse(8),
      sessions: [clone],
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[source.id, source]]);
    params.refs.sessionsRef.current = [source];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCloneSessionFromExisting(source.id),
    ).resolves.toBe(true);

    expect(createSessionSpy).toHaveBeenCalledWith(
      expect.objectContaining({
        agent: "OpenCode",
        model: "auto",
        opencodeApprovalMode: "auto-approve",
      }),
    );
    expect(updateSessionSettingsSpy).toHaveBeenCalledWith(clone.id, {
      opencodeEffort: "high",
      opencodeMode: "plan",
    });
  });

  it.each([undefined, "ask", "auto-approve"] as const)("creates Kimi with approval draft %s and backend effort default", async approval => {
    const create = vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6, serverInstanceId: "server-a", session: makeSession("new", { agent: "Kimi" }), sessionId: "new",
    });
    vi.spyOn(api, "refreshSessionModelOptions").mockResolvedValue(makeStateResponse(7));
    const params = makeSessionActionsParams();
    params.defaults.defaultKimiApprovalMode = "auto-approve";
    const actions = useAppSessionActions(params);
    await expect(actions.handleNewSession({ agent: "Kimi", kimiApprovalMode: approval, kimiMode: "plan" })).resolves.toBe(true);
    expect(create).toHaveBeenCalledWith(expect.objectContaining({
      kimiApprovalMode: approval ?? "auto-approve", kimiMode: "plan",
    }));
    expect(create.mock.calls[0]?.[0]).not.toHaveProperty("kimiEffort");
  });

  it.each([undefined, "max"])("preserves source Kimi approval, mode and effort %s on clone", async effort => {
    const source = makeSession("kimi-source", { agent: "Kimi", kimiEffort: effort, kimiApprovalMode: "ask", kimiMode: "yolo" });
    const clone = makeSession("kimi-clone", { agent: "Kimi", kimiEffort: "high" });
    const create = vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6, serverInstanceId: "server-a", session: clone, sessionId: clone.id,
    });
    vi.spyOn(api, "refreshSessionModelOptions").mockResolvedValue({
      ...makeStateResponse(7), sessions: [{ ...clone, kimiEffortOptions: [{ value: "max", label: "Max" }] }],
    });
    const patch = vi.spyOn(api, "updateSessionSettings").mockResolvedValue({
      ...makeStateResponse(8), sessions: [{ ...clone, kimiEffort: effort }],
    });
    const params = makeSessionActionsParams({ adoptState: vi.fn(() => true) });
    params.defaults.defaultKimiApprovalMode = "auto-approve";
    params.lookups.sessionLookup = new Map([[source.id, source]]);
    params.refs.sessionsRef.current = [source];
    await expect(useAppSessionActions(params).handleCloneSessionFromExisting(source.id)).resolves.toBe(true);
    expect(create).toHaveBeenCalledWith(expect.objectContaining({ kimiApprovalMode: "ask", kimiMode: "yolo" }));
    expect(patch).toHaveBeenCalledWith(clone.id, { kimiEffort: effort ?? "auto" });
    expect(params.refs.configuringClonedSessionIdsRef.current[clone.id]).toBeUndefined();
  });

  it.each([true, false])("discovers and preserves explicit Kimi effort on clone (accepted=%s)", async (accepted) => {
    const source = makeSession("kimi-source", { agent: "Kimi", kimiEffort: "max" });
    const clone = makeSession("kimi-clone", { agent: "Kimi" });
    vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6, serverInstanceId: "server-a", session: clone, sessionId: clone.id,
    });
    const refresh = vi.spyOn(api, "refreshSessionModelOptions").mockResolvedValue({
      ...makeStateResponse(7), sessions: [{ ...clone, kimiEffortOptions: accepted ? [{ value: "max", label: "Max" }] : [] }],
    });
    const patch = vi.spyOn(api, "updateSessionSettings");
    if (accepted) patch.mockResolvedValue({ ...makeStateResponse(8), sessions: [{ ...clone, kimiEffort: "max" }] });
    else patch.mockRejectedValue(new Error("Kimi did not advertise this reasoning effort"));
    const params = makeSessionActionsParams({ adoptState: vi.fn(() => true) });
    params.lookups.sessionLookup = new Map([[source.id, source]]);
    params.refs.sessionsRef.current = [source];
    const actions = useAppSessionActions(params);
    await expect(actions.handleCloneSessionFromExisting(source.id)).resolves.toBe(accepted);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledWith(clone.id);
    expect(patch).toHaveBeenCalledWith(clone.id, { kimiEffort: "max" });
    expect(refresh.mock.invocationCallOrder[0]).toBeLessThan(patch.mock.invocationCallOrder[0]!);
    if (!accepted) expect(params.reportRequestError).toHaveBeenCalledWith(
      expect.objectContaining({ message: expect.stringContaining("Clone created, but its reasoning effort could not be preserved") }),
    );
  });

  it.each([true, false])("awaits one guarded refresh while cloning Kimi effort (adopted=%s)", async (adopted) => {
    const source = makeSession("kimi-source", { agent: "Kimi", model: "model-b", kimiEffort: "max" });
    const clone = makeSession("kimi-clone", { agent: "Kimi", model: "model-b" });
    vi.spyOn(api, "createSession").mockResolvedValue({
      revision: 6, serverInstanceId: "server-a", session: clone, sessionId: clone.id,
    });
    let resolveRefresh!: (state: StateResponse) => void;
    const refresh = vi.spyOn(api, "refreshSessionModelOptions").mockReturnValue(
      new Promise<StateResponse>((resolve) => { resolveRefresh = resolve; }),
    );
    const patch = vi.spyOn(api, "updateSessionSettings").mockResolvedValue({
      ...makeStateResponse(8), sessions: [{ ...clone, kimiEffort: "max" }],
    });
    const params = makeSessionActionsParams({ adoptState: vi.fn(() => adopted) });
    params.lookups.sessionLookup = new Map([[source.id, source]]);
    params.refs.sessionsRef.current = [source];
    const actions = useAppSessionActions(params);
    const cloning = actions.handleCloneSessionFromExisting(source.id);
    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(patch).not.toHaveBeenCalled();
    expect(params.refs.configuringClonedSessionIdsRef.current[clone.id]).toBe(true);
    expect(actions.handleSend(clone.id, "do not send before effort is saved")).toBe(false);
    await actions.handleSessionSettingsChange(clone.id, "kimiEffort", "low");
    expect(params.setters.setRequestError).toHaveBeenCalledWith(
      "Wait for the clone's reasoning effort to finish configuring before changing settings.",
    );
    expect(patch).not.toHaveBeenCalled();
    await expect(actions.handleRefreshSessionModelOptions(clone.id)).resolves.toBe("skipped");
    expect(refresh).toHaveBeenCalledTimes(1);
    resolveRefresh({ ...makeStateResponse(7), sessions: [
      { ...clone, kimiEffortOptions: [{ value: "max", label: "Max" }] },
    ] });
    await expect(cloning).resolves.toBe(adopted);
    expect(params.refs.configuringClonedSessionIdsRef.current[clone.id]).toBeUndefined();
    expect(refresh).toHaveBeenCalledTimes(1);
    if (adopted) expect(patch).toHaveBeenCalledWith(clone.id, { kimiEffort: "max" });
    else {
      expect(patch).not.toHaveBeenCalled();
      expect(params.requestActionRecoveryResync).toHaveBeenCalled();
    }
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
      sseReconnectRequestId: 42,
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

  it("forces SSE reconnect when send response is rejected after server restart", async () => {
    const restartedState = {
      ...makeStateResponse(6),
      serverInstanceId: "server-b",
      sessions: [makeSession("session-1", { status: "active" })],
    };
    const sendMessageSpy = vi
      .spyOn(api, "sendMessage")
      .mockResolvedValue(restartedState);
    const params = makeSessionActionsParams({
      adoptState: vi.fn(() => false),
    });
    const actions = useAppSessionActions(params);

    expect(actions.handleSend("session-1", "hello")).toBe(true);

    await waitFor(() => {
      expect(sendMessageSpy).toHaveBeenCalledWith(
        "session-1",
        "hello",
        [],
        null,
      );
      expect(params.adoptState).toHaveBeenCalledWith(restartedState);
      expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
        allowUnknownServerInstance: true,
        sseReconnectRequestId: 42,
      });
      expect(params.forceSseReconnect).toHaveBeenCalledTimes(1);
    });
    params.refs.activePromptPollCancelRef.current?.();
  });

  it("publishes a pending prompt optimistically before send returns", () => {
    vi.spyOn(api, "sendMessage").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const params = makeSessionActionsParams();
    const actions = useAppSessionActions(params);

    expect(actions.handleSend("session-1", "hello")).toBe(true);

    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toEqual([
      expect.objectContaining({
        expandedText: null,
        localOnly: true,
        text: "hello",
        transcriptEndIndexAtEnqueue: 0,
      }),
    ]);
    expect(params.setters.setSessions).toHaveBeenCalledWith(
      params.refs.sessionsRef.current,
    );
  });

  it("keeps the draft cleared when an accepted Human prompt is queued after provider startup failure", async () => {
    const queuedSession = makeSession("session-1", {
      status: "error", queuePaused: true, messagesLoaded: true,
      preview: "Human prompt queued; start failed: provider unavailable. Resume retries the retained prompt.",
      pendingPrompts: [{ id: "accepted-human", timestamp: "2026-11-01T00:00:00Z", text: "run migration" }],
    });
    const response = {
      ...makeStateResponse(6),
      sessions: [makeSession("session-1", {
        status: "error", queuePaused: true,
        preview: queuedSession.preview,
      })],
    } satisfies StateResponse;
    const send = vi.spyOn(api, "sendMessage").mockResolvedValue(response);
    let params!: ReturnType<typeof makeSessionActionsParams>;
    const adoptState = vi.fn((state: StateResponse) => {
      params.refs.sessionsRef.current = reconcileStateSessionSummaries(
        params.refs.sessionsRef.current, state.sessions,
      );
      return true;
    });
    params = makeSessionActionsParams({ adoptState });
    params.refs.draftsBySessionIdRef.current = { "session-1": "run migration" };
    const actions = useAppSessionActions(params);

    expect(actions.handleSend("session-1")).toBe(true);
    await waitFor(() => expect(adoptState).toHaveBeenCalledWith(response));

    expect(params.refs.draftsBySessionIdRef.current["session-1"]).toBe("");
    expect(params.reportRequestError).not.toHaveBeenCalled();
    expect(send).toHaveBeenCalledTimes(1);
    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toEqual([
      expect.objectContaining({ localOnly: true, text: "run migration" }),
    ]);
    expect(params.refs.sessionsRef.current[0]?.queuePaused).toBe(true);
    params.refs.sessionsRef.current = [reconcileSingleSession(
      params.refs.sessionsRef.current[0]!, queuedSession,
    )];
    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toEqual([
      expect.objectContaining({ id: "accepted-human", text: "run migration" }),
    ]);
    expect(params.refs.draftsBySessionIdRef.current["session-1"]).toBe("");
    expect(actions.handleSend("session-1")).toBe(false);
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("keeps one optimistic prompt when the send response is transcript-free", async () => {
    const retainedMessage = {
      id: "message-79",
      type: "text" as const,
      timestamp: "10:00",
      author: "assistant" as const,
      text: "Ready",
    };
    const previousSession = makeSession("session-1", {
      messagesLoaded: false,
      messageStartIndex: 79,
      messageCount: 80,
      sessionMutationStamp: 10,
      messages: [retainedMessage],
    });
    const response = {
      ...makeStateResponse(6),
      sessions: [
        makeSession("session-1", {
          messagesLoaded: false,
          messageStartIndex: 79,
          messageCount: 81,
          sessionMutationStamp: 11,
          messages: [
            { ...retainedMessage },
            {
              id: "message-80",
              type: "text",
              timestamp: "10:01",
              author: "you",
              text: "hello",
            },
          ],
        }),
      ],
    } satisfies StateResponse;
    vi.spyOn(api, "sendMessage").mockResolvedValue(response);
    let params!: ReturnType<typeof makeSessionActionsParams>;
    const adoptState = vi.fn((state: StateResponse) => {
      const sessions = reconcileStateSessionSummaries(
        params.refs.sessionsRef.current,
        state.sessions,
      );
      params.refs.sessionsRef.current = sessions;
      params.setters.setSessions(sessions);
      return true;
    });
    params = makeSessionActionsParams({ adoptState });
    params.refs.sessionsRef.current = [previousSession];
    params.lookups.sessionLookup.set(previousSession.id, previousSession);
    params.lookups.activeSession = previousSession;
    const actions = useAppSessionActions(params);

    expect(actions.handleSend("session-1", "hello")).toBe(true);
    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toEqual([
      expect.objectContaining({
        localOnly: true,
        text: "hello",
        transcriptEndIndexAtEnqueue: 80,
      }),
    ]);

    await waitFor(() => expect(adoptState).toHaveBeenCalledWith(response));

    const adoptedSession = params.refs.sessionsRef.current[0]!;
    const persistedCopies = adoptedSession.messages.filter(
      (message) =>
        message.type === "text" &&
        message.author === "you" &&
        message.text === "hello",
    ).length;
    const queuedCopies = (adoptedSession.pendingPrompts ?? []).filter(
      (prompt) => prompt.text === "hello",
    ).length;
    expect(persistedCopies + queuedCopies).toBe(1);
    expect(adoptedSession.pendingPrompts).toEqual([
      expect.objectContaining({
        localOnly: true,
        text: "hello",
      }),
    ]);
  });

  it("keeps the optimistic pending prompt after a stale send response", async () => {
    const staleState = {
      ...makeStateResponse(4),
      sessions: [makeSession("session-1", { status: "active" })],
    };
    vi.spyOn(api, "sendMessage").mockResolvedValue(staleState);
    const params = makeSessionActionsParams({
      adoptState: vi.fn(() => false),
    });
    const actions = useAppSessionActions(params);

    expect(actions.handleSend("session-1", "hello")).toBe(true);
    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toHaveLength(1);

    await waitFor(() => {
      expect(params.adoptState).toHaveBeenCalledWith(staleState);
      expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toEqual([
        expect.objectContaining({
          localOnly: true,
          text: "hello",
        }),
      ]);
    });
  });

  it("threads the originating pane into session-scoped action recovery", async () => {
    const state = {
      ...makeStateResponse(6),
      serverInstanceId: "server-b",
    };
    vi.spyOn(api, "renameSession").mockResolvedValue(state);
    const params = makeSessionActionsParams();
    params.lookups.workspace = {
      lastContentPaneId: null,
      lastViewerPaneId: null,
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
      sseReconnectRequestId: 42,
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

  it("resolves a local optimistic prompt before canceling its server queue entry", async () => {
    const optimisticPrompt = {
      id: "optimistic-send-session-1-abc-1",
      timestamp: "10:00",
      text: "queued follow-up",
      localOnly: true,
      transcriptEndIndexAtEnqueue: 4,
    };
    const authoritativePrompt = {
      id: "pending-prompt-7",
      timestamp: "10:00",
      text: "queued follow-up",
    };
    const localSession = makeSession("session-1", {
      status: "active",
      pendingPrompts: [optimisticPrompt],
    });
    const authoritativeSession = makeSession("session-1", {
      status: "active",
      pendingPrompts: [authoritativePrompt],
    });
    const canceledState = {
      ...makeStateResponse(7),
      sessions: [
        makeSession("session-1", {
          status: "active",
        }),
      ],
    };
    const fetchSessionTail = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 6,
      serverInstanceId: "server-a",
      session: authoritativeSession,
    });
    const cancelQueuedPrompt = vi
      .spyOn(api, "cancelQueuedPrompt")
      .mockResolvedValue(canceledState);
    const adoptState = vi.fn(() => true);
    const params = makeSessionActionsParams({ adoptState });
    params.refs.sessionsRef.current = [localSession];
    const actions = useAppSessionActions(params);

    await actions.handleCancelQueuedPrompt(
      "session-1",
      optimisticPrompt.id,
    );

    expect(fetchSessionTail).toHaveBeenCalledWith("session-1");
    expect(cancelQueuedPrompt).toHaveBeenCalledWith(
      "session-1",
      authoritativePrompt.id,
    );
    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toBeUndefined();
    expect(adoptState).toHaveBeenCalledWith(canceledState, undefined);
  });

  it("adopts the snapshot returned by a queue resume", async () => {
    const resumedState = {
      ...makeStateResponse(5),
      sessions: [
        makeSession("session-1", {
          sessionMutationStamp: 3,
          queuePaused: false,
        }),
      ],
    };
    const resumeSpy = vi
      .spyOn(api, "resumeSessionQueue")
      .mockResolvedValue(resumedState);
    const adoptState = vi.fn(() => true);
    const params = makeSessionActionsParams({ adoptState });
    const actions = useAppSessionActions(params);

    await actions.handleResumeSessionQueue("session-1");

    expect(resumeSpy).toHaveBeenCalledWith("session-1");
    expect(adoptState).toHaveBeenCalledTimes(1);
    expect(adoptState).toHaveBeenCalledWith(resumedState, undefined);
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("binds a human queue choice to the displayed head without changing unknown content", async () => {
    const prompt = { id: "legacy-head", timestamp: "2026-11-01T00:00:00Z", text: "original input" };
    const originalError = new Error("the original delivery remains unknown");
    const resumeSpy = vi.spyOn(api, "resumeSessionQueue").mockRejectedValue(originalError);
    const reportRequestError = vi.fn();
    const params = makeSessionActionsParams({ reportRequestError });
    params.refs.sessionsRef.current = [makeSession("session-1", {
      pendingPrompts: [prompt], sessionMutationStamp: 42, queuePaused: true,
    })];
    const actions = useAppSessionActions(params);

    await actions.handleResumeSessionQueue("session-1");

    expect(resumeSpy).toHaveBeenCalledWith("session-1", { prompt, sessionMutationStamp: 42 });
    expect(reportRequestError).toHaveBeenCalledWith(originalError);
    expect(params.refs.sessionsRef.current[0]?.pendingPrompts).toEqual([prompt]);
    expect(prompt).toEqual({ id: "legacy-head", timestamp: "2026-11-01T00:00:00Z", text: "original input" });
  });

  it("uses ordinary Resume at OFF and preserves the exact-head choice at ON", async () => {
    for (const { gated, optimistic, ordinaryOff, enabled = true } of [
      { gated: false, optimistic: false, ordinaryOff: true },
      { gated: false, optimistic: true, ordinaryOff: true },
      { gated: true, optimistic: false, ordinaryOff: false },
      // Disabled integration and the global kill switch keep the ordinary
      // pre-adapter policy, rather than the Base/OFF history-skipping lane.
      { gated: true, optimistic: false, ordinaryOff: false, enabled: false },
      { gated: false, optimistic: false, ordinaryOff: false, enabled: false },
    ]) {
      const prompt = { id: "unknown-original", timestamp: "2026-11-01T00:00:00Z",
        text: "unknown provider attempt", engramInterrupted: true, isEngramRetained: true,
        ...(optimistic ? { localOnly: true } : {}),
      };
      const successor = { id: "ordinary-successor", timestamp: "2026-11-01T00:00:01Z",
        text: "known ordinary successor" };
      const params = makeSessionActionsParams();
      const session = makeSession("session-1", { projectId: "project-1",
        engramControlOff: ordinaryOff,
        pendingPrompts: [prompt, successor], sessionMutationStamp: 42, queuePaused: true });
      params.refs.sessionsRef.current = [session];
      const snapshot = params.refs.actionStateClassifierContextRef.current.getSnapshot;
      params.refs.actionStateClassifierContextRef.current.getSnapshot = () => ({
        ...snapshot(), projects: [{ id: "project-1", name: "Project", rootPath: "/repo",
          engram: { enabled, turnGatedControl: gated } }],
      });
      const resume = vi.spyOn(api, "resumeSessionQueue").mockResolvedValue({
        ...makeStateResponse(6), sessions: [{ ...session, queuePaused: false }],
      });
      const tail = vi.spyOn(api, "fetchSessionTail").mockRejectedValue(
        new Error("ordinary OFF Resume must not depend on selecting a projected head"),
      );
      await useAppSessionActions(params).handleResumeSessionQueue("session-1");
      expect(resume).toHaveBeenCalledTimes(1);
      if (!ordinaryOff) {
        expect(resume).toHaveBeenCalledWith("session-1", { prompt, sessionMutationStamp: 42 });
      } else {
        expect(resume).toHaveBeenCalledWith("session-1");
      }
      expect(tail).not.toHaveBeenCalled();
      expect(prompt.text).toBe("unknown provider attempt");
      expect(successor.text).toBe("known ordinary successor");
      vi.restoreAllMocks();
    }
  });

  it("uses executing-session OFF applicability and keeps unknown remote selection", async () => {
    for (const kind of ["remote-unknown", "remote-off", "inherited-off"]) {
      const prompt = { id: "legacy-owner", timestamp: "10:00", text: "original input" };
      const params = makeSessionActionsParams();
      params.refs.sessionsRef.current = [makeSession("session-1", {
        projectId: kind === "inherited-off" ? undefined : "remote-project",
        remoteId: kind.startsWith("remote") ? "remote-owner" : undefined,
        pendingPrompts: [prompt], sessionMutationStamp: 42,
        ...(kind.endsWith("off") ? { engramControlOff: true } : {}),
      })];
      const snapshot = params.refs.actionStateClassifierContextRef.current.getSnapshot;
      params.refs.actionStateClassifierContextRef.current.getSnapshot = () => ({
        ...snapshot(), projects: [{ id: "remote-project", name: "Remote", rootPath: "/repo",
          remoteId: "remote-owner" }],
      });
      const resume = vi.spyOn(api, "resumeSessionQueue").mockResolvedValue(makeStateResponse(6));
      await useAppSessionActions(params).handleResumeSessionQueue("session-1");
      expect(resume).toHaveBeenCalledTimes(1);
      if (kind === "remote-unknown") {
        expect(resume).toHaveBeenCalledWith("session-1", { prompt, sessionMutationStamp: 42 });
      } else {
        expect(resume).toHaveBeenCalledWith("session-1");
      }
      vi.restoreAllMocks();
    }
  });

  it("adopts same-stamp OFF policy changes before choosing the Resume body", async () => {
    for (const summary of [false, true]) {
      for (const disableMutationStampFastPath of [false, true]) {
        for (const [before, after] of [
          [false, true], [true, false], [undefined, true],
          [true, undefined], [undefined, false], [false, undefined],
        ]) {
          const prompt = { id: "retained-head", timestamp: "10:00", text: "original input" };
          const previous = makeSession("session-1", { pendingPrompts: [prompt],
            messagesLoaded: true, sessionMutationStamp: 42, engramControlOff: before });
          const canonical = { ...previous, engramControlOff: after };
          const options = { disableMutationStampFastPath };
          const adopted = summary
            ? reconcileStateSessionSummaries([previous], [canonical], options)[0]
            : reconcileSingleSession(previous, canonical, options);
          expect(adopted.engramControlOff).toBe(after);
          expect(adopted.sessionMutationStamp).toBe(42);
          const params = makeSessionActionsParams();
          params.refs.sessionsRef.current = [adopted];
          const resume = vi.spyOn(api, "resumeSessionQueue").mockResolvedValue(makeStateResponse(6));
          await useAppSessionActions(params).handleResumeSessionQueue("session-1");
          expect(resume).toHaveBeenCalledTimes(1);
          if (after === true) {
            expect(resume).toHaveBeenCalledWith("session-1");
          } else {
            expect(resume).toHaveBeenCalledWith("session-1", { prompt, sessionMutationStamp: 42 });
          }
          vi.restoreAllMocks();
        }
      }
    }
  });

  it("hydrates an optimistic head before selecting Resume and refuses a changed head", async () => {
    for (const changed of [false, true]) {
      const optimistic = { id: "optimistic-human", timestamp: "10:00", text: "run migration", localOnly: true };
      const authoritative = { id: "accepted-human", timestamp: "10:00", text: "run migration" };
      const params = makeSessionActionsParams();
      params.refs.sessionsRef.current = [makeSession("session-1", {
        pendingPrompts: [optimistic], sessionMutationStamp: 4, queuePaused: true,
      })];
      vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
        revision: 8, serverInstanceId: "server-a", session: makeSession("session-1", {
          pendingPrompts: changed ? [{ ...authoritative, id: "replacement", text: "different work" }] : [authoritative],
          sessionMutationStamp: 8, queuePaused: true,
        }),
      });
      const resume = vi.spyOn(api, "resumeSessionQueue").mockResolvedValue(makeStateResponse(9));
      await useAppSessionActions(params).handleResumeSessionQueue("session-1");
      if (changed) {
        expect(resume).not.toHaveBeenCalled();
        expect(params.setters.setRequestError).toHaveBeenCalledWith("The queued prompt changed; refresh before resuming.");
      } else {
        expect(resume).toHaveBeenCalledWith("session-1", { prompt: authoritative, sessionMutationStamp: 8 });
      }
      vi.restoreAllMocks();
    }
  });

  it("reports a failed queue resume without a recovery resync", async () => {
    const originalError = new Error("resume failed");
    vi.spyOn(api, "resumeSessionQueue").mockRejectedValue(originalError);
    const reportRequestError = vi.fn();
    const params = makeSessionActionsParams({ reportRequestError });
    const actions = useAppSessionActions(params);

    await actions.handleResumeSessionQueue("session-1");

    expect(reportRequestError).toHaveBeenCalledWith(originalError);
    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
  });

  it("treats a model refresh conflict as a benign lifecycle deferral", async () => {
    vi.spyOn(api, "refreshSessionModelOptions").mockRejectedValue(
      new ApiRequestError(
        "request-failed",
        "session model options cannot be refreshed while the session is active or stopping",
        { status: 409 },
      ),
    );
    const reportRequestError = vi.fn();
    const params = makeSessionActionsParams({ reportRequestError });
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleRefreshSessionModelOptions("session-1"),
    ).resolves.toBe("deferred");

    expect(reportRequestError).not.toHaveBeenCalled();
    expect(params.setters.setRequestError).not.toHaveBeenCalledWith(
      expect.any(String),
    );
  });

  it("creates checkpoint markers and updates the local session slice", async () => {
    const marker: ConversationMarker = {
      id: "marker-1",
      sessionId: "session-1",
      kind: "checkpoint",
      name: "Checkpoint",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      messageIndexHint: 0,
      endMessageId: null,
      endMessageIndexHint: null,
      createdAt: "2026-05-01 10:00:00",
      updatedAt: "2026-05-01 10:00:00",
      createdBy: "user",
    };
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [],
    });
    const createConversationMarkerSpy = vi
      .spyOn(api, "createConversationMarker")
      .mockResolvedValue({
        marker,
        revision: 6,
        serverInstanceId: "server-a",
        sessionMutationStamp: 6,
      });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(true);

    expect(createConversationMarkerSpy).toHaveBeenCalledWith("session-1", {
      kind: "checkpoint",
      name: "Checkpoint",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      endMessageId: null,
    });
    expect(params.refs.sessionsRef.current[0].markers).toEqual([marker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(6);
    expect(params.refs.latestStateRevisionRef.current).toBe(6);
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("voids the state-diff baseline when a marker response advances the revision", async () => {
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [],
    });
    vi.spyOn(api, "createConversationMarker").mockResolvedValue({
      marker: makeConversationMarker(),
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.stateDiffBaselineRevisionRef.current = 5;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(true);

    // One marker's data cannot be a base a full-state diff builds on.
    expect(params.refs.latestStateRevisionRef.current).toBe(6);
    expect(params.refs.stateDiffBaselineRevisionRef.current).toBeNull();
  });

  it("uses the provided checkpoint marker label when creating markers", async () => {
    const marker = makeConversationMarker({
      name: "Review later",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [],
    });
    const createConversationMarkerSpy = vi
      .spyOn(api, "createConversationMarker")
      .mockResolvedValue({
        marker,
        revision: 6,
        serverInstanceId: "server-a",
        sessionMutationStamp: 6,
      });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1", {
        name: "  Review later  ",
      }),
    ).resolves.toBe(true);

    expect(createConversationMarkerSpy).toHaveBeenCalledWith("session-1", {
      kind: "checkpoint",
      name: "Review later",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      endMessageId: null,
    });
  });

  it("treats stale same-instance marker success as a no-op", async () => {
    const currentMarker = makeConversationMarker();
    const responseMarker = { ...currentMarker };
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [currentMarker],
      sessionMutationStamp: 7,
    });
    vi.spyOn(api, "createConversationMarker").mockResolvedValue({
      marker: responseMarker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(true);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([currentMarker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(7);
    expect(params.refs.latestStateRevisionRef.current).toBe(7);
    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("recovers stale marker success instead of trusting clock-only timestamp order", async () => {
    const currentMarker = makeConversationMarker({
      name: "Before midnight",
      updatedAt: "23:59:59",
    });
    const responseMarker = makeConversationMarker({
      name: "After midnight",
      updatedAt: "00:01:00",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "23:59",
        },
      ],
      markers: [currentMarker],
      sessionMutationStamp: 7,
    });
    vi.spyOn(api, "createConversationMarker").mockResolvedValue({
      marker: responseMarker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([currentMarker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(7);
    expect(params.refs.latestStateRevisionRef.current).toBe(7);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("recovers stale same-instance marker success when the marker is absent locally", async () => {
    const marker: ConversationMarker = {
      id: "marker-1",
      sessionId: "session-1",
      kind: "checkpoint",
      name: "Checkpoint",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      messageIndexHint: 0,
      endMessageId: null,
      endMessageIndexHint: null,
      createdAt: "2026-05-01 10:00:00",
      updatedAt: "2026-05-01 10:00:00",
      createdBy: "user",
    };
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [],
      sessionMutationStamp: 5,
    });
    vi.spyOn(api, "createConversationMarker").mockResolvedValue({
      marker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([]);
    expect(params.refs.latestStateRevisionRef.current).toBe(7);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("recovers stale same-instance marker success when local marker evidence is behind", async () => {
    const marker: ConversationMarker = {
      id: "marker-1",
      sessionId: "session-1",
      kind: "checkpoint",
      name: "Checkpoint",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      messageIndexHint: 0,
      endMessageId: null,
      endMessageIndexHint: null,
      createdAt: "2026-05-01 10:00:00",
      updatedAt: "2026-05-01 10:00:00",
      createdBy: "user",
    };
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
      sessionMutationStamp: 5,
    });
    vi.spyOn(api, "createConversationMarker").mockResolvedValue({
      marker: { ...marker, name: "New checkpoint" },
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([marker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(5);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("forces SSE reconnect when marker success comes from a new server instance", async () => {
    const marker: ConversationMarker = {
      id: "marker-1",
      sessionId: "session-1",
      kind: "checkpoint",
      name: "Checkpoint",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      messageIndexHint: 0,
      endMessageId: null,
      endMessageIndexHint: null,
      createdAt: "2026-05-01 10:00:00",
      updatedAt: "2026-05-01 10:00:00",
      createdBy: "user",
    };
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [],
    });
    vi.spyOn(api, "createConversationMarker").mockResolvedValue({
      marker,
      revision: 6,
      serverInstanceId: "server-b",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleCreateConversationMarker("session-1", "message-1"),
    ).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
      sseReconnectRequestId: 42,
    });
    expect(params.forceSseReconnect).toHaveBeenCalledTimes(1);
    expect(params.refs.sessionsRef.current[0].markers).toEqual([]);
    expect(params.setters.setRequestError).not.toHaveBeenCalledWith(null);
  });

  it("updates conversation markers through the guarded marker response path", async () => {
    const marker = makeConversationMarker();
    const updatedMarker = makeConversationMarker({
      kind: "bug",
      name: "Bug marker",
      color: "#ef4444",
      updatedAt: "2026-05-01 10:01:00",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
      sessionMutationStamp: 5,
    });
    const updateConversationMarkerSpy = vi
      .spyOn(api, "updateConversationMarker")
      .mockResolvedValue({
        marker: updatedMarker,
        revision: 6,
        serverInstanceId: "server-a",
        sessionMutationStamp: 6,
      });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleUpdateConversationMarker("session-1", "marker-1", {
        kind: "bug",
        name: "Bug marker",
        color: "#ef4444",
      }),
    ).resolves.toBe(true);

    expect(updateConversationMarkerSpy).toHaveBeenCalledWith(
      "session-1",
      "marker-1",
      {
        kind: "bug",
        name: "Bug marker",
        color: "#ef4444",
      },
    );
    expect(params.refs.sessionsRef.current[0].markers).toEqual([updatedMarker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(6);
    expect(params.refs.latestStateRevisionRef.current).toBe(6);
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("recovers stale same-instance marker update when local marker data does not match", async () => {
    const marker = makeConversationMarker({
      updatedAt: "2026-05-01 10:00:00",
    });
    const updatedMarker = makeConversationMarker({
      kind: "bug",
      name: "Bug marker",
      color: "#ef4444",
      updatedAt: "2026-05-01 10:01:00",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
      sessionMutationStamp: 7,
    });
    vi.spyOn(api, "updateConversationMarker").mockResolvedValue({
      marker: updatedMarker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleUpdateConversationMarker("session-1", "marker-1", {
        kind: "bug",
        name: "Bug marker",
        color: "#ef4444",
      }),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([marker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(7);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("accepts stale same-instance marker update when only valid color casing differs", async () => {
    const currentMarker = makeConversationMarker({
      kind: "bug",
      name: "Bug marker",
      color: "#EF4444",
      updatedAt: "2026-05-01 10:01:00",
    });
    const responseMarker = makeConversationMarker({
      kind: "bug",
      name: "Bug marker",
      color: "#ef4444",
      updatedAt: "2026-05-01 10:01:00",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [currentMarker],
      sessionMutationStamp: 7,
    });
    vi.spyOn(api, "updateConversationMarker").mockResolvedValue({
      marker: responseMarker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleUpdateConversationMarker("session-1", "marker-1", {
        kind: "bug",
        name: "Bug marker",
        color: "#ef4444",
      }),
    ).resolves.toBe(true);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([currentMarker]);
    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("recovers stale same-instance marker update when valid colors genuinely differ", async () => {
    const currentMarker = makeConversationMarker({
      kind: "bug",
      name: "Bug marker",
      color: "#EF4444",
      updatedAt: "2026-05-01 10:01:00",
    });
    const responseMarker = makeConversationMarker({
      kind: "bug",
      name: "Bug marker",
      color: "#3B82F6",
      updatedAt: "2026-05-01 10:01:00",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [currentMarker],
      sessionMutationStamp: 7,
    });
    vi.spyOn(api, "updateConversationMarker").mockResolvedValue({
      marker: responseMarker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleUpdateConversationMarker("session-1", "marker-1", {
        kind: "bug",
        name: "Bug marker",
        color: "#3B82F6",
      }),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([currentMarker]);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("recovers stale same-instance marker update when local color is invalid", async () => {
    const currentMarker = makeConversationMarker({
      color: "url(https://example.test/marker)",
      updatedAt: "2026-05-01 10:01:00",
    });
    const responseMarker = makeConversationMarker({
      color: "#3b82f6",
      updatedAt: "2026-05-01 10:01:00",
    });
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [currentMarker],
      sessionMutationStamp: 7,
    });
    vi.spyOn(api, "updateConversationMarker").mockResolvedValue({
      marker: responseMarker,
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleUpdateConversationMarker("session-1", "marker-1", {
        color: "#3b82f6",
      }),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([currentMarker]);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("deletes conversation markers through the guarded marker response path", async () => {
    const marker = makeConversationMarker();
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
      sessionMutationStamp: 5,
    });
    const deleteConversationMarkerSpy = vi
      .spyOn(api, "deleteConversationMarker")
      .mockResolvedValue({
        markerId: "marker-1",
        revision: 6,
        serverInstanceId: "server-a",
        sessionMutationStamp: 6,
      });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleDeleteConversationMarker("session-1", "marker-1"),
    ).resolves.toBe(true);

    expect(deleteConversationMarkerSpy).toHaveBeenCalledWith(
      "session-1",
      "marker-1",
    );
    expect(params.refs.sessionsRef.current[0].markers).toEqual([]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(6);
    expect(params.refs.latestStateRevisionRef.current).toBe(6);
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("treats stale same-instance marker delete success as a no-op when already absent", async () => {
    const marker = makeConversationMarker();
    const sessionAtClick = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
      sessionMutationStamp: 5,
    });
    const liveSession = {
      ...sessionAtClick,
      markers: [],
      sessionMutationStamp: 7,
    };
    vi.spyOn(api, "deleteConversationMarker").mockResolvedValue({
      markerId: "marker-1",
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[sessionAtClick.id, sessionAtClick]]);
    params.refs.sessionsRef.current = [liveSession];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleDeleteConversationMarker("session-1", "marker-1"),
    ).resolves.toBe(true);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(7);
    expect(params.refs.latestStateRevisionRef.current).toBe(7);
    expect(params.requestActionRecoveryResync).not.toHaveBeenCalled();
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expect(params.setters.setRequestError).toHaveBeenCalledWith(null);
  });

  it("recovers stale same-instance marker delete success when the marker is still present", async () => {
    const marker = makeConversationMarker();
    const sessionAtClick = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
      sessionMutationStamp: 5,
    });
    const liveSession = {
      ...sessionAtClick,
      markers: [marker],
      sessionMutationStamp: 7,
    };
    vi.spyOn(api, "deleteConversationMarker").mockResolvedValue({
      markerId: "marker-1",
      revision: 6,
      serverInstanceId: "server-a",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[sessionAtClick.id, sessionAtClick]]);
    params.refs.sessionsRef.current = [liveSession];
    params.refs.latestStateRevisionRef.current = 7;
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleDeleteConversationMarker("session-1", "marker-1"),
    ).resolves.toBe(false);

    expect(params.refs.sessionsRef.current[0].markers).toEqual([marker]);
    expect(params.refs.sessionsRef.current[0].sessionMutationStamp).toBe(7);
    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
    });
    expect(params.forceSseReconnect).not.toHaveBeenCalled();
    expectRequestErrorDeferredUpdatesOnly(params.setters.setRequestError);
  });

  it("forces SSE reconnect when marker delete success comes from a new server instance", async () => {
    const marker = makeConversationMarker();
    const session = makeSession("session-1", {
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          text: "Decision point",
          timestamp: "10:00",
        },
      ],
      markers: [marker],
    });
    vi.spyOn(api, "deleteConversationMarker").mockResolvedValue({
      markerId: "marker-1",
      revision: 6,
      serverInstanceId: "server-b",
      sessionMutationStamp: 6,
    });
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    const actions = useAppSessionActions(params);

    await expect(
      actions.handleDeleteConversationMarker("session-1", "marker-1"),
    ).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      openSessionId: "session-1",
      paneId: null,
      allowUnknownServerInstance: true,
      sseReconnectRequestId: 42,
    });
    expect(params.forceSseReconnect).toHaveBeenCalledTimes(1);
    expect(params.refs.sessionsRef.current[0].markers).toEqual([marker]);
    expect(params.setters.setRequestError).not.toHaveBeenCalledWith(null);
  });

  it("reads live sessions after stale same-instance settings success", async () => {
    const previousSession: TestSession = {
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
    const staleSession: TestSession = {
      ...previousSession,
      model: "gpt-small",
      reasoningEffort: "high",
      sessionMutationStamp: 2,
    };
    const liveSession: TestSession = {
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

  it("voids the state-diff baseline when a failed settings change rolls back", async () => {
    const session: TestSession = {
      ...makeSession("session-1"),
      reasoningEffort: "medium",
    };
    vi.spyOn(api, "updateSessionSettings").mockRejectedValue(new Error("rejected"));
    const params = makeSessionActionsParams();
    params.lookups.sessionLookup = new Map([[session.id, session]]);
    params.refs.sessionsRef.current = [session];
    params.refs.stateDiffBaselineRevisionRef.current = 5;
    const actions = useAppSessionActions(params);

    await actions.handleSessionSettingsChange("session-1", "reasoningEffort", "high");

    expect(params.refs.sessionsRef.current[0]?.reasoningEffort).toBe("medium");
    expect(params.refs.stateDiffBaselineRevisionRef.current).toBeNull();
    expect(params.reportRequestError).toHaveBeenCalled();
  });

  it("reads response sessions after adopted settings success", async () => {
    const previousSession: TestSession = {
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
    const responseSession: TestSession = {
      ...previousSession,
      model: "gpt-small",
      reasoningEffort: "low",
    };
    const staleLocalSession: TestSession = {
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
    params.refs.actionStateClassifierContextRef.current = {
      getSnapshot: () => ({
        revision: params.refs.latestStateRevisionRef.current,
        serverInstanceId: params.refs.lastSeenServerInstanceIdRef.current,
        projects: [project],
        sessions: params.refs.sessionsRef.current,
      }),
    };
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
    params.setters.setSelectedProjectId = setSelectedProjectId;
    const actions = useAppSessionActions(params);

    await expect(actions.handleCreateProject()).resolves.toBe(false);

    expect(params.requestActionRecoveryResync).toHaveBeenCalledWith({
      allowUnknownServerInstance: true,
    });
    expect(setSelectedProjectId).not.toHaveBeenCalled();
  });
});
