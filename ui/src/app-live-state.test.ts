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
  getSessionRecordSnapshotForTesting,
  resetSessionStoreForTesting,
  upsertSessionStoreSession,
} from "./session-store";
import {
  SESSION_HISTORY_PAGE_MESSAGE_COUNT,
  SESSION_TAIL_WINDOW_MESSAGE_COUNT,
} from "./session-tail-policy";
import {
  __resetSessionHydrationPerformanceForTests,
  sessionTranscriptCommitToken,
} from "./session-hydration-performance";
import {
  requestSessionHistoryAroundPage,
  requestSessionHistoryOlderPage,
  requestSessionHistoryPage,
  requestSessionHistoryStartPage,
  requestSessionHistoryTailPage,
} from "./session-history-demand";
import { CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES } from "./panels/conversation-composer-focus";
import type { StateResponse } from "./api";
import type {
  DelegationSummary,
  Message,
  Session,
  StateSessionSummary,
} from "./types";
import type { WorkspaceState } from "./workspace";

const PAGED_TRANSCRIPT_MESSAGE_COUNT =
  SESSION_HISTORY_PAGE_MESSAGE_COUNT + SESSION_TAIL_WINDOW_MESSAGE_COUNT;

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

type TestSession = Session & { messageCount: number; queuePaused: boolean };

function makeSession(overrides: Partial<Session> = {}): TestSession {
  const messages = overrides.messages ?? [];
  return {
    id: "session-1",
    name: "Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "idle",
    preview: "",
    messages,
    messagesLoaded: false,
    ...overrides,
    messageCount: overrides.messageCount ?? messages.length,
    queuePaused: overrides.queuePaused ?? false,
  };
}

function makeStateSessionSummary(session: Session): StateSessionSummary {
  return {
    ...session,
    messageCount: session.messageCount ?? session.messages.length,
    queuePaused: session.queuePaused ?? false,
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
    reviewResultRequired: overrides.reviewResultRequired ?? false,
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
    sessions: [makeStateSessionSummary(session)],
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
      setDelegationChildSessionIds: noopSetter,
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
      setHasAdoptedStateSnapshot: noopSetter,
      setBackendConnectionIssueDetail: noopSetter,
      setBackendConnectionState: noopSetter,
    },
    preferenceSetters: {
      setDefaultCodexModel: noopSetter,
      setDefaultCodexSandboxMode: noopSetter,
      setDefaultCodexApprovalPolicy: noopSetter,
      setDefaultClaudeModel: noopSetter,
      setDefaultCursorModel: noopSetter,
      setDefaultGeminiModel: noopSetter,
      setDefaultOpenCodeModel: noopSetter,
      setDefaultCodexReasoningEffort: noopSetter,
      setDefaultClaudeApprovalMode: noopSetter,
      setDefaultClaudeEffort: noopSetter,
      setRemoteConfigs: noopSetter,
      setTelegramConfig: noopSetter,
      setEngramHostSettings: noopSetter,
    },
    applyControlPanelLayout: (workspace) => workspace,
    clearRecoveredBackendRequestError: vi.fn(),
    reportRequestError: vi.fn(),
    requestBackendReconnectRef: { current: vi.fn() },
    requestActionRecoveryResyncRef: makeCountingActionRecoveryRef(
      actionRecoveryInvocations,
    ),
    activeSession: session,
    activeTranscriptSessionId: session.id,
    visibleSessionHydrationTargets: [{ id: session.id, messagesLoaded: false }],
  } as UseAppLiveStateParams;
}

