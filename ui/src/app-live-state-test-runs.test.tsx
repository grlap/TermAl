// Test-run snapshot/delta integration through the real live-state hook and
// transport. No sessions are needed: runs share the app's global revision gate.
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import { useAppLiveState } from "./app-live-state";
import type { UseAppLiveStateParams } from "./app-live-state-types";
import { makeTestRun, makeTestRunCard, makeTestRunDetail, makeTestRunWait } from "./test-runs-fixtures";
import type { Session, TestRunCardMessage } from "./types";
import * as runsApi from "./test-runs-api";
import { TestRunsProvider } from "./test-runs-context";
import { TestRunsPanel } from "./panels/TestRunsPanel";
import { RECONNECT_STATE_RESYNC_DELAY_MS } from "./app-shell-internals";

class Stream extends EventTarget {
  static latest: Stream;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readyState = 1;
  constructor() { super(); Stream.latest = this; }
  close() { this.readyState = 2; }
  emit(payload: unknown) { this.dispatchEvent(new MessageEvent("delta", { data: JSON.stringify(payload) })); }
}
function params(): UseAppLiveStateParams {
  const set = vi.fn();
  return {
    adoptionRefs: {
      isMountedRef: { current: true }, latestStateRevisionRef: { current: null },
      lastSeenServerInstanceIdRef: { current: null }, seenServerInstanceIdsRef: { current: new Set() },
      sessionsRef: { current: [] }, draftsBySessionIdRef: { current: {} }, draftAttachmentsBySessionIdRef: { current: {} },
      codexStateRef: { current: {} }, agentReadinessRef: { current: [] }, projectsRef: { current: [] },
      orchestratorsRef: { current: [] }, delegationWaitsRef: { current: [] }, workspaceSummariesRef: { current: [] },
      refreshingAgentCommandSessionIdsRef: { current: {} }, confirmedUnknownModelSendsRef: { current: new Set() },
      activePromptPollCancelRef: { current: null }, activePromptPollSessionIdRef: { current: null },
    },
    stateSetters: {
      setSessions: set, setWorkspace: set, setCodexState: set, setAgentReadiness: set,
      setProjects: set, setOrchestrators: set, setDelegationWaits: set, setDelegationChildSessionIds: set,
      setWorkspaceSummaries: set, setDraftsBySessionId: set, setDraftAttachmentsBySessionId: set,
      setSendingSessionIds: set, setStoppingSessionIds: set, setKillingSessionIds: set,
      setKillRevealSessionId: set, setPendingKillSessionId: set, setPendingSessionRename: set,
      setUpdatingSessionIds: set, setAgentCommandsBySessionId: set, setRefreshingAgentCommandSessionIds: set,
      setAgentCommandErrors: set, setSessionSettingNotices: set, setSelectedProjectId: set,
      setIsLoading: set, setHasAdoptedStateSnapshot: set, setBackendConnectionIssueDetail: set, setBackendConnectionState: set,
    },
    preferenceSetters: {
      setDefaultCodexModel: set, setDefaultCodexSandboxMode: set, setDefaultCodexApprovalPolicy: set,
      setDefaultClaudeModel: set, setDefaultCursorModel: set, setDefaultGeminiModel: set, setDefaultKimiModel: set,
      setDefaultKimiApprovalMode: set, setDefaultKimiEffort: set,
      setDefaultOpenCodeModel: set, setDefaultOpenCodeApprovalMode: set, setDefaultCodexReasoningEffort: set,
      setDefaultClaudeApprovalMode: set, setDefaultClaudeEffort: set, setRemoteConfigs: set,
      setTelegramConfig: set, setEngramHostSettings: set,
    },
    applyControlPanelLayout: workspace => workspace,
    clearRecoveredBackendRequestError: vi.fn(), reportRequestError: vi.fn(),
    requestBackendReconnectRef: { current: vi.fn() }, requestActionRecoveryResyncRef: { current: vi.fn() },
    activeSession: null, activeTranscriptSessionId: null, visibleSessionHydrationTargets: [],
  };
}
function snapshot(revision: number, testRuns?: api.StateResponse["testRuns"]): api.StateResponse {
  return {
    revision, serverInstanceId: "test-server", codex: {}, agentReadiness: [],
    projects: [], sessions: [], workspaces: [], orchestrators: [], testRuns,
    preferences: { defaultCodexModel: "default", defaultCodexSandboxMode: "workspace-write",
      defaultCodexApprovalPolicy: "on-request", defaultClaudeModel: "default", defaultCursorModel: "default",
      defaultGeminiModel: "default", defaultOpenCodeModel: "default", defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask", defaultClaudeEffort: "default" },
  };
}
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("adopts pending waits, revision-gates wait state and clears omitted waits", () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const wait = makeTestRunWait();
  act(() => { hook.result.current.adoptState({ ...snapshot(1), testRunWaits: [wait] }, { allowUnknownServerInstance: true }); });
  expect(hook.result.current.testRunWaits).toEqual([wait]);
  act(() => Stream.latest.emit({ type: "testRunWaitCreated", serverInstanceId: "test-server", revision: 2, wait: { ...wait, id: "second" } }));
  expect(hook.result.current.testRunWaits).toHaveLength(2);
  act(() => Stream.latest.emit({ type: "testRunWaitConsumed", serverInstanceId: "test-server", revision: 3, waitId: wait.id, sessionId: wait.sessionId, reason: "sessionStopped" }));
  expect(hook.result.current.testRunWaits.map(value => value.id)).toEqual(["second"]);
  act(() => Stream.latest.emit({ type: "testRunWaitCreated", serverInstanceId: "test-server", revision: 2, wait }));
  expect(hook.result.current.testRunWaits).toHaveLength(1);
  act(() => { hook.result.current.adoptState(snapshot(5)); });
  expect(hook.result.current.testRunWaits).toEqual([]);
});

