import { act, render, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import {
  resolveAdoptStateSessionOptions,
  useAppLiveState,
  type SessionHydrationTarget,
  type UseAppLiveStateParams,
  type UseAppLiveStateReturn,
} from "./app-live-state";
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
import type { StateResponse } from "./api";
import type { Message, Session } from "./types";
import type { WorkspaceState } from "./workspace";

class EventSourceMock {
  static instances: EventSourceMock[] = [];

  onerror: ((event: Event) => void) | null = null;
  onopen: ((event: Event) => void) | null = null;

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
      vi.advanceTimersByTime(50);
      await Promise.resolve();
    });

    expect(fetchSession).toHaveBeenCalledTimes(2);
    expect(actionRecoveryInvocations).not.toHaveBeenCalled();
    expect(fetchState).not.toHaveBeenCalled();
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
      vi.advanceTimersByTime(50);
      await Promise.resolve();
    });

    expect(fetchSession).toHaveBeenCalledTimes(3);
    expect(fetchSession.mock.calls.map(([sessionId]) => sessionId)).toEqual([
      "session-1",
      "session-2",
      "session-1",
    ]);
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
