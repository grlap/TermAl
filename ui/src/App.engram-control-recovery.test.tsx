// Real App/settings consumers across removal, reload, reconnect and live cuts.
// Backend durability and provider-send proofs belong to the canonical producer.
import { act, cleanup, screen, within } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "./api";
import * as liveState from "./app-live-state";
import type { EngramControlRecoverySummary } from "./api";
import {
  EventSourceMock, ResizeObserverMock, clickAndSettle, createScheduledAnimationFrameMocks,
  dispatchOpenedStateEvent, flushUiWork, latestEventSource, makeSession, makeStateResponse,
  makeWorkspaceLayoutResponse, renderApp, stubScrollIntoView, withVerifiedNoReactActWarnings,
} from "./app-test-harness";

vi.mock("./MonacoDiffEditor", () => ({ MonacoDiffEditor: forwardRef(function EditorMock() { return <div />; }) }));
vi.mock("./MonacoCodeEditor", () => ({ MonacoCodeEditor: forwardRef(function EditorMock() { return <div />; }) }));
vi.mock("./acceptance-settings-api", () => ({
  getAcceptancePolicy: vi.fn().mockResolvedValue({ available: false, readerKey: "test", error: "Unavailable" }),
  saveEvaluatorDefaults: vi.fn(), changeAcceptancePolicy: vi.fn(),
}));

const project = { id: "repo", name: "Repository", rootPath: "C:/repo", remoteId: "local", engramDeclared: true };
const debt: EngramControlRecoverySummary = {
  projectId: project.id, sessionId: "removed-owner", operation: "turn_checkpoint",
  code: "existing_checkpoint_unconfirmed", excluded: true, settlementConfirmed: false,
  nextActions: ["inspect_session", "stop_session_if_running"],
};
const owner = makeSession(debt.sessionId, { projectId: project.id, status: "idle", name: "Original owner" });
const healthy = makeSession("healthy", { projectId: project.id, name: "Healthy session" });
function state(revision: number, entries = [debt], sessions = [healthy]) {
  return { ...makeStateResponse({ revision, projects: [project], sessions, orchestrators: [], workspaces: [] }),
    engramControlRecovery: entries, stateDiffBase: true };
}

const originalScrollTo = HTMLElement.prototype.scrollTo;
beforeEach(() => {
  const frames = createScheduledAnimationFrameMocks();
  vi.stubGlobal("requestAnimationFrame", frames.requestAnimationFrameMock);
  vi.stubGlobal("cancelAnimationFrame", frames.cancelAnimationFrameMock);
  vi.stubGlobal("EventSource", EventSourceMock);
  vi.stubGlobal("ResizeObserver", ResizeObserverMock);
  HTMLElement.prototype.scrollTo = vi.fn();
  EventSourceMock.instances = [];
  stubScrollIntoView();
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
  vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
  vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(makeWorkspaceLayoutResponse());
});
afterEach(async () => {
  await act(async () => { cleanup(); await flushUiWork(); });
  HTMLElement.prototype.scrollTo = originalScrollTo;
  window.localStorage.clear();
  vi.restoreAllMocks(); vi.unstubAllGlobals();
});

async function openRecovery(initial: ReturnType<typeof state>) {
  vi.spyOn(api, "fetchState").mockResolvedValue(initial);
  await renderApp();
  await dispatchOpenedStateEvent(latestEventSource(), initial);
  await clickAndSettle(screen.getByRole("button", { name: "Open preferences" }));
  await clickAndSettle(screen.getByRole("tab", { name: "Engram" }));
  return screen.getByRole("region", { name: "Engram control recovery" });
}

async function live(next: api.StateResponse & { stateDiffBase?: boolean }) {
  await act(async () => { latestEventSource().dispatchNamedEvent("state", next); await flushUiWork(); });
}