it("consumes a current-server dispatch failure revision before the next ordinary delta", () => {
  vi.stubGlobal("EventSource", Stream);
  const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  act(() => hook.result.current.adoptState(snapshot(3), { allowUnknownServerInstance: true }));
  const callsBefore = fetchState.mock.calls.length;
  const failure = { type: "testRunWaitResumeDispatchFailed", serverInstanceId: "test-server", revision: 4, sessionId: "owner", error: "cannot dispatch" };
  act(() => Stream.latest.emit(failure));
  expect(hook.result.current.testRunWaitFailures.owner).toEqual(failure);
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(4);
  act(() => Stream.latest.emit({ type: "testRunChanged", serverInstanceId: "test-server", revision: 5, run: makeTestRun({ state: "passed" }) }));
  expect(hook.result.current.testRuns[0].state).toBe("passed");
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(5);
  expect(fetchState).toHaveBeenCalledTimes(callsBefore);
});

it("repairs a real gap before a current-server dispatch failure without adopting its revision", async () => {
  vi.stubGlobal("EventSource", Stream);
  const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  act(() => hook.result.current.adoptState(snapshot(3), { allowUnknownServerInstance: true }));
  const callsBefore = fetchState.mock.calls.length;
  const failure = { type: "testRunWaitResumeDispatchFailed", serverInstanceId: "test-server", revision: 5, sessionId: "owner", error: "cannot dispatch" };
  act(() => Stream.latest.emit(failure));
  expect(hook.result.current.testRunWaitFailures.owner).toEqual(failure);
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(3);
  await waitFor(() => expect(fetchState).toHaveBeenCalledTimes(callsBefore + 1));
});

it.each(["server-b", undefined])("does not adopt a failure revision from an unproven server %s", serverInstanceId => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  act(() => hook.result.current.adoptState(snapshot(3), { allowUnknownServerInstance: true }));
  act(() => Stream.latest.emit({ type: "testRunWaitResumeDispatchFailed", serverInstanceId, revision: 4, sessionId: "owner", error: "cannot dispatch" }));
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(3);
});

it.each([3, 2])("retains dispatch failure at revision %s after consumed snapshot/delta at 3, without changing revision", failureRevision => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const wait = makeTestRunWait();
  act(() => hook.result.current.adoptState({ ...snapshot(1), testRunWaits: [wait] }, { allowUnknownServerInstance: true }));
  // Compatibility with older hosts/replays: snapshot R removes the wait,
  // consumed(R), failed(R) (or an older dispatch revision).
  act(() => hook.result.current.adoptState({ ...snapshot(3), testRunWaits: [] }));
  act(() => Stream.latest.emit({ type: "testRunWaitConsumed", serverInstanceId: "test-server", revision: 3, waitId: wait.id, sessionId: wait.sessionId, reason: "completed" }));
  const failure = { type: "testRunWaitResumeDispatchFailed", revision: failureRevision, sessionId: wait.sessionId, error: "cannot dispatch" };
  act(() => Stream.latest.emit(failure));
  expect(hook.result.current.testRunWaitFailures[wait.sessionId]).toEqual(failure);
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(3);
  const failures = hook.result.current.testRunWaitFailures;
  act(() => Stream.latest.emit(failure));
  expect(hook.result.current.testRunWaitFailures).toBe(failures);
  act(() => Stream.latest.emit({ type: "testRunChanged", revision: 4, run: makeTestRun() }));
  act(() => hook.result.current.adoptState(snapshot(5)));
  expect(hook.result.current.testRunWaitFailures).toBe(failures);
  expect(hook.result.current.testRunWaits).toEqual([]);
  act(() => hook.result.current.dismissTestRunWaitFailure(wait.sessionId));
  expect(hook.result.current.testRunWaitFailures).toEqual({});
  act(() => Stream.latest.emit(failure));
  expect(hook.result.current.testRunWaitFailures).toEqual({});
  act(() => Stream.latest.emit({ ...failure, revision: 6, error: "new failure" }));
  expect(hook.result.current.testRunWaitFailures[wait.sessionId].error).toBe("new failure");
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(5);
});

it("keeps a failure a restarted server sent before its first snapshot and drops older or untagged ones", () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  act(() => hook.result.current.adoptState(snapshot(5), { allowUnknownServerInstance: true }));
  const failure = (sessionId: string, serverInstanceId?: string) => ({
    type: "testRunWaitResumeDispatchFailed" as const, revision: 1, sessionId,
    error: `cannot dispatch ${sessionId}`, ...(serverInstanceId ? { serverInstanceId } : {}),
  });
  act(() => Stream.latest.emit(failure("old-server", "test-server")));
  act(() => Stream.latest.emit(failure("untagged")));
  // The restarted server reports a failure before the UI adopts its snapshot.
  act(() => Stream.latest.emit(failure("restarted-server", "server-b")));
  expect(Object.keys(hook.result.current.testRunWaitFailures).sort())
    .toEqual(["old-server", "restarted-server", "untagged"]);
  act(() => hook.result.current.adoptState({ ...snapshot(1), serverInstanceId: "server-b" }, { allowUnknownServerInstance: true }));
  expect(hook.result.current.testRunWaitFailures).toEqual({ "restarted-server": failure("restarted-server", "server-b") });
});

it("keeps a restarted server's failure for a session whose old-server failure had a higher revision", () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  act(() => hook.result.current.adoptState(snapshot(240), { allowUnknownServerInstance: true }));
  const failure = (revision: number, serverInstanceId: string) => ({
    type: "testRunWaitResumeDispatchFailed" as const, revision, sessionId: "owner",
    error: "cannot dispatch", serverInstanceId,
  });
  act(() => Stream.latest.emit(failure(237, "test-server")));
  act(() => hook.result.current.dismissTestRunWaitFailure("owner"));
  expect(hook.result.current.testRunWaitFailures).toEqual({});
  // A restart rolled the revision back. The new server's failure for the same
  // session, with the same error, is new and arrives before its snapshot.
  act(() => Stream.latest.emit(failure(214, "server-b")));
  expect(hook.result.current.testRunWaitFailures).toEqual({ owner: failure(214, "server-b") });
  act(() => hook.result.current.adoptState({ ...snapshot(214), serverInstanceId: "server-b" }, { allowUnknownServerInstance: true }));
  expect(hook.result.current.testRunWaitFailures).toEqual({ owner: failure(214, "server-b") });
});

