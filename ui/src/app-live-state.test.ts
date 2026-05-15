import { act, render, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import {
  SESSION_HYDRATION_FIRST_RETRY_DELAY_MS,
  SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
  resolveAdoptStateSessionOptions,
  useAppLiveState,
  type SessionHydrationTarget,
  type UseAppLiveStateParams,
  type UseAppLiveStateReturn,
} from "./app-live-state";
import { RECONNECT_STATE_RESYNC_DELAY_MS } from "./app-shell-internals";
import {
  classifyFetchedSessionAdoption,
  hydrationRetainedMessagesMatch,
  hydrationSessionMetadataIsAhead,
  hydrationSessionMetadataMatches,
  type SessionHydrationRequestContext,
} from "./session-hydration-adoption";
import {
  getSessionRecordSnapshotForTesting,
  resetSessionStoreForTesting,
  upsertSessionStoreSession,
} from "./session-store";
import {
  SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
  SESSION_TAIL_WINDOW_MESSAGE_COUNT,
} from "./session-tail-policy";
import type { StateResponse } from "./api";
import type { DelegationSummary, Message, Session } from "./types";
import type { WorkspaceState } from "./workspace";

class EventSourceMock {
  static instances: EventSourceMock[] = [];

  onerror: ((event: Event) => void) | null = null;
  onopen: ((event: Event) => void) | null = null;
  readyState?: number;

  private listeners = new Map<
    string,
    Set<(event: MessageEvent<string>) => void>
  >();

  constructor() {
    EventSourceMock.instances.push(this);
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    const listeners =
      this.listeners.get(type) ??
      new Set<(event: MessageEvent<string>) => void>();
    listeners.add(normalizeMessageEventListener(listener));
    this.listeners.set(type, listeners);
  }

  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
  ) {
    this.listeners.get(type)?.delete(normalizeMessageEventListener(listener));
  }

  close() {}

  dispatchOpen() {
    this.readyState = 1;
    this.onopen?.(new Event("open"));
  }

  dispatchError() {
    this.onerror?.(new Event("error"));
  }

  dispatchNamedEvent(type: string, data: unknown) {
    const payload = typeof data === "string" ? data : JSON.stringify(data);
    const event = { data: payload } as MessageEvent<string>;
    this.listeners.get(type)?.forEach((listener) => {
      listener(event);
    });
  }
}

function normalizeMessageEventListener(
  listener: EventListenerOrEventListenerObject,
) {
  if (typeof listener === "function") {
    return listener as (event: MessageEvent<string>) => void;
  }

  return (event: MessageEvent<string>) => listener.handleEvent(event);
}

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "idle",
    preview: "",
    messages: [],
    messagesLoaded: false,
    ...overrides,
  };
}

function makeHydrationMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    author: index % 2 === 0 ? "you" : "assistant",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    text: `Message ${index + 1}`,
  }));
}

function makeDelegationSummary(
  overrides: Partial<DelegationSummary> = {},
): DelegationSummary {
  return {
    id: "delegation-1",
    parentSessionId: "session-1",
    childSessionId: "child-session",
    mode: "reviewer",
    status: "running",
    title: "Review",
    agent: "Codex",
    model: "codex",
    writePolicy: { kind: "readOnly" },
    createdAt: "10:00",
    startedAt: "10:00",
    completedAt: null,
    result: null,
    ...overrides,
  };
}

function makeDelegationDeltaCases(revision: number) {
  return [
    [
      "delegationCreated",
      () => ({
        type: "delegationCreated",
        revision,
        delegation: makeDelegationSummary(),
      }),
    ],
    [
      "delegationUpdated",
      () => ({
        type: "delegationUpdated",
        revision,
        delegationId: "delegation-1",
        status: "running",
        updatedAt: "10:01",
      }),
    ],
    [
      "delegationCompleted",
      () => ({
        type: "delegationCompleted",
        revision,
        delegationId: "delegation-1",
        completedAt: "10:02",
        result: {
          delegationId: "delegation-1",
          childSessionId: "child-session",
          status: "completed",
          summary: "Done.",
        },
      }),
    ],
    [
      "delegationFailed",
      () => ({
        type: "delegationFailed",
        revision,
        delegationId: "delegation-1",
        failedAt: "10:02",
        result: {
          delegationId: "delegation-1",
          childSessionId: "child-session",
          status: "failed",
          summary: "Failed.",
        },
      }),
    ],
    [
      "delegationCanceled",
      () => ({
        type: "delegationCanceled",
        revision,
        delegationId: "delegation-1",
        canceledAt: "10:03",
        reason: "Canceled.",
      }),
    ],
  ] as const;
}

function makeHydrationRequestContext(
  overrides: Partial<SessionHydrationRequestContext> = {},
): SessionHydrationRequestContext {
  return {
    messageCount: 1,
    revision: 5,
    serverInstanceId: "server-a",
    sessionMutationStamp: 1,
    ...overrides,
  };
}

