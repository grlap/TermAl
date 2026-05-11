import { waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import { useAppSessionActions } from "./app-session-actions";
import type { StateResponse } from "./api";
import type { AgentType, ConversationMarker, Project, Session } from "./types";
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
      defaultCodexModel: "default",
      defaultClaudeModel: "default",
      defaultCursorModel: "default",
      defaultGeminiModel: "default",
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
      defaultCodexModel: "default",
      defaultCodexReasoningEffort: "medium",
      defaultCodexSandboxMode: "workspace-write",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
      defaultClaudeModel: "default",
      defaultCursorModel: "default",
      defaultCursorMode: "agent",
      defaultGeminiApprovalMode: "default",
      defaultGeminiModel: "default",
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
  | "defaultGeminiModel";

const MODEL_PICKER_AGENT_CASES = [
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
] satisfies ReadonlyArray<{
  agent: AgentType;
  defaultModelKey: DefaultModelKey;
  customModel: string;
}>;

describe("useAppSessionActions", () => {
  afterEach(() => {
    vi.restoreAllMocks();
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

      await expect(
        actions.handleNewSession({ agent, model: "default" }),
      ).resolves.toBe(true);

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

      await expect(
        actions.handleNewSession({ agent, model: "default" }),
      ).resolves.toBe(true);

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
      });
      expect(params.forceSseReconnect).toHaveBeenCalledTimes(1);
    });
    params.refs.activePromptPollCancelRef.current?.();
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
    });
    expect(params.forceSseReconnect).toHaveBeenCalledTimes(1);
    expect(params.refs.sessionsRef.current[0].markers).toEqual([marker]);
    expect(params.setters.setRequestError).not.toHaveBeenCalledWith(null);
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