it.each([false, true])("rejects retired-server failures after restart (current failure: %s)", hasCurrentFailure => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const failure = (serverInstanceId: string) => ({
    type: "testRunWaitResumeDispatchFailed" as const, revision: 9, sessionId: "owner",
    error: `cannot dispatch on ${serverInstanceId}`, serverInstanceId,
  });
  act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
  act(() => Stream.latest.emit(failure("test-server")));
  act(() => hook.result.current.adoptState({ ...snapshot(1), serverInstanceId: "server-b" }, { allowUnknownServerInstance: true }));
  expect(hook.result.current.testRunWaitFailures).toEqual({});
  if (hasCurrentFailure) act(() => Stream.latest.emit(failure("server-b")));
  const currentFailures = hook.result.current.testRunWaitFailures;
  act(() => Stream.latest.emit(failure("test-server")));
  expect(hook.result.current.testRunWaitFailures).toBe(currentFailures);
  // A rejected late event must not poison deduplication of the current server.
  act(() => Stream.latest.emit(failure("server-b")));
  expect(hook.result.current.testRunWaitFailures).toEqual({ owner: failure("server-b") });
});

describe.each(["test-run", "delegation"] as const)("%s wait snapshot ordering", kind => {
  it("confirms live delivery from a stale current-server create after reopen while repairing waits", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    let resolve!: (state: api.StateResponse) => void;
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(done => { resolve = done; }));
    const options = params();
    options.stateSetters.setBackendConnectionState = vi.fn();
    options.stateSetters.setBackendConnectionIssueDetail = vi.fn();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(2), { allowUnknownServerInstance: true }));
    act(() => Stream.latest.onopen?.(new Event("open")));
    act(() => { Stream.latest.readyState = 0; Stream.latest.onerror?.(new Event("error")); });
    act(() => { Stream.latest.readyState = 1; Stream.latest.onopen?.(new Event("open")); });
    vi.mocked(options.stateSetters.setBackendConnectionState).mockClear();
    vi.mocked(options.stateSetters.setBackendConnectionIssueDetail).mockClear();
    vi.mocked(options.clearRecoveredBackendRequestError).mockClear();
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"], mode: "all", createdAt: "now",
    };
    act(() => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitCreated" : "delegationWaitCreated",
      wait: kind === "test-run" ? runWait : delegationWait, revision: 1, serverInstanceId: "test-server",
    }));
    // The live frame confirms transport before its separate HTTP repair completes.
    expect(options.stateSetters.setBackendConnectionState).toHaveBeenCalledWith("connected");
    expect(options.stateSetters.setBackendConnectionIssueDetail).toHaveBeenCalledWith(null);
    expect(options.clearRecoveredBackendRequestError).toHaveBeenCalled();
    expect(hook.result.current.testRunWaits).toEqual([]);
    expect(options.adoptionRefs.delegationWaitsRef.current).toEqual([]);
    expect(fetchState).toHaveBeenCalledTimes(1);
    await act(async () => resolve({ ...snapshot(2), testRunWaits: [runWait], delegationWaits: [delegationWait] }));
    expect(kind === "test-run" ? hook.result.current.testRunWaits : options.adoptionRefs.delegationWaitsRef.current)
      .toEqual([kind === "test-run" ? runWait : delegationWait]);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 8));
    expect(fetchState).toHaveBeenCalledTimes(1);
  });

  it("does not reuse a tagged replacement hint after a different server was adopted", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    const fetchState = vi.spyOn(api, "fetchState")
      .mockRejectedValueOnce(new Error("transient failure"))
      .mockResolvedValue({ ...snapshot(1), serverInstanceId: "server-b" });
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    await act(async () => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: "unknown-wait", sessionId: "owner", parentSessionId: "owner", reason: "completed",
      revision: 100, serverInstanceId: "server-b",
    }));
    act(() => hook.result.current.adoptState({ ...snapshot(7), serverInstanceId: "server-c" }, { allowUnknownServerInstance: true }));
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS));
    expect(fetchState).toHaveBeenCalledTimes(2);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("server-c");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(7);
    fetchState.mockResolvedValue({ ...snapshot(7), serverInstanceId: "server-c" });
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 2));
    expect(fetchState).toHaveBeenCalledTimes(3);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 4));
    expect(fetchState).toHaveBeenCalledTimes(3);
  });

  it.each(["rejected", "failed"] as const)("eventually repairs an unseen tagged wait after a %s probe and unrelated progress", async outcome => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    let resolve!: (state: api.StateResponse) => void;
    let reject!: (error: Error) => void;
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"], mode: "all", createdAt: "now",
    };
    const replacement = {
      ...snapshot(1), serverInstanceId: "server-b", testRunWaits: [runWait], delegationWaits: [delegationWait],
    };
    const fetchState = vi.spyOn(api, "fetchState")
      .mockImplementationOnce(() => new Promise((done, fail) => { resolve = done; reject = fail; }))
      .mockResolvedValue(replacement);
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    act(() => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitCreated" : "delegationWaitCreated",
      wait: kind === "test-run" ? runWait : delegationWait, revision: 100, serverInstanceId: "server-b",
    }));
    expect(fetchState).toHaveBeenCalledTimes(1);
    if (outcome === "rejected") {
      act(() => Stream.latest.emit({ type: "testRunChanged", revision: 11, run: makeTestRun() }));
      await act(async () => resolve(replacement));
    } else {
      await act(async () => reject(new Error("transient failure")));
      act(() => Stream.latest.emit({ type: "testRunChanged", revision: 11, run: makeTestRun() }));
    }
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("test-server");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(11);
    expect(hook.result.current.testRunWaits).toEqual([]);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS));
    expect(fetchState).toHaveBeenCalledTimes(2);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("server-b");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(1);
    expect(hook.result.current.testRunWaits).toEqual([runWait]);
    expect(options.adoptionRefs.delegationWaitsRef.current).toEqual([delegationWait]);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 4));
    expect(fetchState).toHaveBeenCalledTimes(2);
  });

  it.each(["unrelated delta", "SSE error"] as const)("keeps failed wait repair independent of an %s", async traffic => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    const fetchState = vi.spyOn(api, "fetchState")
      .mockRejectedValueOnce(new Error("transient failure"))
      .mockResolvedValue(traffic === "unrelated delta" ? snapshot(3) : { ...snapshot(1), serverInstanceId: "server-b" });
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"], mode: "all", createdAt: "now",
    };
    act(() => hook.result.current.adoptState({
      ...snapshot(1), testRunWaits: [runWait], delegationWaits: [delegationWait],
    }, { allowUnknownServerInstance: true }));
    const session: Session = {
      id: "owner", name: "Owner", agent: "Codex", emoji: "x", workdir: "/repo", model: "default",
      status: "idle", preview: "", messages: [], messageCount: 0, messagesLoaded: true,
    };
    act(() => hook.result.current.adoptCreatedSessionResponse({
      sessionId: session.id, session, revision: 2, serverInstanceId: "test-server",
    }));
    await act(async () => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: kind === "test-run" ? runWait.id : delegationWait.id,
      sessionId: "owner", parentSessionId: "owner", reason: "completed", revision: 100,
    }));
    expect(fetchState).toHaveBeenCalledTimes(1);
    if (traffic === "unrelated delta") {
      act(() => Stream.latest.emit({ type: "testRunChanged", revision: 3, run: makeTestRun() }));
    } else {
      // A separate real outage must retain broad reconnect recovery.
      act(() => { Stream.latest.readyState = 0; Stream.latest.onerror?.(new Event("error")); });
    }
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS));
    expect(fetchState.mock.calls.length).toBeGreaterThan(1);
    expect(hook.result.current.testRunWaits).toEqual([]);
    expect(options.adoptionRefs.delegationWaitsRef.current).toEqual([]);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe(traffic === "unrelated delta" ? "test-server" : "server-b");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(traffic === "unrelated delta" ? 3 : 1);
    if (traffic === "SSE error") {
      const callsBefore = fetchState.mock.calls.length;
      await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 2));
      expect(fetchState.mock.calls.length).toBeGreaterThan(callsBefore);
    }
  });

  it("retains generic recovery when a revision gap coalesces with an untagged wait repair", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    let finishFirst!: (state: api.StateResponse) => void;
    const fetchState = vi.spyOn(api, "fetchState")
      .mockImplementationOnce(() => new Promise(resolve => { finishFirst = resolve; }))
      .mockRejectedValueOnce(new Error("coalesced fetch failed"))
      .mockResolvedValue({ ...snapshot(1), serverInstanceId: "server-b" });
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    act(() => options.requestActionRecoveryResyncRef.current());
    act(() => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: "unknown-wait", sessionId: "owner", parentSessionId: "owner", reason: "completed", revision: 100,
    }));
    act(() => Stream.latest.emit({ type: "testRunChanged", revision: 12, run: makeTestRun() }));
    await act(async () => finishFirst(snapshot(10)));
    expect(fetchState).toHaveBeenCalledTimes(2);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS));
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("server-b");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(1);
  });

  it("allows independent tagged replacement evidence after an untagged repair fails", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    const fetchState = vi.spyOn(api, "fetchState")
      .mockRejectedValueOnce(new Error("transient failure"))
      .mockResolvedValue({ ...snapshot(1), serverInstanceId: "server-b" });
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    const delta = {
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: "unknown-wait", sessionId: "owner", parentSessionId: "owner", reason: "completed", revision: 100,
    };
    await act(async () => Stream.latest.emit(delta));
    expect(fetchState).toHaveBeenCalledTimes(1);
    // The nonempty unseen identity supplies independent replacement evidence.
    await act(async () => Stream.latest.emit({ ...delta, serverInstanceId: "server-b" }));
    expect(fetchState).toHaveBeenCalledTimes(2);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("server-b");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(1);
  });

  it.each([
    { identity: undefined, responseServer: "server-b", responseRevision: 1, adopts: false },
    { identity: "", responseServer: "server-b", responseRevision: 10, adopts: false },
    { identity: undefined, responseServer: "server-b", responseRevision: 11, adopts: false },
    { identity: "", responseServer: "test-server", responseRevision: 9, adopts: false },
    { identity: undefined, responseServer: "test-server", responseRevision: 10, adopts: true },
    { identity: "", responseServer: "test-server", responseRevision: 10, adopts: true },
  ])("preserves missing-identity permissions through repeated failed fetches ($identity, $responseServer/$responseRevision)", async ({ identity, responseServer, responseRevision, adopts }) => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", Stream);
    const fetchState = vi.spyOn(api, "fetchState")
      .mockRejectedValueOnce(new Error("first transient failure"))
      .mockRejectedValueOnce(new Error("second transient failure"))
      .mockResolvedValue({ ...snapshot(responseRevision), serverInstanceId: responseServer });
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"], mode: "all", createdAt: "now",
    };
    act(() => hook.result.current.adoptState({
      ...snapshot(10), testRunWaits: [runWait], delegationWaits: [delegationWait],
    }, { allowUnknownServerInstance: true }));
    const waits = () => kind === "test-run" ? hook.result.current.testRunWaits : options.adoptionRefs.delegationWaitsRef.current;
    const before = waits();
    await act(async () => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: kind === "test-run" ? runWait.id : delegationWait.id,
      sessionId: "owner", parentSessionId: "owner", reason: "completed", revision: 100,
      serverInstanceId: identity,
    }));
    expect(fetchState).toHaveBeenCalledTimes(1);
    expect(waits()).toBe(before);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS));
    expect(fetchState).toHaveBeenCalledTimes(2);
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 2));
    expect(fetchState).toHaveBeenCalledTimes(3);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("test-server");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(10);
    if (adopts) expect(waits()).toEqual([]);
    else expect(waits()).toBe(before);
    // Rejected snapshots leave the obligation pending, without broadening it.
    // An accepted snapshot ends the repair independently of live-SSE proof.
    await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 4));
    expect(fetchState).toHaveBeenCalledTimes(adopts ? 3 : 4);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("test-server");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(10);
    if (!adopts) {
      fetchState.mockResolvedValue(snapshot(10));
      await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 8));
      expect(waits()).toEqual([]);
      const callsAfterRepair = fetchState.mock.calls.length;
      await act(async () => vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS * 16));
      expect(fetchState).toHaveBeenCalledTimes(callsAfterRepair);
    }
  });

  it.each([
    { serverInstanceId: undefined, created: true },
    { serverInstanceId: undefined, created: false },
    { serverInstanceId: "", created: true },
    { serverInstanceId: "", created: false },
    { serverInstanceId: "test-server", created: true, deltaRevision: 1 },
  ])("repairs an equal-revision snapshot after partial advancement (identity=$serverInstanceId, created=$created)", async ({ serverInstanceId, created, deltaRevision }) => {
    vi.stubGlobal("EventSource", Stream);
    let resolve!: (state: api.StateResponse) => void;
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(done => { resolve = done; }));
    const options = params();
    options.stateSetters.setDelegationWaits = vi.fn();
    const hook = renderHook(() => useAppLiveState(options));
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"], mode: "all", createdAt: "now",
    };
    act(() => hook.result.current.adoptState({
      ...snapshot(1), testRunWaits: created ? [] : [runWait], delegationWaits: created ? [] : [delegationWait],
    }, { allowUnknownServerInstance: true }));
    const session: Session = {
      id: "owner", name: "Owner", agent: "Codex", emoji: "x", workdir: "/repo", model: "default",
      status: "idle", preview: "", messages: [], messageCount: 0, messagesLoaded: true,
    };
    // A create response advances the global revision without refreshing waits.
    act(() => expect(hook.result.current.adoptCreatedSessionResponse({
      sessionId: session.id, session, revision: 2, serverInstanceId: "test-server",
    })).toBe("adopted"));
    const callsBefore = fetchState.mock.calls.length;
    const waits = () => kind === "test-run" ? hook.result.current.testRunWaits : options.adoptionRefs.delegationWaitsRef.current;
    const before = waits();
    const payload = kind === "test-run"
      ? created ? { type: "testRunWaitCreated", wait: runWait }
        : { type: "testRunWaitConsumed", waitId: runWait.id, sessionId: runWait.sessionId, reason: "completed" }
      : created ? { type: "delegationWaitCreated", wait: delegationWait }
        : { type: "delegationWaitConsumed", waitId: delegationWait.id, parentSessionId: "owner", reason: "completed" };
    act(() => Stream.latest.emit({ ...payload, serverInstanceId, revision: deltaRevision ?? 100 }));
    expect(waits()).toBe(before);
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(2);
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
    await act(async () => resolve({
      ...snapshot(2), testRunWaits: created ? [runWait] : [], delegationWaits: created ? [delegationWait] : [],
    }));
    expect(waits()).toEqual(created ? [kind === "test-run" ? runWait : delegationWait] : []);
    expect(options.stateSetters.setDelegationWaits).toHaveBeenLastCalledWith(created ? [delegationWait] : []);
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(2);
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("test-server");
  });

  it.each([
    { serverInstanceId: undefined, snapshotServerId: "test-server", revision: 11 },
    { serverInstanceId: "unseen-server", snapshotServerId: "unseen-server", revision: 2 },
    { serverInstanceId: "unseen-server", snapshotServerId: "unseen-server", revision: 10 },
    { serverInstanceId: "unseen-server", snapshotServerId: "unseen-server", revision: 11 },
  ])("repairs identity $serverInstanceId from the actual snapshot at revision $revision", async ({ serverInstanceId, snapshotServerId, revision }) => {
    vi.stubGlobal("EventSource", Stream);
    let resolve!: (state: api.StateResponse) => void;
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(done => { resolve = done; }));
    const options = params();
    options.stateSetters.setDelegationWaits = vi.fn();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    const callsBefore = fetchState.mock.calls.length;
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"], mode: "all", createdAt: "now",
    };
    const payload = kind === "test-run"
      ? { type: "testRunWaitCreated", wait: runWait }
      : { type: "delegationWaitCreated", wait: delegationWait };
    act(() => Stream.latest.emit({ ...payload, revision: 100, serverInstanceId }));
    expect(hook.result.current.testRunWaits).toEqual([]);
    expect(options.adoptionRefs.delegationWaitsRef.current).toEqual([]);
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(10);
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
    // Missing/unknown identity must not leave a revision-100 watermark behind.
    await act(async () => resolve({
      ...snapshot(revision), serverInstanceId: snapshotServerId,
      testRunWaits: [runWait], delegationWaits: [delegationWait],
    }));
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe(snapshotServerId);
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(revision);
    expect(hook.result.current.testRunWaits).toEqual([runWait]);
    expect(options.adoptionRefs.delegationWaitsRef.current).toEqual([delegationWait]);
    expect(options.stateSetters.setDelegationWaits).toHaveBeenLastCalledWith([delegationWait]);
  });

  it.each(["older response", "newer revision in flight", "replacement in flight"] as const)("does not roll back a missing-identity repair with %s", async scenario => {
    vi.stubGlobal("EventSource", Stream);
    let resolve!: (state: api.StateResponse) => void;
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(done => { resolve = done; }));
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    const callsBefore = fetchState.mock.calls.length;
    act(() => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: "unknown-wait", sessionId: "owner", parentSessionId: "owner", reason: "completed", revision: 100,
      serverInstanceId: "",
    }));
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
    const currentRevision = scenario === "newer revision in flight" ? 11 : 10;
    const currentServer = scenario === "replacement in flight" ? "server-b" : "test-server";
    if (scenario !== "older response") {
      act(() => hook.result.current.adoptState({
        ...snapshot(currentRevision), serverInstanceId: currentServer,
      }, { allowUnknownServerInstance: true }));
    }
    await act(async () => resolve({
      ...snapshot(scenario === "older response" ? 9 : 10), testRunWaits: [makeTestRunWait()],
    }));
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe(currentServer);
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(currentRevision);
    expect(hook.result.current.testRunWaits).toEqual([]);
  });

  it.each(["missing identity", "empty identity", "retired response", "newer state during fetch"] as const)("keeps the replacement adoption guard for %s", async scenario => {
    vi.stubGlobal("EventSource", Stream);
    let resolve!: (state: api.StateResponse) => void;
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(done => { resolve = done; }));
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState(snapshot(10), { allowUnknownServerInstance: true }));
    act(() => hook.result.current.adoptState({ ...snapshot(1), serverInstanceId: "server-b" }, { allowUnknownServerInstance: true }));
    const callsBefore = fetchState.mock.calls.length;
    act(() => Stream.latest.emit({
      type: kind === "test-run" ? "testRunWaitConsumed" : "delegationWaitConsumed",
      waitId: "unknown-wait", sessionId: "owner", parentSessionId: "owner", reason: "completed", revision: 100,
      serverInstanceId: scenario === "missing identity" ? undefined : scenario === "empty identity" ? "" : "unseen-server",
    }));
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
    if (scenario === "newer state during fetch") {
      act(() => hook.result.current.adoptState({ ...snapshot(2), serverInstanceId: "server-b" }));
    }
    await act(async () => resolve({
      ...snapshot(11), serverInstanceId: scenario === "retired response" ? "test-server" : "unseen-server",
      testRunWaits: [makeTestRunWait()],
    }));
    expect(options.adoptionRefs.lastSeenServerInstanceIdRef.current).toBe("server-b");
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(scenario === "newer state during fetch" ? 2 : 1);
    expect(hook.result.current.testRunWaits).toEqual([]);
  });

  it.each(["created", "consumed"] as const)("ignores a retired server's late %s delta without blocking current snapshots", async operation => {
    vi.stubGlobal("EventSource", Stream);
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
    const options = params();
    options.stateSetters.setDelegationWaits = vi.fn();
    const hook = renderHook(() => useAppLiveState(options));
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"],
      mode: "all", createdAt: "12:00:00",
    };
    const state = (revision: number, serverInstanceId: string): api.StateResponse => ({
      ...snapshot(revision), serverInstanceId, testRunWaits: [runWait], delegationWaits: [delegationWait],
    });
    const waits = () => kind === "test-run" ? hook.result.current.testRunWaits : options.adoptionRefs.delegationWaitsRef.current;
    act(() => hook.result.current.adoptState(state(10, "test-server"), { allowUnknownServerInstance: true }));
    act(() => hook.result.current.adoptState(state(1, "server-b"), { allowUnknownServerInstance: true }));
    const accepted = waits();
    const repairCount = fetchState.mock.calls.length;
    vi.mocked(options.stateSetters.setDelegationWaits).mockClear();
    const payload = kind === "test-run"
      ? operation === "created"
        ? { type: "testRunWaitCreated", wait: { ...runWait, id: "phantom" } }
        : { type: "testRunWaitConsumed", waitId: runWait.id, sessionId: runWait.sessionId, reason: "completed" }
      : operation === "created"
        ? { type: "delegationWaitCreated", wait: { ...delegationWait, id: "phantom" } }
        : { type: "delegationWaitConsumed", waitId: delegationWait.id, parentSessionId: delegationWait.parentSessionId, reason: "completed" };
    act(() => Stream.latest.emit({ ...payload, revision: 9, serverInstanceId: "test-server" }));
    expect(waits()).toBe(accepted);
    expect(options.stateSetters.setDelegationWaits).not.toHaveBeenCalled();
    expect(fetchState.mock.calls.length).toBe(repairCount);
    act(() => hook.result.current.adoptState({ ...state(2, "server-b"), testRunWaits: [], delegationWaits: [] }));
    expect(waits()).toEqual([]);
    if (kind === "delegation") expect(options.stateSetters.setDelegationWaits).toHaveBeenLastCalledWith([]);
    // Positive control: no earlier in-flight fetch can mask a repair request.
    act(() => Stream.latest.emit({ ...payload, revision: 3, serverInstanceId: "server-b" }));
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(repairCount + 1));
  });

  it.each(["created", "consumed", "consumed before loading"] as const)("preserves a %s delta across an older snapshot, then accepts its commit and a restart", operation => {
    vi.stubGlobal("EventSource", Stream);
    vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    const runWait = makeTestRunWait();
    const delegationWait: api.DelegationWaitRecord = {
      id: "delegation-wait", parentSessionId: "owner", delegationIds: ["child"],
      mode: "all", createdAt: "12:00:00", title: "Review",
    };
    const waitSnapshot = (revision: number, present: boolean): api.StateResponse => ({
      ...snapshot(revision),
      ...(kind === "test-run"
        ? { testRunWaits: present ? [runWait] : [] }
        : { delegationWaits: present ? [delegationWait] : [] }),
    });
    const waits = () => kind === "test-run"
      ? hook.result.current.testRunWaits
      : options.adoptionRefs.delegationWaitsRef.current;
    const created = operation === "created";
    act(() => hook.result.current.adoptState(waitSnapshot(1, operation === "consumed"), { allowUnknownServerInstance: true }));
    act(() => Stream.latest.emit(kind === "test-run"
      ? created
        ? { type: "testRunWaitCreated", serverInstanceId: "test-server", revision: 5, wait: runWait }
        : { type: "testRunWaitConsumed", serverInstanceId: "test-server", revision: 5, waitId: runWait.id, sessionId: runWait.sessionId, reason: "completed" }
      : created
        ? { type: "delegationWaitCreated", serverInstanceId: "test-server", revision: 5, wait: delegationWait }
        : { type: "delegationWaitConsumed", serverInstanceId: "test-server", revision: 5, waitId: delegationWait.id, parentSessionId: delegationWait.parentSessionId, reason: "completed" }));
    expect(waits()).toHaveLength(created ? 1 : 0);
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(1);
    // This is newer than the last complete state, but older than the wait delta.
    act(() => expect(hook.result.current.adoptState(waitSnapshot(4, !created))).toBe(true));
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(4);
    expect(waits()).toHaveLength(created ? 1 : 0);
    // The complete commit can also contain a sibling wait absent from the delta.
    const commit = waitSnapshot(5, created);
    if (kind === "test-run") commit.testRunWaits = [...commit.testRunWaits ?? [], { ...runWait, id: "sibling" }];
    else commit.delegationWaits = [...commit.delegationWaits ?? [], { ...delegationWait, id: "sibling" }];
    act(() => hook.result.current.adoptState(commit));
    expect(waits()).toHaveLength(created ? 2 : 1);
    // Server-local wait watermarks must reset even when the revision rolls back.
    act(() => hook.result.current.adoptState({ ...waitSnapshot(1, !created), serverInstanceId: "server-b" }, { allowUnknownServerInstance: true }));
    expect(waits()).toHaveLength(created ? 0 : 1);
  });
});