function makeStateResponse(session: Session, revision = 2): StateResponse {
  return {
    revision,
    serverInstanceId: "server-a",
    codex: {},
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
    sessions: [session],
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

function makeCountingActionRecoveryRef(invocations: () => void) {
  let assigned: NonNullable<
    UseAppLiveStateParams["requestActionRecoveryResyncRef"]["current"]
  > = () => {};
  const wrapAssigned = (next: typeof assigned): typeof assigned =>
    ((...args: Parameters<typeof assigned>) => {
      invocations();
      return next(...args);
    }) as typeof assigned;
  let current = wrapAssigned(assigned);
  return {
    get current() {
      return current;
    },
    set current(next: typeof assigned) {
      assigned = next;
      current = wrapAssigned(assigned);
    },
  } as UseAppLiveStateParams["requestActionRecoveryResyncRef"];
}

function makeLiveStateParams(
  session: Session,
  actionRecoveryInvocations = vi.fn(),
): UseAppLiveStateParams {
  const noopSetter = vi.fn();
  return {
    adoptionRefs: {
      isMountedRef: { current: true },
      latestStateRevisionRef: { current: 1 },
      lastSeenServerInstanceIdRef: { current: "server-a" },
      seenServerInstanceIdsRef: { current: new Set(["server-a"]) },
      sessionsRef: { current: [session] },
      draftsBySessionIdRef: { current: {} },
      draftAttachmentsBySessionIdRef: { current: {} },
      codexStateRef: { current: {} },
      agentReadinessRef: { current: [] },
      projectsRef: { current: [] },
      orchestratorsRef: { current: [] },
      delegationWaitsRef: { current: [] },
      workspaceSummariesRef: { current: [] },
      refreshingAgentCommandSessionIdsRef: { current: {} },
      confirmedUnknownModelSendsRef: { current: new Set() },
      activePromptPollCancelRef: { current: null },
      activePromptPollSessionIdRef: { current: null },
    },
    stateSetters: {
      setSessions: noopSetter,
      setWorkspace: noopSetter,
      setCodexState: noopSetter,
      setAgentReadiness: noopSetter,
      setProjects: noopSetter,
      setOrchestrators: noopSetter,
      setDelegationWaits: noopSetter,
      setWorkspaceSummaries: noopSetter,
      setDraftsBySessionId: noopSetter,
      setDraftAttachmentsBySessionId: noopSetter,
      setSendingSessionIds: noopSetter,
      setStoppingSessionIds: noopSetter,
      setKillingSessionIds: noopSetter,
      setKillRevealSessionId: noopSetter,
      setPendingKillSessionId: noopSetter,
      setPendingSessionRename: noopSetter,
      setUpdatingSessionIds: noopSetter,
      setAgentCommandsBySessionId: noopSetter,
      setRefreshingAgentCommandSessionIds: noopSetter,
      setAgentCommandErrors: noopSetter,
      setSessionSettingNotices: noopSetter,
      setSelectedProjectId: noopSetter,
      setIsLoading: noopSetter,
      setBackendConnectionIssueDetail: noopSetter,
      setBackendConnectionState: noopSetter,
    },
    preferenceSetters: {
      setDefaultCodexModel: noopSetter,
      setDefaultClaudeModel: noopSetter,
      setDefaultCursorModel: noopSetter,
      setDefaultGeminiModel: noopSetter,
      setDefaultCodexReasoningEffort: noopSetter,
      setDefaultClaudeApprovalMode: noopSetter,
      setDefaultClaudeEffort: noopSetter,
      setRemoteConfigs: noopSetter,
    },
    applyControlPanelLayout: (workspace) => workspace,
    clearRecoveredBackendRequestError: vi.fn(),
    reportRequestError: vi.fn(),
    requestBackendReconnectRef: { current: vi.fn() },
    requestActionRecoveryResyncRef: makeCountingActionRecoveryRef(
      actionRecoveryInvocations,
    ),
    activeSession: session,
    visibleSessionHydrationTargets: [{ id: session.id, messagesLoaded: false }],
  } as UseAppLiveStateParams;
}

function renderLiveStateHarness(
  params: UseAppLiveStateParams,
  capture: (hook: UseAppLiveStateReturn) => void,
  getVisibleSessionHydrationTargets: () => readonly SessionHydrationTarget[] =
    () => [
      {
        id: params.activeSession?.id ?? "session-1",
        messagesLoaded: false,
      },
    ],
) {
  function Harness() {
    const hook = useAppLiveState({
      ...params,
      visibleSessionHydrationTargets: getVisibleSessionHydrationTargets(),
    });
    capture(hook);
    return null;
  }

  const rendered = render(createElement(Harness));
  return {
    ...rendered,
    rerenderLiveState() {
      rendered.rerender(createElement(Harness));
    },
  };
}

afterEach(() => {
  resetSessionStoreForTesting();
  EventSourceMock.instances = [];
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("deferred session-store sync", () => {
  it("clears reconnecting when valid session data arrives after an error without an open event", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const session = makeSession({
      messagesLoaded: true,
      messageCount: 1,
      preview: "Partial output.",
      messages: [
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Partial output.",
        },
      ],
    });
    const params = makeLiveStateParams(session);
    const setBackendConnectionState = vi.fn();
    params.stateSetters.setBackendConnectionState = setBackendConnectionState;
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchError();
    });
    expect(setBackendConnectionState).toHaveBeenCalledWith("reconnecting");

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "textReplace",
        revision: 3,
        sessionId: session.id,
        messageId: "message-assistant-1",
        messageIndex: 0,
        messageCount: 1,
        text: "Recovered live output.",
        preview: "Recovered live output.",
        sessionMutationStamp: 3,
      });
    });

    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3);
    expect(params.adoptionRefs.sessionsRef.current[0]?.preview).toBe(
      "Recovered live output.",
    );
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("connected");
  });

  it("clears reconnecting when automatic fallback adopts a newer idle snapshot", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const activeSession = makeSession({
      status: "active",
      messagesLoaded: true,
      messageCount: 1,
      preview: "Still working.",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "test",
        },
      ],
    });
    const idleSession = makeSession({
      status: "idle",
      messagesLoaded: true,
      messageCount: 2,
      preview: "Done.",
      messages: [
        ...activeSession.messages,
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Done.",
        },
      ],
      sessionMutationStamp: 2,
    });
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockResolvedValue(makeStateResponse(idleSession, 3));
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(activeSession);
    const setBackendConnectionState = vi.fn();
    params.stateSetters.setBackendConnectionState = setBackendConnectionState;
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [activeSession];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchError();
    });
    expect(setBackendConnectionState).toHaveBeenCalledWith("reconnecting");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.sessionsRef.current[0]?.status).toBe("idle");
    expect(params.adoptionRefs.sessionsRef.current[0]?.preview).toBe("Done.");
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("connected");
  });

  it("applies delegation wait create and consume deltas locally", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );

    const session = makeSession();
    const params = makeLiveStateParams(session);
    renderLiveStateHarness(params, () => {});
    const eventSource = EventSourceMock.instances[0];
    expect(eventSource).toBeDefined();

    act(() => {
      eventSource!.dispatchNamedEvent("delta", {
        type: "delegationWaitCreated",
        revision: 2,
        wait: {
          id: "wait-1",
          parentSessionId: session.id,
          delegationIds: ["delegation-1"],
          mode: "all",
          createdAt: "12:00:00",
          title: "Review",
        },
      });
    });

    expect(params.adoptionRefs.delegationWaitsRef.current).toEqual([
      {
        id: "wait-1",
        parentSessionId: session.id,
        delegationIds: ["delegation-1"],
        mode: "all",
        createdAt: "12:00:00",
        title: "Review",
      },
    ]);

    act(() => {
      eventSource!.dispatchNamedEvent("delta", {
        type: "delegationWaitConsumed",
        revision: 3,
        waitId: "wait-1",
        parentSessionId: session.id,
        reason: "completed",
      });
    });

    expect(params.adoptionRefs.delegationWaitsRef.current).toEqual([]);
  });

  it("prunes queued session ids that disappear before the pending frame flushes", async () => {
    let pendingFrame: FrameRequestCallback | null = null;
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        pendingFrame = callback;
        return 1;
      }),
    );
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn(() => {
        pendingFrame = null;
      }),
    );
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );

    const session = makeSession({
      messagesLoaded: true,
      messageCount: 0,
      sessionMutationStamp: 1,
    });
    upsertSessionStoreSession({
      session,
      committedDraft: "",
      draftAttachments: [],
    });
    const params = makeLiveStateParams(session);

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];
    expect(eventSource).toBeDefined();

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "messageCreated",
        revision: 2,
        sessionId: session.id,
        messageId: "message-1",
        messageIndex: 0,
        messageCount: 1,
        message: {
          id: "message-1",
          type: "text",
          author: "assistant",
          timestamp: "10:01",
          text: "Created",
        },
        preview: "Created",
        status: "idle",
        sessionMutationStamp: 2,
      });
    });

    expect(pendingFrame).not.toBeNull();
    params.adoptionRefs.sessionsRef.current = [];

    act(() => {
      pendingFrame?.(123);
    });

    expect(getSessionRecordSnapshotForTesting(session.id)).toBeNull();
  });
});

