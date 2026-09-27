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
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("adopts pending waits, revision-gates wait state and clears omitted waits", () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const wait = makeTestRunWait();
  act(() => { hook.result.current.adoptState({ ...snapshot(1), testRunWaits: [wait] }, { allowUnknownServerInstance: true }); });
  expect(hook.result.current.testRunWaits).toEqual([wait]);
  act(() => Stream.latest.emit({ type: "testRunWaitCreated", revision: 2, wait: { ...wait, id: "second" } }));
  expect(hook.result.current.testRunWaits).toHaveLength(2);
  act(() => Stream.latest.emit({ type: "testRunWaitConsumed", revision: 3, waitId: wait.id, sessionId: wait.sessionId, reason: "sessionStopped" }));
  expect(hook.result.current.testRunWaits.map(value => value.id)).toEqual(["second"]);
  act(() => Stream.latest.emit({ type: "testRunWaitCreated", revision: 2, wait }));
  expect(hook.result.current.testRunWaits).toHaveLength(1);
  act(() => { hook.result.current.adoptState(snapshot(5)); });
  expect(hook.result.current.testRunWaits).toEqual([]);
});

it.each([3, 2])("retains dispatch failure at revision %s after consumed snapshot/delta at 3, without changing revision", failureRevision => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const options = params();
  const hook = renderHook(() => useAppLiveState(options));
  const wait = makeTestRunWait();
  act(() => hook.result.current.adoptState({ ...snapshot(1), testRunWaits: [wait] }, { allowUnknownServerInstance: true }));
  // Actual backend ordering: snapshot R removes the wait, consumed(R), failed(R)
  // (or an older dispatch revision). The failure is not a snapshot mutation.
  act(() => hook.result.current.adoptState({ ...snapshot(3), testRunWaits: [] }));
  act(() => Stream.latest.emit({ type: "testRunWaitConsumed", revision: 3, waitId: wait.id, sessionId: wait.sessionId, reason: "completed" }));
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
  act(() => Stream.latest.emit({ type: "testRunWaitCreated", revision: 2, wait: second }));
  act(() => hook.result.current.adoptState({ ...snapshot(3), testRunWaits: [second] }));
  act(() => Stream.latest.emit({ type: "testRunWaitConsumed", revision: 3, waitId: first.id, sessionId: first.sessionId, reason: "completed" }));
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
      type: "testRunWaitConsumed", revision, waitId: wait.id, sessionId: wait.sessionId, reason: "completed",
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