it("takes same-revision wait deltas after their snapshot without a transcript hydration", async () => {
  vi.stubGlobal("EventSource", Stream);
  const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const fetchSessionTail = vi.spyOn(api, "fetchSessionTail").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const first = makeTestRunWait();
  const second = makeTestRunWait({ id: "wait-two" });
  const flush = () => act(async () => { await Promise.resolve(); await Promise.resolve(); });
  act(() => hook.result.current.adoptState({ ...snapshot(1), testRunWaits: [first] }, { allowUnknownServerInstance: true }));
  await flush();
  const callsBefore = fetchState.mock.calls.length;
  // Backend order: commit_locked publishes snapshot R, then the delta at R.
  act(() => hook.result.current.adoptState({ ...snapshot(2), testRunWaits: [first, second] }));
  act(() => Stream.latest.emit({ type: "testRunWaitCreated", serverInstanceId: "test-server", revision: 2, wait: second }));
  act(() => hook.result.current.adoptState({ ...snapshot(3), testRunWaits: [second] }));
  act(() => Stream.latest.emit({ type: "testRunWaitConsumed", serverInstanceId: "test-server", revision: 3, waitId: first.id, sessionId: first.sessionId, reason: "completed" }));
  await flush();
  expect(hook.result.current.testRunWaits.map(wait => wait.id)).toEqual(["wait-two"]);
  expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(3);
  // A wait delta always asks for the authoritative snapshot, as delegation
  // deltas do; it never reloads a session transcript.
  await waitFor(() => expect(fetchState.mock.calls.length).toBeGreaterThan(callsBefore));
  expect(fetchSessionTail).not.toHaveBeenCalled();
});