describe("delegation delta repair", () => {
  it.each(makeDelegationDeltaCases(2))("repairs equal-revision %s without session-delta hydration", async (_, makeDelta) => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const session = makeSession({
      messagesLoaded: true,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockResolvedValue(makeStateResponse(session, 2));
    const fetchSession = vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", makeDelta());
    });

    await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));
    expect(fetchSession).not.toHaveBeenCalled();
  });

  it.each(makeDelegationDeltaCases(1))(
    "ignores stale %s without state repair",
    async (_, makeDelta) => {
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      const session = makeSession({
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 5,
      });
      const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
        () => new Promise<StateResponse>(() => {}),
      );
      const fetchSession = vi.spyOn(api, "fetchSession").mockImplementation(
        () =>
          new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
      );
      const params = makeLiveStateParams(session);
      params.adoptionRefs.latestStateRevisionRef.current = 2;
      params.adoptionRefs.sessionsRef.current = [session];

      renderLiveStateHarness(params, () => {});
      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];

      act(() => {
        eventSource?.dispatchNamedEvent("delta", makeDelta());
      });

      await act(async () => {
        await Promise.resolve();
      });
      expect(fetchState).not.toHaveBeenCalled();
      expect(fetchSession).not.toHaveBeenCalled();
      expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(2);
    },
  );

  it.each(makeDelegationDeltaCases(3))(
    "repairs newer %s through authoritative state",
    async (_, makeDelta) => {
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      const session = makeSession({
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 2,
      });
      const fetchState = vi
        .spyOn(api, "fetchState")
        .mockResolvedValue(makeStateResponse(session, 3));
      const fetchSession = vi.spyOn(api, "fetchSession").mockImplementation(
        () =>
          new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
      );
      const params = makeLiveStateParams(session);
      params.adoptionRefs.latestStateRevisionRef.current = 2;
      params.adoptionRefs.sessionsRef.current = [session];

      renderLiveStateHarness(params, () => {});
      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];

      act(() => {
        eventSource?.dispatchNamedEvent("delta", makeDelta());
      });

      await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));
      expect(fetchSession).not.toHaveBeenCalled();
      await waitFor(() =>
        expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3),
      );
    },
  );

  it("replays same-revision parent-card deltas after the state snapshot revision is current", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const session = makeSession({
      messagesLoaded: false,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "messageCreated",
        revision: 2,
        sessionId: session.id,
        messageId: "parent-card",
        messageIndex: 0,
        messageCount: 1,
        message: {
          id: "parent-card",
          type: "parallelAgents",
          author: "assistant",
          timestamp: "10:01",
          agents: [
            {
              id: "delegation-1",
              source: "delegation",
              title: "Review",
              status: "running",
              summary: "Reviewing",
            },
          ],
        },
        preview: "Reviewing",
        status: "idle",
        sessionMutationStamp: 2,
      });
    });

    const updated = params.adoptionRefs.sessionsRef.current[0];
    expect(updated.messages).toHaveLength(1);
    expect(updated.messages[0]?.id).toBe("parent-card");
    expect(updated.messagesLoaded).toBe(true);
    expect(fetchState).not.toHaveBeenCalled();
  });

  it("keeps delegation repair pending while same-revision sibling session deltas apply", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const parentSession = makeSession({
      messagesLoaded: false,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    let resolveRepair!: (state: StateResponse) => void;
    const repair = new Promise<StateResponse>((resolve) => {
      resolveRepair = resolve;
    });
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
      () => repair,
    );
    const fetchSession = vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(parentSession);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [parentSession];

    renderLiveStateHarness(params, () => {}, () => [
      { id: parentSession.id, messagesLoaded: true },
    ]);
    fetchSession.mockClear();
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationCreated",
        revision: 2,
        delegation: makeDelegationSummary({
          parentSessionId: parentSession.id,
        }),
      });
    });

    await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "sessionCreated",
        revision: 2,
        sessionId: "child-session",
        session: makeSession({
          id: "child-session",
          name: "Delegation child",
          messagesLoaded: true,
          messageCount: 0,
          sessionMutationStamp: 1,
        }),
      });
      eventSource?.dispatchNamedEvent("delta", {
        type: "messageCreated",
        revision: 2,
        sessionId: parentSession.id,
        messageId: "parent-card",
        messageIndex: 0,
        messageCount: 1,
        message: {
          id: "parent-card",
          type: "parallelAgents",
          author: "assistant",
          timestamp: "10:01",
          agents: [
            {
              id: "delegation-1",
              source: "delegation",
              title: "Review",
              status: "running",
              summary: "Reviewing",
            },
          ],
        },
        preview: "Reviewing",
        status: "idle",
        sessionMutationStamp: 2,
      });
    });

    expect(fetchSession).not.toHaveBeenCalled();
    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(
      params.adoptionRefs.sessionsRef.current.some(
        (session) => session.id === "child-session",
      ),
    ).toBe(true);
    const updatedParent = params.adoptionRefs.sessionsRef.current.find(
      (session) => session.id === parentSession.id,
    );
    expect(updatedParent?.messages[0]?.id).toBe("parent-card");

    await act(async () => {
      resolveRepair({
        ...makeStateResponse(
          {
            ...parentSession,
            messages: updatedParent?.messages ?? [],
            messagesLoaded: true,
            messageCount: 1,
          },
          2,
        ),
        projects: [
          {
            id: "project-repaired",
            name: "Repaired Project",
            rootPath: "C:/workspace",
          },
        ],
        delegations: [
          makeDelegationSummary({
            parentSessionId: parentSession.id,
          }),
        ],
      });
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(params.adoptionRefs.projectsRef.current[0]?.id).toBe(
        "project-repaired",
      ),
    );
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(2);
  });

  it("retries delegation repair after a transient state fetch failure", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const session = makeSession({
      messagesLoaded: true,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    let fetchStateCallCount = 0;
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => {
      fetchStateCallCount += 1;
      if (fetchStateCallCount === 1) {
        return Promise.reject(new Error("transient state repair failure"));
      }
      return Promise.resolve({
        ...makeStateResponse(session, 3),
        projects: [
          {
            id: "project-after-retry",
            name: "Project After Retry",
            rootPath: "C:/workspace",
          },
        ],
      });
    });
    const fetchSession = vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationCreated",
        revision: 3,
        delegation: makeDelegationSummary({
          parentSessionId: session.id,
        }),
      });
    });

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(2);
    expect(params.adoptionRefs.projectsRef.current[0]?.id).toBe(
      "project-after-retry",
    );
    expect(fetchSession).not.toHaveBeenCalled();
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3);
  });

  it("keeps delegation repair pending when a newer delta lands before adoption", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const session = makeSession({
      preview: "parent card already applied",
      messagesLoaded: true,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    let resolveFirstRepair!: (state: StateResponse) => void;
    let resolveSecondRepair!: (state: StateResponse) => void;
    const firstRepair = new Promise<StateResponse>((resolve) => {
      resolveFirstRepair = resolve;
    });
    const secondRepair = new Promise<StateResponse>((resolve) => {
      resolveSecondRepair = resolve;
    });
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockImplementationOnce(() => firstRepair)
      .mockImplementationOnce(() => secondRepair);
    const repairedState = {
        ...makeStateResponse(session, 2),
        projects: [
          {
            id: "project-repaired",
            name: "Repaired Project",
            rootPath: "C:/workspace",
          },
        ],
      };
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];
    expect(eventSource).toBeDefined();

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationCreated",
        revision: 2,
        delegation: makeDelegationSummary({
          parentSessionId: session.id,
        }),
      });
    });

    await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));
    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "codexUpdated",
        revision: 3,
        codex: {},
      });
    });
    await act(async () => {
      resolveFirstRepair(repairedState);
      await Promise.resolve();
    });
    await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(2));
    await act(async () => {
      resolveSecondRepair({
        ...repairedState,
        revision: 3,
      });
      await Promise.resolve();
    });
    await waitFor(() =>
      expect(params.adoptionRefs.projectsRef.current[0]?.id).toBe(
        "project-repaired",
      ),
    );
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3);
  });

  it("keeps reconnect recovery armed after delegation repair until a later live event", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const session = makeSession({
      messagesLoaded: true,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    const repairedState = {
      ...makeStateResponse(session, 3),
      projects: [
        {
          id: "project-after-delegation-repair",
          name: "Project After Delegation Repair",
          rootPath: "C:/workspace",
        },
      ],
      delegations: [
        makeDelegationSummary({
          parentSessionId: session.id,
        }),
      ],
    };
    const laterRepairedState = {
      ...repairedState,
      revision: 4,
      delegations: [
        makeDelegationSummary({
          parentSessionId: session.id,
        }),
      ],
    };
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockResolvedValueOnce(repairedState)
      .mockResolvedValueOnce(repairedState)
      .mockResolvedValue(laterRepairedState);
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchError();
      eventSource?.dispatchOpen();
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationCreated",
        revision: 3,
        delegation: makeDelegationSummary({
          parentSessionId: session.id,
        }),
      });
    });

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.projectsRef.current[0]?.id).toBe(
      "project-after-delegation-repair",
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(2);

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationUpdated",
        revision: 4,
        delegationId: "delegation-1",
        status: "running",
        updatedAt: "10:02",
      });
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(3);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(fetchState).toHaveBeenCalledTimes(3);
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(4);
  });

  it("confirms bad-live-event recovery after delegation repair and later delegation traffic", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const session = makeSession({
      messagesLoaded: true,
      messageCount: 1,
      sessionMutationStamp: 2,
    });
    const repairedState = {
      ...makeStateResponse(session, 3),
      delegations: [
        makeDelegationSummary({
          parentSessionId: session.id,
        }),
      ],
    };
    const laterRepairedState = {
      ...repairedState,
      revision: 4,
    };
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockResolvedValueOnce(repairedState)
      .mockResolvedValue(laterRepairedState);
    vi.spyOn(api, "fetchSession").mockImplementation(
      () => new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(() => {}),
    );
    const params = makeLiveStateParams(session);
    const setBackendConnectionState = vi.fn();
    params.stateSetters.setBackendConnectionState = setBackendConnectionState;
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {});
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchError();
      eventSource?.dispatchOpen();
      eventSource?.dispatchNamedEvent("delta", "{");
    });
    expect(setBackendConnectionState).toHaveBeenCalledWith("reconnecting");

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationCreated",
        revision: 3,
        delegation: makeDelegationSummary({
          parentSessionId: session.id,
        }),
      });
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3);
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("reconnecting");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(2);
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(4);
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("reconnecting");

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "delegationUpdated",
        revision: 4,
        delegationId: "delegation-1",
        status: "running",
        updatedAt: "10:02",
      });
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchState).toHaveBeenCalledTimes(3);
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(4);
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("connected");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 4);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(fetchState).toHaveBeenCalledTimes(3);
  });
});