it("keeps actual settings debt after owner removal; live cuts only remove confirmed settlement", async () => {
  await withVerifiedNoReactActWarnings(async () => {
    const send = vi.spyOn(api, "sendMessage"), stop = vi.spyOn(api, "stopSession"), resume = vi.spyOn(api, "resumeSessionQueue");
    const recovery = await openRecovery(state(1, [debt], [owner, healthy]));
    expect(recovery).toHaveTextContent("Original owner");
    await live(state(2));
    expect(recovery).toHaveTextContent("Removed session");
    expect(recovery).toHaveTextContent("removed-owner");
    expect(within(recovery).queryByRole("button")).not.toBeInTheDocument();
    // A stale settlement must not hide a newer unresolved operation.
    await live({ ...state(1, [{ ...debt, settlementConfirmed: true }]), stateDiffBase: false });
    expect(recovery).toHaveTextContent("removed-owner");
    const next = state(3, [debt], [{ ...healthy, name: "Healthy session progressed" }]);
    await act(async () => {
      latestEventSource().dispatchNamedEvent("stateDiff", {
        baseRevision: 2, revision: 3, serverInstanceId: next.serverInstanceId,
        header: { ...next, pendingEngramMcpRevocationSessionIds: [], testRunWaits: [] },
        sessions: { changed: next.sessions, removed: [] }, delegations: { changed: [], removed: [] }, testRuns: { changed: [], removed: [] },
      });
      await flushUiWork();
    });
    expect(recovery).toHaveTextContent("removed-owner");
    await clickAndSettle(screen.getByRole("button", { name: "Close dialog" }));
    await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
    expect(screen.getAllByRole("button", { name: /Healthy session progressed/ })[0]).toBeVisible();
    await clickAndSettle(screen.getByRole("button", { name: "Open preferences" }));
    await clickAndSettle(screen.getByRole("tab", { name: "Engram" }));
    await act(async () => {
      latestEventSource().dispatchNamedEvent("stateDiff", {
        baseRevision: 3, revision: 4, serverInstanceId: next.serverInstanceId,
        header: { ...next, engramControlRecovery: [], pendingEngramMcpRevocationSessionIds: [], testRunWaits: [] },
        sessions: { changed: [], removed: [] }, delegations: { changed: [], removed: [] }, testRuns: { changed: [], removed: [] },
      });
      await flushUiWork();
    });
    expect(screen.queryByRole("region", { name: "Engram control recovery" })).not.toBeInTheDocument();
    expect(send).not.toHaveBeenCalled(); expect(stop).not.toHaveBeenCalled(); expect(resume).not.toHaveBeenCalled();
  });
});

it("hydrates orphan debt again after reload and keeps it through reconnect and disabled control", async () => {
  await withVerifiedNoReactActWarnings(async () => {
    await openRecovery(state(5));
    await act(async () => { cleanup(); await flushUiWork(); });
    const reloaded = state(6);
    const recovery = await openRecovery(reloaded);
    expect(recovery).toHaveTextContent("removed-owner");
    await act(async () => { latestEventSource().dispatchError(); latestEventSource().dispatchOpen(); await flushUiWork(); });
    await live({ ...state(7), projects: [{ ...project, engramOperatorDisabled: true }] });
    expect(recovery).toHaveTextContent("removed-owner");
    await live(state(8, [{ ...debt, settlementConfirmed: true, excluded: false }]));
    expect(screen.queryByRole("region", { name: "Engram control recovery" })).not.toBeInTheDocument();
  });
});

it("wires present-owner Stop and Inspect to the existing App handlers", async () => {
  await withVerifiedNoReactActWarnings(async () => {
    let finishStop!: (value: api.StateResponse) => void;
    const stop = vi.spyOn(api, "stopSession").mockImplementation(() => new Promise(resolve => { finishStop = resolve; }));
    const send = vi.spyOn(api, "sendMessage");
    const recovery = await openRecovery(state(1, [debt], [{ ...owner, status: "active" }, healthy]));
    const stopButton = within(recovery).getByRole("button", { name: "Stop running session removed-owner" });
    await clickAndSettle(stopButton);
    expect(stop).toHaveBeenCalledExactlyOnceWith(owner.id);
    expect(stopButton).toBeDisabled();
    await act(async () => { finishStop(state(2, [debt], [owner, healthy])); await flushUiWork(); });
    expect(recovery).toHaveTextContent("removed-owner");
    expect(within(recovery).queryByRole("button", { name: "Stop running session removed-owner" })).not.toBeInTheDocument();
    await clickAndSettle(within(recovery).getByRole("button", { name: "Inspect session removed-owner" }));
    expect(screen.queryByRole("dialog", { name: "Settings" })).not.toBeInTheDocument();
    expect(await screen.findByRole("textbox", { name: "Message Original owner" })).toBeVisible();
    expect(send).not.toHaveBeenCalled();
  });
});

it("keeps canonical recovery array identity across equal full snapshots and omitted empty slices", async () => {
  await withVerifiedNoReactActWarnings(async () => {
    const original = liveState.useAppLiveState;
    let current: readonly EngramControlRecoverySummary[] = [];
    vi.spyOn(liveState, "useAppLiveState").mockImplementation(params => {
      const result = original(params); current = result.engramControlRecovery; return result;
    });
    await openRecovery(state(1));
    const retained = current;
    expect(retained).toEqual([debt]);
    await live(state(2, [{ ...debt, nextActions: [...debt.nextActions] }]));
    expect(current).toBe(retained);
    await act(async () => {
      latestEventSource().dispatchNamedEvent("stateDiff", {
        baseRevision: 2, revision: 3, serverInstanceId: "test-instance",
        header: { ...state(3, [{ ...debt }]), pendingEngramMcpRevocationSessionIds: [], testRunWaits: [] },
        sessions: { changed: [], removed: [] }, delegations: { changed: [], removed: [] }, testRuns: { changed: [], removed: [] },
      });
      await flushUiWork();
    });
    expect(current).toBe(retained);
    await live(state(4, []));
    expect(current).toEqual([]);
    const empty = current;
    await live({ ...state(5, []), engramControlRecovery: undefined });
    expect(current).toBe(empty);
  });
});