describe("wait deltas delivered before the snapshot of their commit", () => {
  function setUp(waits: ReturnType<typeof makeTestRunWait>[]) {
    vi.stubGlobal("EventSource", Stream);
    const fetchState = vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
    const options = params();
    const hook = renderHook(() => useAppLiveState(options));
    act(() => hook.result.current.adoptState({ ...snapshot(1), testRunWaits: waits }, { allowUnknownServerInstance: true }));
    const callsBefore = fetchState.mock.calls.length;
    let resolve!: (state: api.StateResponse) => void;
    fetchState.mockImplementation(() => new Promise(done => { resolve = done; }));
    const consumed = (wait: ReturnType<typeof makeTestRunWait>, revision: number) => act(() => Stream.latest.emit({
      type: "testRunWaitConsumed", serverInstanceId: "test-server", revision, waitId: wait.id, sessionId: wait.sessionId, reason: "completed",
    }));
    return { options, hook, fetchState, callsBefore, consumed, resolve: (state: api.StateResponse) => resolve(state) };
  }

  it("does not move the revision and adopts the snapshot of the commit from the repair", async () => {
    const wait = makeTestRunWait();
    const { options, hook, fetchState, callsBefore, consumed, resolve } = setUp([wait]);
    consumed(wait, 2);
    expect(hook.result.current.testRunWaits).toEqual([]);
    // The rest of commit 2 (the queued resume prompt) is not in the delta.
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(1);
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
    await act(async () => resolve(snapshot(2, [makeTestRun()])));
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(2);
    expect(hook.result.current.testRuns).toHaveLength(1);
  });

  it("removes every sibling wait of one commit and adopts that commit's snapshot", async () => {
    const first = makeTestRunWait();
    const second = makeTestRunWait({ id: "wait-two" });
    const { options, hook, fetchState, callsBefore, consumed, resolve } = setUp([first, second]);
    consumed(first, 2);
    consumed(second, 2);
    expect(hook.result.current.testRunWaits).toEqual([]);
    await waitFor(() => expect(fetchState.mock.calls.length).toBeGreaterThan(callsBefore));
    await act(async () => resolve(snapshot(2, [makeTestRun()])));
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(2);
    expect(hook.result.current.testRuns).toHaveLength(1);
  });

  it("does not roll back to an older repair response after a newer snapshot arrived", async () => {
    const wait = makeTestRunWait();
    const { options, hook, fetchState, callsBefore, consumed, resolve } = setUp([wait]);
    consumed(wait, 2);
    await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
    act(() => hook.result.current.adoptState(snapshot(3, [])));
    await act(async () => resolve(snapshot(2, [makeTestRun()])));
    expect(options.adoptionRefs.latestStateRevisionRef.current).toBe(3);
    expect(hook.result.current.testRuns).toEqual([]);
  });
});