describe("hydration mismatch recovery gate", () => {
  it("suppresses repeated recovery resyncs until authoritative state clears the mismatch", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi.spyOn(api, "fetchSession").mockResolvedValue({
      revision: 2,
      serverInstanceId: "server-a",
      session: makeSession({ id: "unexpected-session", messagesLoaded: true }),
    });
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(
      makeSession(),
      actionRecoveryInvocations,
    );

    const harness = renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1),
    );

    harness.rerenderLiveState();

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(2));
    expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1);
  });

  it("suppresses repeated tail mismatch recovery resyncs", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const messages = makeHydrationMessages(150);
    const fetchSessionTail = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        id: "session-2",
        messages,
        messagesLoaded: true,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      }),
    });
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const actionRecoveryInvocations = vi.fn();
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(
      initialSession,
      actionRecoveryInvocations,
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    const harness = renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1),
    );
    expect(fetchSession).not.toHaveBeenCalled();

    harness.rerenderLiveState();

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledTimes(2));
    expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1);
    expect(fetchSession).not.toHaveBeenCalled();
  });

  it("allows a later recovery resync after authoritative state adoption clears the mismatch", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi.spyOn(api, "fetchSession").mockResolvedValue({
      revision: 2,
      serverInstanceId: "server-a",
      session: makeSession({ id: "unexpected-session", messagesLoaded: true }),
    });
    const actionRecoveryInvocations = vi.fn();
    const session = makeSession();
    let hook: UseAppLiveStateReturn | null = null;
    const harness = renderLiveStateHarness(
      makeLiveStateParams(session, actionRecoveryInvocations),
      (nextHook) => {
        hook = nextHook;
      },
    );

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1),
    );

    act(() => {
      hook?.adoptState(makeStateResponse(session, 2));
    });
    harness.rerenderLiveState();

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(actionRecoveryInvocations).toHaveBeenCalledTimes(2),
    );
  });
});

