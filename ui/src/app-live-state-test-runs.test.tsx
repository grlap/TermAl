// Test-run snapshot/delta integration through the real live-state hook and
// transport. No sessions are needed: runs share the app's global revision gate.
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as api from "./api";
import { useAppLiveState } from "./app-live-state";
import type { UseAppLiveStateParams } from "./app-live-state-types";
import { makeTestRun, makeTestRunDetail } from "./test-runs-fixtures";
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

it("does not refetch detail for unrelated snapshots but refreshes changed versions and equal-summary deltas", async () => {
  vi.stubGlobal("EventSource", Stream);
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const read = vi.spyOn(runsApi, "readTestRun").mockResolvedValue({ run: makeTestRun(), detail: makeTestRunDetail() });
  const options = params();
  let live!: ReturnType<typeof useAppLiveState>;
  function Harness() {
    live = useAppLiveState(options);
    return <TestRunsProvider runs={live.testRuns} open={vi.fn()}><TestRunsPanel projects={[]} sessions={[]} /></TestRunsProvider>;
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