it("replays an idempotent card update after a same-revision snapshot without replaying older updates", () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const message: TestRunCardMessage = { id: "card", type: "testRun", schemaVersion: 1, author: "system", timestamp: "now", run: makeTestRunCard() };
  const session: Session = { id: "owner", name: "Owner", agent: "Codex", emoji: "x", workdir: "/repo", model: "default", status: "idle", preview: "run", messages: [message], messageCount: 1, messagesLoaded: true };
  options.adoptionRefs.sessionsRef.current = [session];
  // Broad state snapshots carry no transcript. The card is already retained
  // locally; only the subsequent same-revision delta updates its evidence.
  const summary: api.StateResponse["sessions"][number] = {
    id: session.id, name: session.name, agent: session.agent, emoji: session.emoji,
    workdir: session.workdir, model: session.model, status: session.status,
    preview: session.preview, messageCount: 1, queuePaused: false,
  };
  act(() => hook.result.current.adoptState({ ...snapshot(3), sessions: [summary] }, { allowUnknownServerInstance: true }));
  const delta = { type: "testRunCardUpdated", revision: 3, sessionId: session.id, messageId: message.id, messageIndex: 0, messageCount: 1, preview: "passed", run: { ...message.run, state: "passed" } };
  act(() => Stream.latest.emit(delta));
  expect(options.adoptionRefs.sessionsRef.current[0].messages[0]).toEqual({ ...message, run: delta.run });
  const accepted = options.adoptionRefs.sessionsRef.current;
  act(() => Stream.latest.emit(delta));
  expect(options.adoptionRefs.sessionsRef.current).toBe(accepted);
  act(() => Stream.latest.emit({ ...delta, revision: 2, run: message.run }));
  expect(options.adoptionRefs.sessionsRef.current).toBe(accepted);
});