describe("hydration adoption side effects", () => {
  it("requests action recovery and an authoritative state resync when fetched metadata is ahead", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi.spyOn(api, "fetchSession").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messagesLoaded: true,
        messageCount: 2,
        sessionMutationStamp: 2,
        messages: [
          {
            id: "message-1",
            type: "text",
            author: "assistant",
            timestamp: "10:00",
            text: "One",
          },
          {
            id: "message-2",
            type: "text",
            author: "assistant",
            timestamp: "10:01",
            text: "Two",
          },
        ],
      }),
    });
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(
      makeSession({
        messageCount: 1,
        sessionMutationStamp: 1,
      }),
      actionRecoveryInvocations,
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1),
    );
    await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));
  });

  it("preserves restart recovery while offline and allows replacement-instance adoption on the next manual retry", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const onlineSpy = vi
      .spyOn(window.navigator, "onLine", "get")
      .mockReturnValue(false);
    const replacementSession = makeSession({
      messagesLoaded: false,
      messageCount: 1,
      sessionMutationStamp: 1,
    });
    const replacementState = {
      ...makeStateResponse(replacementSession, 1),
      serverInstanceId: "server-b",
    };
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockResolvedValue(replacementState);
    const fetchSession = vi.spyOn(api, "fetchSession").mockResolvedValue({
      revision: 1,
      serverInstanceId: "server-b",
      session: {
        ...replacementSession,
        messagesLoaded: true,
      },
    });
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(
      makeSession({
        messageCount: 1,
        sessionMutationStamp: 1,
      }),
      actionRecoveryInvocations,
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1),
    );
    expect(fetchState).not.toHaveBeenCalled();

    onlineSpy.mockReturnValue(true);
    act(() => {
      params.requestActionRecoveryResyncRef.current();
    });

    await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(params.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe(
        "server-b",
      ),
    );
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(1);
  });

  it("retries stale fetched sessions without scheduling action recovery", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi.spyOn(api, "fetchSession").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 1,
      }),
    });
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(
      makeSession({
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 1,
      }),
      actionRecoveryInvocations,
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;

    renderLiveStateHarness(params, () => {});

    await act(async () => {
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(1);

    await act(async () => {
      // Advance the first targeted hydration retry after the initial fetch
      // returns a still-stale metadata summary.
      vi.advanceTimersByTime(SESSION_HYDRATION_FIRST_RETRY_DELAY_MS);
      await Promise.resolve();
    });

    expect(fetchSession).toHaveBeenCalledTimes(2);
    expect(actionRecoveryInvocations).not.toHaveBeenCalled();
    expect(fetchState).not.toHaveBeenCalled();
  });

  it("recovers from a transient non-404 hydration failure on the targeted retry", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const hydratedMessages = makeHydrationMessages(1);
    const transientFailure = new Error("temporary session fetch failure");
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockRejectedValueOnce(transientFailure)
      .mockResolvedValueOnce({
        revision: 5,
        serverInstanceId: "server-a",
        session: makeSession({
          messages: hydratedMessages,
          messagesLoaded: true,
          messageCount: hydratedMessages.length,
          sessionMutationStamp: 1,
        }),
      });
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(
      makeSession({
        messagesLoaded: false,
        messageCount: hydratedMessages.length,
        sessionMutationStamp: 1,
      }),
      actionRecoveryInvocations,
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;

    renderLiveStateHarness(params, () => {});

    await act(async () => {
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(1);
    expect(params.reportRequestError).toHaveBeenCalledWith(transientFailure);

    await act(async () => {
      vi.advanceTimersByTime(SESSION_HYDRATION_FIRST_RETRY_DELAY_MS);
      await Promise.resolve();
    });

    expect(fetchSession).toHaveBeenCalledTimes(2);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(true);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      hydratedMessages,
    );
    expect(actionRecoveryInvocations).not.toHaveBeenCalled();
  });

  it("caps automatic hydration retries for persistent non-404 failures", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockRejectedValue(new Error("persistent session fetch failure"));
    const params = makeLiveStateParams(
      makeSession({
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 1,
      }),
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;

    renderLiveStateHarness(params, () => {});

    await act(async () => {
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(1);

    for (let attempt = 0; attempt < SESSION_HYDRATION_MAX_RETRY_ATTEMPTS; attempt += 1) {
      await act(async () => {
        vi.advanceTimersByTime(3_000);
        await Promise.resolve();
      });
    }

    expect(fetchSession).toHaveBeenCalledTimes(
      1 + SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
    );

    await act(async () => {
      vi.advanceTimersByTime(30_000);
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(
      1 + SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
    );
  });

  it("keeps retrying metadata-only hydration responses past the error retry cap", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const hydratedMessages = makeHydrationMessages(1);
    let fetchCount = 0;
    const fetchSession = vi.spyOn(api, "fetchSession").mockImplementation(async () => {
      fetchCount += 1;
      const isStillMetadataOnly =
        fetchCount <= 1 + SESSION_HYDRATION_MAX_RETRY_ATTEMPTS;
      return {
        revision: 5,
        serverInstanceId: "server-a",
        session: makeSession(
          isStillMetadataOnly
            ? {
                messagesLoaded: false,
                messageCount: hydratedMessages.length,
                sessionMutationStamp: 1,
              }
            : {
                messages: hydratedMessages,
                messagesLoaded: true,
                messageCount: hydratedMessages.length,
                sessionMutationStamp: 1,
              },
        ),
      };
    });
    const params = makeLiveStateParams(
      makeSession({
        messagesLoaded: false,
        messageCount: hydratedMessages.length,
        sessionMutationStamp: 1,
      }),
    );
    params.adoptionRefs.latestStateRevisionRef.current = 5;

    renderLiveStateHarness(params, () => {});

    await act(async () => {
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(1);

    for (
      let attempt = 0;
      attempt < SESSION_HYDRATION_MAX_RETRY_ATTEMPTS + 1;
      attempt += 1
    ) {
      await act(async () => {
        vi.advanceTimersByTime(3_000);
        await Promise.resolve();
      });
    }

    expect(fetchSession).toHaveBeenCalledTimes(
      2 + SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(true);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      hydratedMessages,
    );
  });

  it("does not retry when a fully-loaded hydration response is stale due to local SSE skew", async () => {
    // Regression for the runaway `/api/sessions/{id}` retry loop
    // observed during active streaming Codex turns. The response
    // carries `messagesLoaded: true` (the backend has the full
    // transcript), but the local view has already advanced past
    // the response's revision / message_count / mutation_stamp
    // because SSE deltas arrived during the round-trip. The
    // classifier returns "stale" — the response is older than
    // local state, so adopting would be a downgrade. Retrying is
    // futile (the SSE stream is faster than REST, every retry
    // races and loses the same way), so the retry timer must NOT
    // arm. See bugs.md "Hydration retry loop can spam persistent
    // failures" and the inline comment on the `case "stale":` arm
    // in `useAppLiveState`.
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    // Local view: target session is hydrated (messagesLoaded = true)
    // with up-to-date metadata (messageCount = 5, mutation stamp 5).
    const targetSession = makeSession({
      id: "session-target",
      messagesLoaded: true,
      messageCount: 5,
      sessionMutationStamp: 5,
    });
    // The active session is a DIFFERENT session so the hydration
    // useEffect doesn't claim `session-target`'s `messagesLoaded`
    // value from `activeSession` (which would short-circuit and
    // skip the visibleSessionHydrationTargets entry below).
    const activeSession = makeSession({
      id: "session-active",
      messagesLoaded: true,
      messageCount: 0,
      sessionMutationStamp: 1,
    });
    // Response: fully loaded (`messagesLoaded: true`) but lagging
    // local — `revision: 3` < `latestStateRevisionRef: 5`,
    // `messageCount: 3` < `currentSession.messageCount: 5`. Models
    // the streaming-skew race: SSE deltas advanced local past this
    // response during the round-trip. Classifier returns "stale"
    // via the revision-downgrade rejection.
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockResolvedValue({
        revision: 3,
        serverInstanceId: "server-a",
        session: makeSession({
          id: "session-target",
          messagesLoaded: true,
          messageCount: 3,
          sessionMutationStamp: 3,
        }),
      });
    const params = makeLiveStateParams(activeSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [activeSession, targetSession];

    renderLiveStateHarness(params, () => {}, () => [
      { id: "session-target", messagesLoaded: false },
    ]);

    await act(async () => {
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(1);
    expect(fetchSession).toHaveBeenCalledWith("session-target");

    // Advance well past every retry-delay tier
    // (50 / 250 / 1000 / 3000 ms) to prove no retry is scheduled.
    await act(async () => {
      vi.advanceTimersByTime(10_000);
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(1);
  });

  it("retries only the stale session instead of rerunning every visible hydration target", async () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(async (sessionId) => ({
        revision: 5,
        serverInstanceId: "server-a",
        session:
          sessionId === "session-1"
            ? makeSession({
                id: "session-1",
                messagesLoaded: false,
                messageCount: 1,
                sessionMutationStamp: 1,
              })
            : makeSession({
                id: "session-2",
                messagesLoaded: true,
                messageCount: 0,
                sessionMutationStamp: 1,
              }),
      }));
    const session1 = makeSession({
      id: "session-1",
      messagesLoaded: false,
      messageCount: 1,
      sessionMutationStamp: 1,
    });
    const session2 = makeSession({
      id: "session-2",
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(session1);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [session1, session2];

    renderLiveStateHarness(
      params,
      () => {},
      () => [
        { id: "session-1", messagesLoaded: false },
        { id: "session-2", messagesLoaded: false },
      ],
    );

    await act(async () => {
      await Promise.resolve();
    });
    expect(fetchSession).toHaveBeenCalledTimes(2);

    await act(async () => {
      // Only session-1 is still stale, so only its retry timer should fire.
      vi.advanceTimersByTime(SESSION_HYDRATION_FIRST_RETRY_DELAY_MS);
      await Promise.resolve();
    });

    expect(fetchSession).toHaveBeenCalledTimes(3);
    expect(fetchSession.mock.calls.map(([sessionId]) => sessionId)).toEqual([
      "session-1",
      "session-2",
      "session-1",
    ]);
  });

  it("adopts a large-session tail before fetching the full transcript", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(150);
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const tailSession = makeSession({
      messages: messages.slice(-20),
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const fullSession = makeSession({
      messages,
      messagesLoaded: true,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });

    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: tailSession,
    });
    let resolveFullHydration!: (
      response: Awaited<ReturnType<typeof api.fetchSession>>,
    ) => void;
    const fullHydration = new Promise<Awaited<ReturnType<typeof api.fetchSession>>>(
      (resolve) => {
        resolveFullHydration = resolve;
      },
    );
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => fullHydration);
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(api.fetchSessionTail).toHaveBeenCalledTimes(1));
    expect(api.fetchSessionTail).toHaveBeenCalledWith(
      "session-1",
      SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toHaveLength(
        SESSION_TAIL_WINDOW_MESSAGE_COUNT,
      ),
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messageCount).toBe(150);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages[0]?.id).toBe(
      "message-131",
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
      false,
    );
    expect(fetchSession).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolveFullHydration({
        revision: 5,
        serverInstanceId: "server-a",
        session: fullSession,
      });
      await fullHydration;
    });

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
        true,
      ),
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toHaveLength(
      150,
    );
  });

  it("short-circuits tail-first hydration when the tail response is fully loaded", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(150);
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const fullTailSession = makeSession({
      messages,
      messagesLoaded: true,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });

    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: fullTailSession,
    });
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(api.fetchSessionTail).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
        true,
      ),
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toHaveLength(
      150,
    );
    expect(fetchSession).not.toHaveBeenCalled();
  });

  it.each([
    [
      "restartResync",
      {
        revision: 5,
        serverInstanceId: "server-b",
        session: (messages: Message[]) =>
          makeSession({
            messages: messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT),
            messagesLoaded: false,
            messageCount: messages.length,
            sessionMutationStamp: 1,
          }),
      },
    ],
    [
      "stateResync",
      {
        revision: 5,
        serverInstanceId: "server-a",
        session: (messages: Message[]) =>
          makeSession({
            messages: messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT),
            messagesLoaded: false,
            messageCount: messages.length + 1,
            sessionMutationStamp: 2,
          }),
      },
    ],
  ])(
    "requests action recovery when tail-first hydration returns %s",
    async (_, response) => {
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
        () => new Promise<StateResponse>(() => {}),
      );

      const messages = makeHydrationMessages(150);
      const initialSession = makeSession({
        messagesLoaded: false,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      });
      vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
        revision: response.revision,
        serverInstanceId: response.serverInstanceId,
        session: response.session(messages),
      });
      const fetchSession = vi
        .spyOn(api, "fetchSession")
        .mockImplementation(() => new Promise(() => {}));
      const actionRecoveryInvocations = vi.fn();
      const params = makeLiveStateParams(
        initialSession,
        actionRecoveryInvocations,
      );
      params.adoptionRefs.latestStateRevisionRef.current = 5;
      params.adoptionRefs.sessionsRef.current = [initialSession];

      renderLiveStateHarness(params, () => {});

      await waitFor(() => expect(api.fetchSessionTail).toHaveBeenCalledTimes(1));
      await waitFor(() =>
        expect(actionRecoveryInvocations).toHaveBeenCalledTimes(1),
      );
      await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(1));
      expect(fetchSession).not.toHaveBeenCalled();
    },
  );

  it("falls through to full hydration after a stale tail response", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(150);
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 4,
      serverInstanceId: "server-a",
      session: makeSession({
        messages: messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT),
        messagesLoaded: false,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      }),
    });
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(initialSession, actionRecoveryInvocations);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(api.fetchSessionTail).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    expect(actionRecoveryInvocations).not.toHaveBeenCalled();
    expect(fetchState).not.toHaveBeenCalled();
  });

  it("skips full hydration when another path completes during partial tail adoption", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(150);
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messages: messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT),
        messagesLoaded: false,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      }),
    });
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];
    params.stateSetters.setSessions = vi.fn((nextSessions: Session[]) => {
      params.adoptionRefs.sessionsRef.current = nextSessions.map((session) =>
        session.id === initialSession.id
          ? {
              ...session,
              messages,
              messagesLoaded: true,
              messageCount: messages.length,
              sessionMutationStamp: 1,
            }
          : session,
      );
    }) as typeof params.stateSetters.setSessions;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    await waitFor(() => expect(api.fetchSessionTail).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(hook?.hydratedSessionIdsRef.current.has("session-1")).toBe(true),
    );
    expect(fetchSession).not.toHaveBeenCalled();
  });

  it("refetches after a missing-prefix delta races with tail-then-full hydration", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(150);
    const updatedMessages = messages.map((message) =>
      message.id === "message-30"
        ? { ...message, text: "Message 30 updated" }
        : message,
    );
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const tailSession = makeSession({
      messages: messages.slice(-20),
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const staleFullSession = makeSession({
      messages,
      messagesLoaded: true,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const updatedFullSession = makeSession({
      messages: updatedMessages,
      messagesLoaded: true,
      messageCount: updatedMessages.length,
      sessionMutationStamp: 2,
    });

    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: tailSession,
    });
    let resolveStaleFullHydration!: (
      response: Awaited<ReturnType<typeof api.fetchSession>>,
    ) => void;
    const staleFullHydration = new Promise<
      Awaited<ReturnType<typeof api.fetchSession>>
    >((resolve) => {
      resolveStaleFullHydration = resolve;
    });
    let resolveUpdatedFullHydration!: (
      response: Awaited<ReturnType<typeof api.fetchSession>>,
    ) => void;
    const updatedFullHydration = new Promise<
      Awaited<ReturnType<typeof api.fetchSession>>
    >((resolve) => {
      resolveUpdatedFullHydration = resolve;
    });
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementationOnce(() => staleFullHydration)
      .mockImplementationOnce(() => updatedFullHydration);
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages[0]?.id).toBe(
        "message-131",
      ),
    );
    expect(fetchSession).toHaveBeenCalledTimes(1);
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "messageUpdated",
        revision: 6,
        sessionId: "session-1",
        messageId: "message-30",
        messageIndex: 29,
        messageCount: updatedMessages.length,
        message: updatedMessages[29],
        preview: "Message 30 updated",
        status: "idle",
        sessionMutationStamp: 2,
      });
    });

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.sessionMutationStamp).toBe(
        2,
      ),
    );
    expect(fetchSession).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolveStaleFullHydration({
        revision: 5,
        serverInstanceId: "server-a",
        session: staleFullSession,
      });
      await staleFullHydration;
    });

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(2));

    await act(async () => {
      resolveUpdatedFullHydration({
        revision: 6,
        serverInstanceId: "server-a",
        session: updatedFullSession,
      });
      await updatedFullHydration;
    });

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
        true,
      ),
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages[29]).toMatchObject(
      {
        id: "message-30",
        text: "Message 30 updated",
      },
    );
  });

  it("preserves a tail-window delta across tail-then-full hydration retry", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(150);
    const updatedMessages = messages.map((message) =>
      message.id === "message-145"
        ? { ...message, text: "Message 145 updated" }
        : message,
    );
    const initialSession = makeSession({
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const tailSession = makeSession({
      messages: messages.slice(-20),
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const updatedFullSession = makeSession({
      messages: updatedMessages,
      messagesLoaded: true,
      messageCount: updatedMessages.length,
      sessionMutationStamp: 2,
    });

    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: tailSession,
    });
    let resolveFirstFullHydration!: (
      response: Awaited<ReturnType<typeof api.fetchSession>>,
    ) => void;
    const firstFullHydration = new Promise<
      Awaited<ReturnType<typeof api.fetchSession>>
    >((resolve) => {
      resolveFirstFullHydration = resolve;
    });
    let resolveRetriedFullHydration!: (
      response: Awaited<ReturnType<typeof api.fetchSession>>,
    ) => void;
    const retriedFullHydration = new Promise<
      Awaited<ReturnType<typeof api.fetchSession>>
    >((resolve) => {
      resolveRetriedFullHydration = resolve;
    });
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementationOnce(() => firstFullHydration)
      .mockImplementationOnce(() => retriedFullHydration);
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages[0]?.id).toBe(
        "message-131",
      ),
    );
    expect(fetchSession).toHaveBeenCalledTimes(1);
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "messageUpdated",
        revision: 6,
        sessionId: "session-1",
        messageId: "message-145",
        messageIndex: 144,
        messageCount: updatedMessages.length,
        message: updatedMessages[144],
        preview: "Message 145 updated",
        status: "idle",
        sessionMutationStamp: 2,
      });
    });

    await waitFor(() =>
      expect(
        params.adoptionRefs.sessionsRef.current[0]?.messages.find(
          (message) => message.id === "message-145",
        ),
      ).toMatchObject({
        id: "message-145",
        text: "Message 145 updated",
      }),
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
      false,
    );

    await act(async () => {
      resolveFirstFullHydration({
        revision: 6,
        serverInstanceId: "server-a",
        session: updatedFullSession,
      });
      await firstFullHydration;
    });

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(2));

    await act(async () => {
      resolveRetriedFullHydration({
        revision: 6,
        serverInstanceId: "server-a",
        session: updatedFullSession,
      });
      await retriedFullHydration;
    });

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
        true,
      ),
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages[144]).toMatchObject(
      {
        id: "message-145",
        text: "Message 145 updated",
      },
    );
  });

  it.each([
    [
      "the target session is missing",
      {
        sessionsRef: [] as Session[],
        expectFullFetch: false,
      },
    ],
    [
      "the current session is already loaded",
      {
        sessionsRef: [
          makeSession({
            messagesLoaded: true,
            messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
            sessionMutationStamp: 1,
          }),
        ],
        expectFullFetch: true,
      },
    ],
    [
      "the current session already has retained messages",
      {
        sessionsRef: [
          makeSession({
            messages: makeHydrationMessages(1),
            messagesLoaded: false,
            messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
            sessionMutationStamp: 1,
          }),
        ],
        expectFullFetch: true,
      },
    ],
    [
      "the current session is below the tail-first threshold",
      {
        sessionsRef: [
          makeSession({
            messagesLoaded: false,
            messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES - 1,
            sessionMutationStamp: 1,
          }),
        ],
        expectFullFetch: true,
      },
    ],
  ])("skips tail-first hydration when %s", async (_, scenario) => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSessionTail = vi
      .spyOn(api, "fetchSessionTail")
      .mockImplementation(() => new Promise(() => {}));
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const triggerSession = makeSession({
      messagesLoaded: false,
      messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(triggerSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = scenario.sessionsRef;

    renderLiveStateHarness(params, () => {});

    if (scenario.expectFullFetch) {
      await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    } else {
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(fetchSession).not.toHaveBeenCalled();
    }
    expect(fetchSessionTail).not.toHaveBeenCalled();
  });

  it("skips tail-first hydration for divergent text-repair hydration", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSessionTail = vi
      .spyOn(api, "fetchSessionTail")
      .mockImplementation(() => new Promise(() => {}));
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const messages = makeHydrationMessages(1);
    const session = makeSession({
      messages,
      messagesLoaded: true,
      messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [session];

    renderLiveStateHarness(params, () => {}, () => [
      { id: session.id, messagesLoaded: true },
    ]);
    const eventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      eventSource?.dispatchNamedEvent("delta", {
        type: "textDelta",
        revision: 7,
        sessionId: session.id,
        messageId: "message-1",
        messageIndex: 0,
        messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
        delta: " repaired",
        preview: "Message 1 repaired",
        sessionMutationStamp: 2,
      });
    });

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    expect(fetchSessionTail).not.toHaveBeenCalled();
  });

  it("starts tail-first hydration only at the large-session threshold", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const fetchSession = vi
      .spyOn(api, "fetchSession")
      .mockImplementation(() => new Promise(() => {}));
    const fetchSessionTail = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messagesLoaded: false,
        messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
      }),
    });

    const belowThresholdSession = makeSession({
      messagesLoaded: false,
      messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES - 1,
      sessionMutationStamp: 1,
    });
    const belowThresholdParams = makeLiveStateParams(belowThresholdSession);
    belowThresholdParams.adoptionRefs.latestStateRevisionRef.current = 5;
    belowThresholdParams.adoptionRefs.sessionsRef.current = [
      belowThresholdSession,
    ];
    const belowThresholdHarness = renderLiveStateHarness(
      belowThresholdParams,
      () => {},
    );

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    expect(fetchSessionTail).not.toHaveBeenCalled();

    belowThresholdHarness.unmount();
    fetchSession.mockClear();

    const thresholdSession = makeSession({
      messagesLoaded: false,
      messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
      sessionMutationStamp: 1,
    });
    const thresholdParams = makeLiveStateParams(thresholdSession);
    thresholdParams.adoptionRefs.latestStateRevisionRef.current = 5;
    thresholdParams.adoptionRefs.sessionsRef.current = [thresholdSession];

    const thresholdHarness = renderLiveStateHarness(thresholdParams, () => {});

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledTimes(1));
    expect(fetchSessionTail).toHaveBeenCalledWith(
      "session-1",
      SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );

    thresholdHarness.unmount();
    fetchSessionTail.mockClear();

    const aboveThresholdSession = makeSession({
      messagesLoaded: false,
      messageCount: SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES + 1,
      sessionMutationStamp: 1,
    });
    const aboveThresholdParams = makeLiveStateParams(aboveThresholdSession);
    aboveThresholdParams.adoptionRefs.latestStateRevisionRef.current = 5;
    aboveThresholdParams.adoptionRefs.sessionsRef.current = [
      aboveThresholdSession,
    ];

    renderLiveStateHarness(aboveThresholdParams, () => {});

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledTimes(1));
    expect(fetchSessionTail).toHaveBeenCalledWith(
      "session-1",
      SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
  });
});