function renderLiveStateHarness(
  params: UseAppLiveStateParams,
  capture: (hook: UseAppLiveStateReturn) => void,
  getVisibleSessionHydrationTargets: () => readonly SessionHydrationTarget[] = () => [
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

const appendedComposerDrafts: HTMLTextAreaElement[] = [];

function appendFocusedComposerDraft(value: string) {
  const composer = document.createElement("textarea");
  for (const [attribute, attributeValue] of Object.entries(
    CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES,
  )) {
    composer.setAttribute(attribute, attributeValue);
  }
  composer.value = value;
  document.body.appendChild(composer);
  appendedComposerDrafts.push(composer);
  composer.focus();
  return composer;
}

async function flushHydrationMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

function mockHistoryPagesThroughFullSessionFixture() {
  return vi
    .spyOn(api, "fetchSessionHistory")
    .mockImplementation(async (sessionId, options) => {
      const response = await api.fetchSessionTail(sessionId);
      const end =
        options.before === undefined
          ? response.session.messages.length
          : response.session.messages.findIndex(
              (message) => message.id === options.before,
            );
      if (end < 0) {
        throw new Error("history fixture cursor is unavailable");
      }
      const start = Math.max(0, end - (options.limit ?? 500));
      const messages = response.session.messages.slice(start, end);
      const hasMore = start > 0;
      return {
        hasMore,
        messageCount:
          response.session.messageCount ?? response.session.messages.length,
        messages,
        nextBefore: hasMore ? (messages[0]?.id ?? null) : null,
        revision: response.revision,
        serverInstanceId: response.serverInstanceId,
        sessionMutationStamp: response.session.sessionMutationStamp ?? 0,
      };
    });
}

afterEach(() => {
  for (const composer of appendedComposerDrafts.splice(0)) {
    composer.remove();
  }
  resetSessionStoreForTesting();
  __resetSessionHydrationPerformanceForTests();
  EventSourceMock.instances = [];
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("deferred session-store sync", () => {
  it("syncs machine-scoped Engram settings from adopted app state", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const session = makeSession();
    const params = makeLiveStateParams(session);
    const setEngramHostSettings = vi.fn();
    params.preferenceSetters.setEngramHostSettings = setEngramHostSettings;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    const state = makeStateResponse(session);
    act(() => {
      hook?.syncPreferencesFromState({
        ...state,
        preferences: {
          ...state.preferences,
          engram: {
            developerName: "greg",
            binaryPath: "C:\\tools\\engram.exe",
            home: "C:\\EngramHome",
            bootRecoveryBudgetMs: 8_000,
          },
        },
      });
    });

    expect(setEngramHostSettings).toHaveBeenCalledWith({
      developerName: "greg",
      binaryPath: "C:\\tools\\engram.exe",
      home: "C:\\EngramHome",
      bootRecoveryBudgetMs: 8_000,
    });
  });

  it("syncs Telegram preferences from adopted app state", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const session = makeSession();
    const params = makeLiveStateParams(session);
    const setTelegramConfig = vi.fn();
    params.preferenceSetters.setTelegramConfig = setTelegramConfig;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    const state = makeStateResponse(session);
    act(() => {
      hook?.syncPreferencesFromState({
        ...state,
        preferences: {
          ...state.preferences,
          telegram: {
            enabled: true,
            forwardAssistantReplies: true,
            subscribedProjectIds: ["project-1"],
            defaultProjectId: "project-1",
            defaultSessionId: "session-1",
          },
        },
      });
    });

    expect(setTelegramConfig).toHaveBeenCalledTimes(1);
    const updater = setTelegramConfig.mock.calls[0]?.[0];
    expect(typeof updater).toBe("function");
    expect(updater?.(undefined)).toEqual({
      enabled: true,
      forwardAssistantReplies: true,
      subscribedProjectIds: ["project-1"],
      defaultProjectId: "project-1",
      defaultSessionId: "session-1",
    });
  });

  it("keeps the current Telegram config object when adopted app state is equal", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const session = makeSession();
    const params = makeLiveStateParams(session);
    const setTelegramConfig = vi.fn();
    params.preferenceSetters.setTelegramConfig = setTelegramConfig;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    const currentTelegramConfig = {
      enabled: true,
      forwardAssistantReplies: true,
      subscribedProjectIds: ["project-1"],
      defaultProjectId: "project-1",
      defaultSessionId: "session-1",
    };
    const state = makeStateResponse(session);
    act(() => {
      hook?.syncPreferencesFromState({
        ...state,
        preferences: {
          ...state.preferences,
          telegram: { ...currentTelegramConfig },
        },
      });
    });

    expect(setTelegramConfig).toHaveBeenCalledTimes(1);
    const updater = setTelegramConfig.mock.calls[0]?.[0];
    expect(typeof updater).toBe("function");
    expect(updater?.(currentTelegramConfig)).toBe(currentTelegramConfig);
  });

  it("uses delegation summaries to keep child sessions hidden from ordinary lists", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const parentSession = makeSession({ id: "parent-session" });
    const childSession = makeSession({ id: "child-session" });
    const params = makeLiveStateParams(parentSession);
    params.adoptionRefs.sessionsRef.current = [parentSession];
    params.stateSetters.setSessions = vi.fn((nextSessions: Session[]) => {
      params.adoptionRefs.sessionsRef.current = nextSessions;
    }) as typeof params.stateSetters.setSessions;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    act(() => {
      hook?.adoptState({
        ...makeStateResponse(parentSession, 2),
        sessions: [parentSession, childSession],
        delegations: [
          makeDelegationSummary({
            parentSessionId: parentSession.id,
            childSessionId: childSession.id,
          }),
        ],
      });
    });

    expect(
      params.adoptionRefs.sessionsRef.current.find(
        (session) => session.id === childSession.id,
      )?.parentDelegationId,
    ).toBe("delegation-1");
  });

  it("skips broad session and workspace updates when adopted sessions are unchanged", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const session = makeSession({
      messages: [],
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 7,
    });
    const params = makeLiveStateParams(session);
    const previousSessions = [session];
    params.adoptionRefs.sessionsRef.current = previousSessions;
    const setSessions = vi.fn() as typeof params.stateSetters.setSessions;
    const setWorkspace = vi.fn() as typeof params.stateSetters.setWorkspace;
    params.stateSetters.setSessions = setSessions;
    params.stateSetters.setWorkspace = setWorkspace;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    const unchangedSummary = {
      ...session,
      messages: [],
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 7,
    };
    act(() => {
      hook?.adoptState({
        ...makeStateResponse(unchangedSummary, 2),
        sessions: [unchangedSummary],
      });
    });

    expect(params.adoptionRefs.sessionsRef.current).toBe(previousSessions);
    expect(setSessions).not.toHaveBeenCalled();
    expect(setWorkspace).not.toHaveBeenCalled();
  });

  it("prunes delegated child workspace tabs on server instance changes even when sessions are unchanged", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const parentSession = makeSession({
      id: "parent-session",
      messages: [],
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 7,
    });
    const childSession = makeSession({
      id: "child-session",
      parentDelegationId: "delegation-1",
      messages: [],
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 7,
    });
    const params = makeLiveStateParams(parentSession);
    const previousSessions = [parentSession, childSession];
    params.adoptionRefs.sessionsRef.current = previousSessions;
    const setSessions = vi.fn() as typeof params.stateSetters.setSessions;
    const currentWorkspace: WorkspaceState = {
      root: {
        type: "pane",
        paneId: "pane-1",
      },
      panes: [
        {
          id: "pane-1",
          tabs: [
            {
              id: "tab-child",
              kind: "session",
              sessionId: "child-session",
            },
          ],
          activeTabId: "tab-child",
          activeSessionId: "child-session",
          viewMode: "session",
          lastSessionViewMode: "session",
          sourcePath: null,
        },
      ],
      activePaneId: "pane-1",
    };
    let updatedWorkspace: WorkspaceState | null = null;
    const setWorkspace = vi.fn(
      (update: Parameters<typeof params.stateSetters.setWorkspace>[0]) => {
        updatedWorkspace =
          typeof update === "function" ? update(currentWorkspace) : update;
      },
    ) as typeof params.stateSetters.setWorkspace;
    params.stateSetters.setSessions = setSessions;
    params.stateSetters.setWorkspace = setWorkspace;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    act(() => {
      hook?.adoptState(
        {
          ...makeStateResponse(parentSession, 2),
          serverInstanceId: "server-b",
          sessions: previousSessions,
          delegations: [
            makeDelegationSummary({
              id: "delegation-1",
              childSessionId: "child-session",
            }),
          ],
        },
        { allowUnknownServerInstance: true },
      );
    });

    // This is the path under test: `reconcileSessions` must preserve array
    // identity, not just equal contents, while restart pruning still reconciles
    // workspace tabs.
    expect(params.adoptionRefs.sessionsRef.current).toBe(previousSessions);
    expect(setSessions).not.toHaveBeenCalled();
    expect(setWorkspace).toHaveBeenCalledTimes(1);
    expect(updatedWorkspace).not.toBeNull();
    const prunedWorkspace = updatedWorkspace as unknown as WorkspaceState;
    expect(prunedWorkspace.panes[0].tabs).toEqual([
      expect.objectContaining({
        kind: "session",
        sessionId: "parent-session",
      }),
    ]);
    expect(prunedWorkspace.panes[0].activeSessionId).toBe("parent-session");
  });

  it("keeps missing pending-open recovery armed across unchanged session adoption", () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const session = makeSession({
      messages: [],
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 7,
    });
    const pendingSession = makeSession({
      id: "missing-pending-session",
      projectId: "pending-project",
    });
    const params = makeLiveStateParams(session);
    const previousSessions = [session];
    params.adoptionRefs.sessionsRef.current = previousSessions;
    const setSessions = vi.fn((nextSessions: Session[]) => {
      params.adoptionRefs.sessionsRef.current = nextSessions;
    }) as typeof params.stateSetters.setSessions;
    const setWorkspace = vi.fn() as typeof params.stateSetters.setWorkspace;
    const setSelectedProjectId =
      vi.fn() as typeof params.stateSetters.setSelectedProjectId;
    params.stateSetters.setSessions = setSessions;
    params.stateSetters.setWorkspace = setWorkspace;
    params.stateSetters.setSelectedProjectId = setSelectedProjectId;
    let hook: UseAppLiveStateReturn | null = null;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    act(() => {
      params.requestActionRecoveryResyncRef.current({
        openSessionId: pendingSession.id,
        paneId: "pane-1",
      });
    });

    const unchangedSummary = {
      ...session,
      messages: [],
      messagesLoaded: false,
      messageCount: 0,
      sessionMutationStamp: 7,
    };
    act(() => {
      hook?.adoptState({
        ...makeStateResponse(unchangedSummary, 2),
        sessions: [unchangedSummary],
      });
    });

    expect(params.adoptionRefs.sessionsRef.current).toBe(previousSessions);
    expect(setSessions).not.toHaveBeenCalled();
    expect(setWorkspace).not.toHaveBeenCalled();
    expect(setSelectedProjectId).not.toHaveBeenCalled();

    act(() => {
      hook?.adoptState({
        ...makeStateResponse(unchangedSummary, 3),
        sessions: [unchangedSummary, pendingSession],
      });
    });

    expect(setSessions).toHaveBeenCalledTimes(1);
    expect(setSelectedProjectId).toHaveBeenCalledWith("pending-project");
  });

  it("keeps reconnecting when valid delta data arrives after an error without an open event", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("reconnecting");
  });

  it("clears reconnecting when an applied delta arrives on an open stream after a missed open event", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
      if (eventSource) {
        eventSource.readyState = 1;
      }
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

  it("clears reconnecting when a delta arrives on an open stream after retry clears error proof", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
      params.requestBackendReconnectRef.current?.();
      if (eventSource) {
        eventSource.readyState = 1;
      }
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

  it("clears reconnecting when a state event arrives on an open stream after retry clears error proof", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
    const recoveredSession = makeSession({
      ...session,
      preview: "Recovered state output.",
      sessionMutationStamp: 3,
      messages: [
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Recovered state output.",
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
      params.requestBackendReconnectRef.current?.();
      if (eventSource) {
        eventSource.readyState = 1;
      }
      eventSource?.dispatchNamedEvent(
        "state",
        makeStateResponse(recoveredSession, 3),
      );
    });

    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3);
    expect(params.adoptionRefs.sessionsRef.current[0]?.preview).toBe(
      "Recovered state output.",
    );
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("connected");
  });

  it("keeps reconnecting when automatic fallback adopts a newer idle snapshot", async () => {
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
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
      await flushHydrationMicrotasks();
    });

    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.sessionsRef.current[0]?.status).toBe("idle");
    expect(params.adoptionRefs.sessionsRef.current[0]?.preview).toBe("Done.");
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("reconnecting");
  });

  it("keeps reconnect fallback armed after non-rearming action recovery until live proof", async () => {
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
    const recoveredSession = makeSession({
      status: "idle",
      messagesLoaded: true,
      messageCount: 2,
      preview: "Recovered by action recovery.",
      messages: [
        ...activeSession.messages,
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Recovered by action recovery.",
        },
      ],
      sessionMutationStamp: 2,
    });
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockResolvedValue(makeStateResponse(recoveredSession, 3));
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
      params.requestActionRecoveryResyncRef.current();
      await flushHydrationMicrotasks();
    });

    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.sessionsRef.current[0]?.preview).toBe(
      "Recovered by action recovery.",
    );
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("reconnecting");

    fetchState.mockClear();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await Promise.resolve();
    });

    expect(fetchState).toHaveBeenCalledTimes(1);
  });

  it("recreates SSE when an older in-flight resync adopts an armed replacement instance", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
    );
    const session = makeSession({
      messagesLoaded: true,
      messageCount: 1,
      preview: "Before restart.",
      messages: [
        {
          id: "message-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Before restart.",
        },
      ],
    });
    const params = makeLiveStateParams(session);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [session];
    let hook: UseAppLiveStateReturn | null = null;
    let originalEventSource: EventSourceMock | null = null;
    const setSessions = vi.fn((nextSessions: Session[]) => {
      expect(originalEventSource).not.toBeNull();
      expect(EventSourceMock.instances).toEqual([originalEventSource]);
      params.adoptionRefs.sessionsRef.current = nextSessions;
    }) as typeof params.stateSetters.setSessions;
    params.stateSetters.setSessions = setSessions;

    renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });
    originalEventSource =
      EventSourceMock.instances[EventSourceMock.instances.length - 1];

    act(() => {
      hook?.forceSseReconnect();
    });
    act(() => {
      hook?.adoptState(
        {
          ...makeStateResponse(
            makeSession({
              messagesLoaded: true,
              messageCount: 1,
              preview: "Recovered on older probe.",
              messages: [
                {
                  id: "message-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Recovered on older probe.",
                },
              ],
            }),
            1,
          ),
          serverInstanceId: "server-b",
        },
        { allowUnknownServerInstance: true },
      );
    });

    expect(setSessions).toHaveBeenCalled();
    await waitFor(() => expect(EventSourceMock.instances.length).toBe(2));
    expect(EventSourceMock.instances[1]).not.toBe(originalEventSource);
  });

  it("applies delegation wait create and consume deltas locally", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
  it.each(makeDelegationDeltaCases(2))(
    "repairs equal-revision %s without session-delta hydration",
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
        .mockResolvedValue(makeStateResponse(session, 2));
      const fetchSession = vi
        .spyOn(api, "fetchSessionTail")
        .mockImplementation(
          () =>
            new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(
              () => {},
            ),
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
    },
  );

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
      const fetchState = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => new Promise<StateResponse>(() => {}));
      const fetchSession = vi
        .spyOn(api, "fetchSessionTail")
        .mockImplementation(
          () =>
            new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(
              () => {},
            ),
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
      const fetchSession = vi
        .spyOn(api, "fetchSessionTail")
        .mockImplementation(
          () =>
            new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(
              () => {},
            ),
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
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => new Promise<StateResponse>(() => {}));
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => repair);
    const fetchSession = vi
      .spyOn(api, "fetchSessionTail")
      .mockImplementation(
        () =>
          new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(
            () => {},
          ),
      );
    const params = makeLiveStateParams(parentSession);
    params.adoptionRefs.latestStateRevisionRef.current = 2;
    params.adoptionRefs.sessionsRef.current = [parentSession];

    renderLiveStateHarness(
      params,
      () => {},
      () => [{ id: parentSession.id, messagesLoaded: true }],
    );
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
    const fetchSession = vi
      .spyOn(api, "fetchSessionTail")
      .mockImplementation(
        () =>
          new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(
            () => {},
          ),
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
      await flushHydrationMicrotasks();
    });
    expect(fetchState).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    await act(async () => {
      await flushHydrationMicrotasks();
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
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
      await flushHydrationMicrotasks();
    });
    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.projectsRef.current[0]?.id).toBe(
      "project-after-delegation-repair",
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await flushHydrationMicrotasks();
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
      await flushHydrationMicrotasks();
    });
    expect(fetchState).toHaveBeenCalledTimes(3);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await flushHydrationMicrotasks();
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
    vi.spyOn(api, "fetchSessionTail").mockImplementation(
      () =>
        new Promise<Awaited<ReturnType<typeof api.fetchSessionTail>>>(() => {}),
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
      await flushHydrationMicrotasks();
    });
    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(3);
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("reconnecting");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS);
      await flushHydrationMicrotasks();
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
      await flushHydrationMicrotasks();
    });
    expect(fetchState).toHaveBeenCalledTimes(3);
    expect(params.adoptionRefs.latestStateRevisionRef.current).toBe(4);
    expect(setBackendConnectionState).toHaveBeenLastCalledWith("connected");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 4);
      await flushHydrationMicrotasks();
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
    const fetchSession = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 2,
      serverInstanceId: "server-a",
      session: makeSession({ id: "unexpected-session", messagesLoaded: true }),
    });
    const actionRecoveryInvocations = vi.fn();
    const params = makeLiveStateParams(
      makeSession({
        messageCount: PAGED_TRANSCRIPT_MESSAGE_COUNT - 1,
      }),
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
    const messages = makeHydrationMessages(PAGED_TRANSCRIPT_MESSAGE_COUNT);
    const fetchSessionTail = vi
      .spyOn(api, "fetchSessionTail")
      .mockResolvedValue({
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

    harness.rerenderLiveState();

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledTimes(2));
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
    const fetchSession = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 2,
      serverInstanceId: "server-a",
      session: makeSession({ id: "unexpected-session", messagesLoaded: true }),
    });
    const actionRecoveryInvocations = vi.fn();
    const session = makeSession({
      messageCount: PAGED_TRANSCRIPT_MESSAGE_COUNT - 1,
    });
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
  it("publishes the active transcript record before scheduling the parent session list", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const initialSession = makeSession({
      messageCount: 100,
      messagesLoaded: false,
      sessionMutationStamp: 1,
    });
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messageCount: 100,
        messages: makeHydrationMessages(20),
        messagesLoaded: false,
        sessionMutationStamp: 1,
      }),
    });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];
    const setSessions = vi.fn((nextSessions: Session[]) => {
      expect(
        getSessionRecordSnapshotForTesting(initialSession.id)?.messages,
      ).toHaveLength(20);
      expect(nextSessions[0]?.messages).toHaveLength(20);
    });
    params.stateSetters.setSessions =
      setSessions as typeof params.stateSetters.setSessions;

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(setSessions).toHaveBeenCalled());
    const reconciledSessions =
      setSessions.mock.calls[setSessions.mock.calls.length - 1]?.[0];
    expect(reconciledSessions).toHaveLength(1);
    expect(reconciledSessions?.[0]?.messages).toHaveLength(20);
  });

  it("does not arm transcript-commit timing when the active pane shows another view", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const initialSession = makeSession({
      messageCount: 100,
      messagesLoaded: false,
      sessionMutationStamp: 1,
    });
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messageCount: 100,
        messages: makeHydrationMessages(20),
        messagesLoaded: false,
        sessionMutationStamp: 1,
      }),
    });
    const params = makeLiveStateParams(initialSession);
    params.activeTranscriptSessionId = null;
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toHaveLength(
        20,
      ),
    );
    const adoptedSession = params.adoptionRefs.sessionsRef.current[0];
    expect(adoptedSession).toBeDefined();
    expect(sessionTranscriptCommitToken(adoptedSession!)).toBeNull();
  });

  it("adopts queued prompts from a fresh partial-tail hydration", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const initialSession = makeSession({
      messageCount: 100,
      messagesLoaded: false,
      sessionMutationStamp: 1,
    });
    const queuedPrompt = {
      id: "prompt-1",
      timestamp: "10:02",
      text: "Queued while the session is busy",
    };
    const fetchSessionTail = vi
      .spyOn(api, "fetchSessionTail")
      .mockResolvedValue({
        revision: 5,
        serverInstanceId: "server-a",
        session: makeSession({
          messageCount: 100,
          messages: makeHydrationMessages(20),
          messagesLoaded: false,
          pendingPrompts: [queuedPrompt],
          sessionMutationStamp: 1,
        }),
      });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledOnce());
    await waitFor(() => {
      expect(
        params.adoptionRefs.sessionsRef.current[0]?.pendingPrompts,
      ).toEqual([queuedPrompt]);
    });
    expect(
      getSessionRecordSnapshotForTesting(initialSession.id)?.pendingPrompts,
    ).toEqual([queuedPrompt]);
  });

  it("requests action recovery and an authoritative state resync when fetched metadata is ahead", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => new Promise<StateResponse>(() => {}));
    const fetchSession = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
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
    const fetchSession = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
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
    const fetchState = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => new Promise<StateResponse>(() => {}));
    const fetchSession = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
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

  it("refetches a cached partial tail when a newer same-server summary advances", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const originalTail = makeHydrationMessages(20);
    const refreshedTail = [
      ...originalTail.slice(1),
      {
      id: "message-101",
      type: "text" as const,
      author: "assistant" as const,
      timestamp: "10:01",
      text: "Newer assistant reply",
      },
    ];
    const initialSession = makeSession({
      messages: originalTail,
      messagesLoaded: false,
      messageCount: 100,
      sessionMutationStamp: 1,
    });
    const fetchSession = vi
      .spyOn(api, "fetchSessionTail")
      .mockResolvedValueOnce({
        revision: 5,
        serverInstanceId: "server-a",
        session: initialSession,
      })
      .mockResolvedValueOnce({
        revision: 6,
        serverInstanceId: "server-a",
        session: makeSession({
          messages: refreshedTail,
          messagesLoaded: false,
          messageCount: 101,
          sessionMutationStamp: 2,
        }),
      });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    let hook: UseAppLiveStateReturn | null = null;
    const harness = renderLiveStateHarness(params, (nextHook) => {
      hook = nextHook;
    });

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(1));
    act(() => {
      hook?.adoptState(
        makeStateResponse(
          makeSession({
            messages: [],
            messagesLoaded: false,
            messageCount: 101,
            sessionMutationStamp: 2,
          }),
          6,
        ),
      );
    });
    harness.rerenderLiveState();

    await waitFor(() => expect(fetchSession).toHaveBeenCalledTimes(2));
    await waitFor(() => {
      const messages =
        params.adoptionRefs.sessionsRef.current[0]?.messages ?? [];
      expect(messages[messages.length - 1]?.id).toBe("message-101");
    });
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
      .spyOn(api, "fetchSessionTail")
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
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
      true,
    );
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
      .spyOn(api, "fetchSessionTail")
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

    for (
      let attempt = 0;
      attempt < SESSION_HYDRATION_MAX_RETRY_ATTEMPTS;
      attempt += 1
    ) {
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
    const fetchSession = vi
      .spyOn(api, "fetchSessionTail")
      .mockImplementation(async () => {
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
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
      true,
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      hydratedMessages,
    );
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
      .spyOn(api, "fetchSessionTail")
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

  it("loads a bounded tail and exactly one older page per demand", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );

    const messages = makeHydrationMessages(
      2 * SESSION_HISTORY_PAGE_MESSAGE_COUNT +
        SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
    const retainedTail = messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT);
    const oldestPage = messages.slice(0, SESSION_HISTORY_PAGE_MESSAGE_COUNT);
    const newerHistoryPage = messages.slice(
      SESSION_HISTORY_PAGE_MESSAGE_COUNT,
      2 * SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    );
    const initialSession = makeSession({
      messages: [],
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const fetchSessionTail = vi
      .spyOn(api, "fetchSessionTail")
      .mockResolvedValue({
        revision: 5,
        serverInstanceId: "server-a",
        session: makeSession({
          messages: retainedTail,
          messagesLoaded: false,
          messageCount: messages.length,
          sessionMutationStamp: 1,
        }),
      });
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockResolvedValueOnce({
        hasMore: true,
        messageCount: messages.length,
        messages: newerHistoryPage,
        nextBefore: newerHistoryPage[0]?.id ?? null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 1,
      })
      .mockResolvedValueOnce({
        hasMore: false,
        messageCount: messages.length,
        messages: oldestPage,
        nextBefore: null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 1,
      });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});

    await waitFor(() => expect(fetchSessionTail).toHaveBeenCalledOnce());
    expect(fetchSessionTail).toHaveBeenCalledWith(
      "session-1",
      SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
    expect(fetchSessionHistory).not.toHaveBeenCalled();
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      retainedTail,
    );

    let firstOlderPageApplied = false;
    await act(async () => {
      firstOlderPageApplied = await requestSessionHistoryOlderPage("session-1");
    });
    await waitFor(() => expect(fetchSessionHistory).toHaveBeenCalledOnce());

    expect(firstOlderPageApplied).toBe(true);
    expect(fetchSessionHistory).toHaveBeenCalledWith("session-1", {
      before: retainedTail[0]?.id,
      limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    });
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual([
      ...newerHistoryPage,
      ...retainedTail,
    ]);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
      false,
    );

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetchSessionHistory).toHaveBeenCalledTimes(1);

    act(() => requestSessionHistoryPage("session-1"));
    await waitFor(() => expect(fetchSessionHistory).toHaveBeenCalledTimes(2));
    expect(fetchSessionHistory).toHaveBeenLastCalledWith("session-1", {
      before: newerHistoryPage[0]?.id,
      limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    });
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      messages,
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.messagesLoaded).toBe(
      true,
    );
  });

  it("shares one older-page load between passive hydration and completable navigation", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const messages = makeHydrationMessages(
      SESSION_HISTORY_PAGE_MESSAGE_COUNT +
        SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
    const retainedTail = messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT);
    const olderPage = messages.slice(0, SESSION_HISTORY_PAGE_MESSAGE_COUNT);
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messages: retainedTail,
        messagesLoaded: false,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      }),
    });
    let resolveHistoryPage:
      | ((page: Awaited<ReturnType<typeof api.fetchSessionHistory>>) => void)
      | null = null;
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockImplementation(
        () =>
          new Promise((resolve) => {
            resolveHistoryPage = resolve;
          }),
      );
    const initialSession = makeSession({
      messages: [],
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});
    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
        retainedTail,
      ),
    );
    await act(async () => {
      await Promise.resolve();
    });

    act(() => requestSessionHistoryPage("session-1"));
    await waitFor(() => expect(fetchSessionHistory).toHaveBeenCalledOnce());
    const completableDemand = requestSessionHistoryOlderPage("session-1");
    expect(fetchSessionHistory).toHaveBeenCalledTimes(1);

    let applied = false;
    await act(async () => {
      resolveHistoryPage?.({
        hasMore: false,
        messageCount: messages.length,
        messages: olderPage,
        nextBefore: null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 1,
      });
      applied = await completableDemand;
    });

    expect(applied).toBe(true);
    expect(fetchSessionHistory).toHaveBeenCalledTimes(1);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      messages,
    );
  });

  it.each([404, 409])(
    "silently resyncs when passive older hydration fails with %i",
    async (status) => {
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.spyOn(api, "fetchState").mockImplementation(
        () => new Promise<StateResponse>(() => {}),
      );
      const messages = makeHydrationMessages(
        SESSION_HISTORY_PAGE_MESSAGE_COUNT +
          SESSION_TAIL_WINDOW_MESSAGE_COUNT,
      );
      const retainedTail = messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT);
      vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
        revision: 5,
        serverInstanceId: "server-a",
        session: makeSession({
          messages: retainedTail,
          messagesLoaded: false,
          messageCount: messages.length,
          sessionMutationStamp: 1,
        }),
      });
      const requestError = new api.ApiRequestError(
        "request-failed",
        "session history unavailable",
        { status },
      );
      const fetchSessionHistory = vi
        .spyOn(api, "fetchSessionHistory")
        .mockRejectedValue(requestError);
      const actionRecoveryInvocations = vi.fn();
      const initialSession = makeSession({
        messages: [],
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

      renderLiveStateHarness(params, () => {});
      await waitFor(() =>
        expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
          retainedTail,
        ),
      );

      act(() => requestSessionHistoryPage("session-1"));
      await waitFor(() => expect(fetchSessionHistory).toHaveBeenCalledOnce());
      await waitFor(() =>
        expect(actionRecoveryInvocations).toHaveBeenCalledOnce(),
      );

      expect(params.reportRequestError).not.toHaveBeenCalled();
      expect(fetchSessionHistory).toHaveBeenCalledWith("session-1", {
        before: retainedTail[0]?.id,
        limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
      });
    },
  );

  it("releases an older-page load after failure so the same boundary can retry", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const messages = makeHydrationMessages(
      SESSION_HISTORY_PAGE_MESSAGE_COUNT +
        SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
    const retainedTail = messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT);
    const olderPage = messages.slice(0, SESSION_HISTORY_PAGE_MESSAGE_COUNT);
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messages: retainedTail,
        messagesLoaded: false,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      }),
    });
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockRejectedValueOnce(new Error("history unavailable"))
      .mockResolvedValueOnce({
        hasMore: false,
        messageCount: messages.length,
        messages: olderPage,
        nextBefore: null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 1,
      });
    const initialSession = makeSession({
      messages: [],
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});
    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
        retainedTail,
      ),
    );

    await expect(
      requestSessionHistoryOlderPage("session-1"),
    ).resolves.toBe(false);
    await expect(
      requestSessionHistoryOlderPage("session-1"),
    ).resolves.toBe(true);

    expect(fetchSessionHistory).toHaveBeenCalledTimes(2);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      messages,
    );
  });

  it("releases an older-page load when its session disappears before adoption", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const messages = makeHydrationMessages(
      SESSION_HISTORY_PAGE_MESSAGE_COUNT +
        SESSION_TAIL_WINDOW_MESSAGE_COUNT,
    );
    const retainedTail = messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT);
    const olderPage = messages.slice(0, SESSION_HISTORY_PAGE_MESSAGE_COUNT);
    vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
      revision: 5,
      serverInstanceId: "server-a",
      session: makeSession({
        messages: retainedTail,
        messagesLoaded: false,
        messageCount: messages.length,
        sessionMutationStamp: 1,
      }),
    });
    let resolveFirstHistoryPage:
      | ((page: Awaited<ReturnType<typeof api.fetchSessionHistory>>) => void)
      | null = null;
    const historyPage = {
      hasMore: false,
      messageCount: messages.length,
      messages: olderPage,
      nextBefore: null,
      revision: 5,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    };
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirstHistoryPage = resolve;
          }),
      )
      .mockResolvedValueOnce(historyPage);
    const initialSession = makeSession({
      messages: [],
      messagesLoaded: false,
      messageCount: messages.length,
      sessionMutationStamp: 1,
    });
    const params = makeLiveStateParams(initialSession);
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(params, () => {});
    await waitFor(() =>
      expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
        retainedTail,
      ),
    );
    const retainedSession = params.adoptionRefs.sessionsRef.current[0]!;
    const staleDemand = requestSessionHistoryOlderPage("session-1");
    await waitFor(() => expect(fetchSessionHistory).toHaveBeenCalledOnce());
    params.adoptionRefs.sessionsRef.current = [];

    await act(async () => {
      resolveFirstHistoryPage?.(historyPage);
      await expect(staleDemand).resolves.toBe(false);
    });

    params.adoptionRefs.sessionsRef.current = [retainedSession];
    await expect(
      requestSessionHistoryOlderPage("session-1"),
    ).resolves.toBe(true);

    expect(fetchSessionHistory).toHaveBeenCalledTimes(2);
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      messages,
    );
  });

  it("replaces residency with one centered around-position page", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const allMessages = makeHydrationMessages(1_000);
    const initialSession = makeSession({
      messages: allMessages.slice(936),
      messagesLoaded: false,
      hasOlderHistory: true,
      hasNewerHistory: false,
      messageCount: 1_000,
      messageStartIndex: 936,
      sessionMutationStamp: 8,
    });
    const centeredMessages = allMessages.slice(468, 532);
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockResolvedValue({
        hasMore: true,
        hasNewer: true,
        messageStartIndex: 468,
        messageCount: 1_000,
        messages: centeredMessages,
        nextBefore: centeredMessages[0]?.id ?? null,
        nextAfter: centeredMessages[centeredMessages.length - 1]?.id ?? null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 8,
      });
    const params = makeLiveStateParams(initialSession);
    params.activeSession = null;
    params.visibleSessionHydrationTargets = [];
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(
      params,
      () => {},
      () => [],
    );

    let applied = false;
    await act(async () => {
      applied = await requestSessionHistoryAroundPage("session-1", 500);
    });

    expect(applied).toBe(true);
    expect(fetchSessionHistory).toHaveBeenCalledWith("session-1", {
      around: 500,
      limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    });
    const adopted = params.adoptionRefs.sessionsRef.current[0];
    expect(adopted?.messages).toEqual(centeredMessages);
    expect(adopted?.messageStartIndex).toBe(468);
    expect(adopted?.hasOlderHistory).toBe(true);
    expect(adopted?.hasNewerHistory).toBe(true);
  });

  it("adopts a true-start page while the live tail grows concurrently", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const allMessages = makeHydrationMessages(1_001);
    const initialSession = makeSession({
      status: "active",
      messages: allMessages.slice(936, 1_000),
      messagesLoaded: false,
      hasOlderHistory: true,
      hasNewerHistory: false,
      messageCount: 1_000,
      sessionMutationStamp: 8,
    });
    let resolveStartPage:
      | ((page: Awaited<ReturnType<typeof api.fetchSessionHistory>>) => void)
      | null = null;
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockImplementation(
        () =>
          new Promise((resolve) => {
            resolveStartPage = resolve;
          }),
      );
    const params = makeLiveStateParams(initialSession);
    params.activeSession = null;
    params.visibleSessionHydrationTargets = [];
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [initialSession];

    renderLiveStateHarness(
      params,
      () => {},
      () => [],
    );

    const startDemand = requestSessionHistoryStartPage("session-1");
    await waitFor(() => expect(fetchSessionHistory).toHaveBeenCalledOnce());
    expect(fetchSessionHistory).toHaveBeenCalledWith("session-1", {
      from: "start",
      limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    });

    const concurrentlyUpdatedSession = {
      ...initialSession,
      messages: [...initialSession.messages, allMessages[1_000]!],
      messageCount: 1_001,
      sessionMutationStamp: 9,
    };
    params.adoptionRefs.sessionsRef.current = [concurrentlyUpdatedSession];
    upsertSessionStoreSession({
      session: concurrentlyUpdatedSession,
      committedDraft: "",
      draftAttachments: [],
    });

    let applied = false;
    await act(async () => {
      resolveStartPage?.({
        hasMore: false,
        hasNewer: true,
        messageCount: 1_000,
        messages: allMessages.slice(0, SESSION_HISTORY_PAGE_MESSAGE_COUNT),
        nextAfter:
          allMessages[SESSION_HISTORY_PAGE_MESSAGE_COUNT - 1]?.id ?? null,
        nextBefore: null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 8,
      });
      applied = await startDemand;
    });

    expect(applied).toBe(true);
    const adopted = params.adoptionRefs.sessionsRef.current[0];
    expect(adopted?.messages[0]?.id).toBe("message-1");
    expect(adopted?.messages[(adopted?.messages.length ?? 1) - 1]?.id).toBe(
      `message-${SESSION_HISTORY_PAGE_MESSAGE_COUNT}`,
    );
    expect(adopted?.hasNewerHistory).toBe(true);
    expect(adopted?.messageCount).toBe(1_001);
    expect(adopted?.sessionMutationStamp).toBe(9);
  });

  it("replaces a historical window with one bounded live-tail demand", async () => {
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.spyOn(api, "fetchState").mockImplementation(
      () => new Promise<StateResponse>(() => {}),
    );
    const historicalMessages = makeHydrationMessages(
      SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    );
    const liveTailMessages = makeHydrationMessages(1_000).slice(
      -SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    );
    const historicalSession = makeSession({
      messages: historicalMessages,
      messagesLoaded: false,
      hasOlderHistory: false,
      hasNewerHistory: true,
      messageCount: 1_000,
      sessionMutationStamp: 8,
    });
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockResolvedValue({
        hasMore: true,
        messageCount: 1_000,
        messages: liveTailMessages,
        nextBefore: liveTailMessages[0]?.id ?? null,
        hasNewer: false,
        nextAfter: null,
        revision: 5,
        serverInstanceId: "server-a",
        sessionMutationStamp: 8,
      });
    const params = makeLiveStateParams(historicalSession);
    params.activeSession = null;
    params.visibleSessionHydrationTargets = [];
    params.adoptionRefs.latestStateRevisionRef.current = 5;
    params.adoptionRefs.sessionsRef.current = [historicalSession];

    renderLiveStateHarness(
      params,
      () => {},
      () => [],
    );

    let applied = false;
    await act(async () => {
      applied = await requestSessionHistoryTailPage("session-1");
    });

    expect(applied).toBe(true);
    expect(fetchSessionHistory).toHaveBeenCalledWith("session-1", {
      limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    });
    expect(params.adoptionRefs.sessionsRef.current[0]?.messages).toEqual(
      liveTailMessages,
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.hasNewerHistory).toBe(
      false,
    );
    expect(params.adoptionRefs.sessionsRef.current[0]?.hasOlderHistory).toBe(
      true,
    );
  });

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

  it("prunes delegated child workspace tabs on a server instance change", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, true)
        .pruneDelegatedChildWorkspaceTabs,
    ).toBe(true);
  });

  it("does not force messages unloaded without a server instance change", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, false).forceMessagesUnloaded,
    ).toBe(false);
  });

  it("keeps delegated child workspace tabs during ordinary live updates", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, false)
        .pruneDelegatedChildWorkspaceTabs,
    ).toBe(false);
  });
});