it("does not refetch detail for unrelated snapshots but refreshes changed versions and equal-summary deltas", async () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const read = vi.spyOn(runsApi, "readTestRun").mockResolvedValue({ run: makeTestRun(), detail: makeTestRunDetail() });
  const options = params();
  let live!: ReturnType<typeof useAppLiveState>;
  function Harness() {
    live = useAppLiveState(options);
    return <TestRunsProvider snapshotReady runs={live.testRuns} open={vi.fn()}><TestRunsPanel projects={[]} sessions={[]} /></TestRunsProvider>;
  }
  render(<Harness />);
  act(() => { live.adoptState(snapshot(1, [makeTestRun()]), { allowUnknownServerInstance: true }); });
  fireEvent.click(within(screen.getByLabelText("Discovered runs")).getByRole("button", { name: /test-one/ }));
  await screen.findByText("Commands and preflight");
  const runs = live.testRuns;
  await act(async () => { live.adoptState(snapshot(2, [makeTestRun()])); });
  expect(live.testRuns).toBe(runs);
  expect(read).toHaveBeenCalledTimes(1);
  await act(async () => { live.adoptState(snapshot(4, [makeTestRun({ detailVersion: "detail-v2" })])); });
  expect(read).toHaveBeenCalledTimes(2);
  await act(async () => Stream.latest.emit({ type: "testRunChanged", revision: 5, run: makeTestRun({ detailVersion: "detail-v2" }) }));
  expect(read).toHaveBeenCalledTimes(3);
  // An old host has no evidence token: even an equal recovery snapshot must
  // invalidate rather than treating the previously read diagnostics as current.
  await act(async () => { live.adoptState(snapshot(6, [makeTestRun({ detailVersion: undefined })])); });
  expect(read).toHaveBeenCalledTimes(4);
});