describe("resolveAdoptStateSessionOptions", () => {
  it("preserves an explicit mutation-stamp fast-path disable without a server instance change", () => {
    expect(
      resolveAdoptStateSessionOptions(
        { disableMutationStampFastPath: true },
        false,
      ).disableMutationStampFastPath,
    ).toBe(true);
  });

  it("disables the mutation-stamp fast path when the server instance changes", () => {
    expect(
      resolveAdoptStateSessionOptions(
        { disableMutationStampFastPath: false },
        true,
      ).disableMutationStampFastPath,
    ).toBe(true);
  });

  it("keeps the mutation-stamp fast path enabled by default", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, false)
        .disableMutationStampFastPath,
    ).toBe(false);
  });

  it("forces messages unloaded on a server instance change", () => {
    // The persisted-on-disk session record clears `sessionMutationStamp`
    // on save/load, so a coincidentally-matching `messageCount` would
    // otherwise leave the active pane stuck on stale streaming content
    // after a backend restart. Adoption-side opt-in keeps this confined
    // to the restart path; ordinary live-update reconciles must not
    // force re-hydration of every session.
    expect(
      resolveAdoptStateSessionOptions(undefined, true).forceMessagesUnloaded,
    ).toBe(true);
  });

  it("does not force messages unloaded without a server instance change", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, false).forceMessagesUnloaded,
    ).toBe(false);
  });
});

describe("classifyFetchedSessionAdoption", () => {
  const message: Message = {
    id: "message-1",
    type: "text",
    author: "assistant",
    timestamp: "10:00",
    text: "Hello",
  };

  it("adopts a matching loaded same-instance response", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 5,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext(),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("classifies replacement-instance hydration as restart resync", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 1,
        responseServerInstanceId: "server-b",
        requestContext: makeHydrationRequestContext(),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("restartResync");
  });

  it("requests a state resync when fetched metadata is ahead of the summary", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [
            message,
            { ...message, id: "message-2", text: "Newer" },
          ],
          messagesLoaded: true,
          messageCount: 2,
          sessionMutationStamp: 2,
        }),
        responseRevision: 5,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext(),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("stateResync");
  });

  it("adopts a loaded response when retained text diverged but metadata still matches", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 6,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Corrupted live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 6,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("rejects divergent same-metadata hydration after a newer live revision", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 7,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Newer live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 7,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("stale");
  });

  it("allows explicit text-repair hydration after an unrelated newer live revision", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 7,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          allowDivergentTextRepairAfterNewerRevision: true,
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Corrupted gapped live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 7,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("allows explicit text-repair hydration at the request revision after an unrelated newer live revision", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 6,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          allowDivergentTextRepairAfterNewerRevision: true,
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Corrupted gapped live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 7,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("rejects stale lower-revision responses once the session is loaded", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 9,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({ revision: 9 }),
        currentSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 10,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("stale");
  });
});