it("adopts runs only at accepted revisions, removes by delta, and clears an omitted snapshot slice", async () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  act(() => { hook.result.current.adoptState(snapshot(1, [makeTestRun()]), { allowUnknownServerInstance: true }); });
  expect(hook.result.current.testRuns).toHaveLength(1);
  act(() => Stream.latest.emit({ type: "testRunChanged", revision: 2, run: makeTestRun({ state: "passed" }) }));
  expect(hook.result.current.testRuns[0].state).toBe("passed");
  const accepted = hook.result.current.testRuns[0];
  act(() => Stream.latest.emit({ type: "testRunChanged", revision: 1, run: makeTestRun() }));
  expect(hook.result.current.testRuns[0]).toBe(accepted);
  act(() => Stream.latest.emit({ type: "testRunChanged", revision: 3, run: { ...accepted } }));
  expect(hook.result.current.testRuns[0]).not.toBe(accepted);
  expect(hook.result.current.testRuns[0]).toEqual(accepted);
  act(() => Stream.latest.emit({ type: "testRunRemoved", revision: 4, runId: accepted.runId }));
  expect(hook.result.current.testRuns).toEqual([]);
  act(() => { hook.result.current.adoptState(snapshot(5, [makeTestRun()])); });
  act(() => { hook.result.current.adoptState(snapshot(6)); });
  expect(hook.result.current.testRuns).toEqual([]);
  act(() => { hook.result.current.adoptState(snapshot(5, [makeTestRun()])); });
  expect(hook.result.current.testRuns).toEqual([]);
});

it("requests a snapshot on a global revision gap without applying partial run state", async () => {
  vi.stubGlobal("EventSource", Stream);
  const fetchState = vi.spyOn(api, "fetchState").mockResolvedValue(snapshot(1));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  await act(async () => { hook.result.current.adoptState(snapshot(1), { allowUnknownServerInstance: true }); });
  const callsBefore = fetchState.mock.calls.length;
  let resolve!: (state: api.StateResponse) => void;
  fetchState.mockImplementation(() => new Promise(done => { resolve = done; }));
  act(() => Stream.latest.emit({ type: "testRunChanged", revision: 4, run: makeTestRun() }));
  expect(hook.result.current.testRuns).toEqual([]);
  await waitFor(() => expect(fetchState.mock.calls.length).toBe(callsBefore + 1));
  await act(async () => resolve(snapshot(4, [makeTestRun()])));
  expect(hook.result.current.testRuns).toHaveLength(1);
});