describe("hydrationRetainedMessagesMatch", () => {
  const message: Message = {
    id: "message-1",
    type: "text",
    author: "assistant",
    timestamp: "10:00",
    text: "Hello",
  };

  it("matches structurally identical retained messages", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message }] },
        { messages: [{ ...message }] },
      ),
    ).toBe(true);
  });

  it("matches retained messages that appear as an ordered subsequence of the hydrated transcript", () => {
    const olderMessage: Message = {
      ...message,
      id: "message-older",
      text: "Older retained message",
    };
    const missingMessage: Message = {
      ...message,
      id: "message-missing",
      text: "Message only present after hydration",
    };
    const latestMessage: Message = {
      ...message,
      id: "message-latest",
      text: "Latest retained message",
    };

    expect(
      hydrationRetainedMessagesMatch(
        { messages: [olderMessage, missingMessage, latestMessage] },
        { messages: [olderMessage, latestMessage] },
      ),
    ).toBe(true);
  });

  it("treats either empty side as retainable", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [] },
        { messages: [message] },
      ),
    ).toBe(true);
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [message] },
        { messages: [] },
      ),
    ).toBe(true);
  });

  it("rejects non-empty message shape mismatches", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message, text: "Hello" }] },
        { messages: [{ ...message, text: "Goodbye" }] },
      ),
    ).toBe(false);
  });

  it("rejects retained messages that are missing from the hydrated transcript", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message, id: "message-other" }] },
        { messages: [message] },
      ),
    ).toBe(false);
  });

  it("rejects retained messages that appear out of order in the hydrated transcript", () => {
    const firstMessage: Message = {
      ...message,
      id: "message-first",
      text: "First",
    };
    const secondMessage: Message = {
      ...message,
      id: "message-second",
      text: "Second",
    };

    expect(
      hydrationRetainedMessagesMatch(
        { messages: [secondMessage, firstMessage] },
        { messages: [firstMessage, secondMessage] },
      ),
    ).toBe(false);
  });

  it("rejects extra client-side fields on retained messages", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [message] },
        { messages: [{ ...message, localRenderCache: true } as Message] },
      ),
    ).toBe(false);
  });
});

describe("hydrationSessionMetadataMatches", () => {
  const baseSession: Session = {
    id: "session-1",
    name: "Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "idle",
    preview: "",
    messages: [],
    messagesLoaded: false,
  };

  it("rejects numeric response metadata when the current session captured null", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messageCount: 3,
          sessionMutationStamp: 7,
        },
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
      ),
    ).toBe(false);
  });

  it("treats null metadata as an exact value rather than a wildcard", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
      ),
    ).toBe(true);
  });

  it("falls back to loaded message length when messageCount is absent", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messages: [
            {
              id: "m1",
              type: "text",
              author: "assistant",
              timestamp: "10:00",
              text: "One",
            },
          ],
          messagesLoaded: true,
          sessionMutationStamp: 1,
        },
        {
          ...baseSession,
          messageCount: 1,
          messages: [],
          messagesLoaded: false,
          sessionMutationStamp: 1,
        },
      ),
    ).toBe(true);
  });
});

describe("hydrationSessionMetadataIsAhead", () => {
  const baseSession: Session = {
    id: "session-1",
    name: "Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "idle",
    preview: "",
    messages: [],
    messagesLoaded: false,
  };

  it("treats equal counts with a newer mutation stamp as ahead", () => {
    expect(
      hydrationSessionMetadataIsAhead(
        { ...baseSession, messageCount: 3, sessionMutationStamp: 11 },
        { ...baseSession, messageCount: 3, sessionMutationStamp: 10 },
      ),
    ).toBe(true);
  });

  it("does not treat equal counts with an equal mutation stamp as ahead", () => {
    expect(
      hydrationSessionMetadataIsAhead(
        { ...baseSession, messageCount: 3, sessionMutationStamp: 10 },
        { ...baseSession, messageCount: 3, sessionMutationStamp: 10 },
      ),
    ).toBe(false);
  });

  it("falls back to mutation stamps when message counts are unavailable", () => {
    expect(
      hydrationSessionMetadataIsAhead(
        { ...baseSession, messageCount: null, sessionMutationStamp: 2 },
        { ...baseSession, messageCount: null, sessionMutationStamp: 1 },
      ),
    ).toBe(true);
  });
});
