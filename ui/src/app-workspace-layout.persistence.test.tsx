import { act, cleanup, renderHook } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "./api";
import { ApiRequestError, createBackendUnavailableError } from "./api-request";
import { useAppWorkspaceLayout, type UseAppWorkspaceLayoutParams } from "./app-workspace-layout";
import { createInitialWorkspaceBootstrap } from "./initial-workspace-bootstrap";
import { hasPendingWorkspaceLayout, persistWorkspaceLayout, WorkspaceLayoutStorageReadError } from "./workspace-storage";
import type { WorkspaceState } from "./workspace-types";
import type { ControlPanelSide } from "./workspace-storage";
import { makeSession } from "./app-test-harness";

const workspace: WorkspaceState = {
  lastContentPaneId: "pane-session", lastViewerPaneId: null,
  activePaneId: "pane-session", root: { type: "pane", paneId: "pane-session" },
  panes: [{ id: "pane-session", activeTabId: "tab-session", activeSessionId: "session-1",
    tabs: [{ id: "tab-session", kind: "session", sessionId: "session-1" }],
    viewMode: "session", lastSessionViewMode: "session", sourcePath: null }],
};

function paramsForLocalWorkspace(): UseAppWorkspaceLayoutParams {
  const workspaceViewId = "workspace-local-only";
  persistWorkspaceLayout(workspaceViewId, { controlPanelSide: "left", workspace });
  const initial = createInitialWorkspaceBootstrap(workspaceViewId);
  return {
    workspaceViewId, workspace: initial.workspace, setWorkspace: vi.fn(),
    sessions: [], sessionsRef: { current: [] }, isSessionStateReady: false,
    controlPanelSide: "left", setControlPanelSide: vi.fn(),
    preferences: initial,
    setPreferences: {
      setThemeId: vi.fn(), setLightThemeId: vi.fn(), setDarkThemeId: vi.fn(),
      setThemeMode: vi.fn(), setStyleId: vi.fn(), setMarkdownThemeId: vi.fn(),
      setMarkdownStyleId: vi.fn(), setDiagramThemeOverrideMode: vi.fn(),
      setDiagramLook: vi.fn(), setDiagramPalette: vi.fn(), setFontSizePx: vi.fn(),
      setEditorFontSizePx: vi.fn(), setDensityPercent: vi.fn(),
    },
    setIsWorkspaceSwitcherOpen: vi.fn(), setRequestError: vi.fn(),
    isMountedRef: { current: true }, clearRecoveredBackendRequestError: vi.fn(),
    setBackendConnectionState: vi.fn(), reportRequestError: vi.fn(),
    applyControlPanelLayout: (value) => value,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(
    JSON.stringify({ error: "workspace layout not found" }),
    { status: 404, headers: { "Content-Type": "application/json" } },
  )));
  vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue({} as api.WorkspaceLayoutResponse);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.localStorage.clear();
});

it("creates a locally restored workspace after JSON 404 while equivalent layouts keep arriving", async () => {
  const params = paramsForLocalWorkspace();
  const { result, rerender } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  expect(result.current.isWorkspaceLayoutReady).toBe(true);
  for (let index = 0; index < 5; index += 1) {
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender({ ...params, workspace: structuredClone(params.workspace) });
  }
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(1);
  expect(api.saveWorkspaceLayout).toHaveBeenCalledWith("workspace-local-only",
    expect.objectContaining({ workspace: params.workspace }), undefined);
});

it("saves a real layout edit after ignoring equivalent snapshot reconciliations", async () => {
  const params = paramsForLocalWorkspace();
  const { rerender } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { vi.advanceTimersByTime(200); });
  vi.mocked(api.saveWorkspaceLayout).mockClear();
  rerender({ ...params, workspace: structuredClone(params.workspace) });
  await act(async () => { vi.advanceTimersByTime(200); });
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();

  const edited = structuredClone(params.workspace);
  edited.panes.find((pane) => pane.id === "pane-session")!.tabs.push({
    id: "tab-second", kind: "session", sessionId: "session-2",
  });
  rerender({ ...params, workspace: edited });
  await act(async () => { vi.advanceTimersByTime(200); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(1);
  expect(api.saveWorkspaceLayout).toHaveBeenCalledWith("workspace-local-only",
    expect.objectContaining({ workspace: edited }), undefined);
});

it("keeps a pending layout available for pagehide after an equivalent rerender", async () => {
  const params = paramsForLocalWorkspace();
  const { rerender } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  rerender({ ...params, workspace: structuredClone(params.workspace) });
  act(() => { window.dispatchEvent(new Event("pagehide")); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledWith("workspace-local-only",
    expect.objectContaining({ workspace: params.workspace }), { keepalive: true });
});

it("retries a failed initial save without needing another layout edit", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValueOnce(createBackendUnavailableError("offline"));
  const params = paramsForLocalWorkspace();
  const { rerender } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { vi.advanceTimersByTime(200); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(1);
  rerender({ ...params, workspace: structuredClone(params.workspace) });
  await act(async () => { vi.advanceTimersByTime(1000); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(2);
  expect(vi.mocked(api.saveWorkspaceLayout).mock.calls[1]).toEqual(
    vi.mocked(api.saveWorkspaceLayout).mock.calls[0],
  );
});

it("lets a new edit replace a failed save and cancels retries on unmount", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValue(createBackendUnavailableError("offline"));
  const params = paramsForLocalWorkspace();
  const { rerender, unmount } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { vi.advanceTimersByTime(200); });
  const edited = structuredClone(params.workspace);
  edited.panes[0].tabs.push({ id: "tab-new", kind: "session", sessionId: "session-2" });
  rerender({ ...params, workspace: edited });
  await act(async () => { vi.advanceTimersByTime(200); });
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith("workspace-local-only",
    expect.objectContaining({ workspace: edited }), undefined);
  unmount();
  await act(async () => { vi.advanceTimersByTime(30_000); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(2);
});

it.each([400, 401, 403, 404, 413, 422])("stops HTTP %i retries, retains local content and saves a later edit", async (status) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValue(
    new ApiRequestError("request-failed", "Layout rejected", { status }),
  );
  const params = paramsForLocalWorkspace();
  const { rerender } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { vi.advanceTimersByTime(200); });
  rerender({ ...params, workspace: structuredClone(params.workspace) });
  await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
  act(() => window.dispatchEvent(new Event("pagehide")));
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(1);
  expect(params.setRequestError).toHaveBeenCalledWith(expect.stringContaining("Layout rejected"));
  expect(createInitialWorkspaceBootstrap(params.workspaceViewId).workspace).toEqual(params.workspace);

  const edited = structuredClone(params.workspace);
  vi.mocked(api.saveWorkspaceLayout).mockResolvedValue({} as api.WorkspaceLayoutResponse);
  edited.panes[0].tabs.push({ id: "tab-revised", kind: "session", sessionId: "session-2" });
  rerender({ ...params, workspace: edited });
  await act(async () => { vi.advanceTimersByTime(200); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(2);
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: edited }), undefined);
  const errorMessage = vi.mocked(params.setRequestError).mock.calls.find(([value]) => typeof value === "string")![0];
  const clearError = vi.mocked(params.setRequestError).mock.lastCall![0];
  expect(typeof clearError).toBe("function");
  if (typeof clearError !== "function" || typeof errorMessage !== "string") throw new Error("Missing error lifecycle");
  expect(clearError(errorMessage)).toBeNull();
  expect(clearError("Unrelated later error")).toBe("Unrelated later error");
});

it.each([408, 429, 500, 503])("recovers a transient HTTP %i failure without another edit", async (status) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValueOnce(
    new ApiRequestError("request-failed", "Try later", { status }),
  );
  renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});
  await act(async () => { vi.advanceTimersByTime(200); });
  await act(async () => { vi.advanceTimersByTime(1000); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(2);
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(2);
});

it("does not loop on a malformed response and ignores a superseded save failure", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  let rejectOld!: (reason: unknown) => void;
  vi.mocked(api.saveWorkspaceLayout).mockImplementationOnce(() => new Promise((_, reject) => { rejectOld = reject; }));
  const params = paramsForLocalWorkspace();
  const { rerender } = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { vi.advanceTimersByTime(200); });
  const edited = structuredClone(params.workspace);
  edited.panes[0].tabs.push({ id: "tab-latest", kind: "session", sessionId: "session-2" });
  rerender({ ...params, workspace: edited });
  await act(async () => { vi.advanceTimersByTime(200); });
  await act(async () => rejectOld(new ApiRequestError("request-failed", "Old rejection", { status: 413 })));
  expect(params.setRequestError).not.toHaveBeenCalled();
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValue(new SyntaxError("Invalid JSON"));
  rerender({ ...params, controlPanelSide: "right" });
  await act(async () => { vi.advanceTimersByTime(200); });
  await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
  expect(api.saveWorkspaceLayout).toHaveBeenCalledTimes(3);
  expect(params.setRequestError).toHaveBeenCalledWith(expect.stringContaining("Invalid JSON"));
});

it.each([200, 503, 413])("handles a broken HTTP %i response body through the real save API", async (status) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.mocked(api.saveWorkspaceLayout).mockRestore();
  const save = vi.spyOn(api, "saveWorkspaceLayout");
  let writes = 0;
  const bodyError = new TypeError("response stream disconnected");
  vi.stubGlobal("fetch", vi.fn(async (_url, init?: RequestInit) => {
    if (init?.method !== "PUT") return new Response("{}", { status: 404 });
    if (++writes === 1) return new Response(new ReadableStream({
      start(controller) { controller.error(bodyError); },
    }), { status });
    return new Response("{}");
  }));
  const params = paramsForLocalWorkspace();
  renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(writes).toBe(status === 413 ? 1 : 2);
  await expect(save.mock.results[0].value).rejects.toMatchObject({
    name: "ApiRequestError", status, cause: bodyError,
  });
  if (status !== 413) expect(save.mock.calls[1]).toEqual(save.mock.calls[0]);
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(writes).toBe(status === 413 ? 1 : 2);
});

it("does not reclassify complete malformed JSON as a transport failure", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.mocked(api.saveWorkspaceLayout).mockRestore();
  const save = vi.spyOn(api, "saveWorkspaceLayout");
  vi.stubGlobal("fetch", vi.fn(async (_url, init?: RequestInit) =>
    new Response(init?.method === "PUT" ? "{broken json" : "{}", {
      status: init?.method === "PUT" ? 200 : 404,
    })));
  renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
  expect(save).toHaveBeenCalledOnce();
  await expect(save.mock.results[0].value).rejects.toBeInstanceOf(SyntaxError);
});

it("retains rejected local tabs across remount instead of replacing them with the old server layout", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const sessions = [makeSession("session-1"), makeSession("session-2")];
  const sessionsRef = { current: sessions };
  const serverWorkspace = structuredClone(params.workspace);
  vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "left", workspace: serverWorkspace,
  } }))));
  function useStatefulLayout() {
    const initial = createInitialWorkspaceBootstrap(params.workspaceViewId);
    const [current, setCurrent] = useState(initial.workspace);
    const hook = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions, sessionsRef, isSessionStateReady: true });
    return { ...hook, current, setCurrent };
  }
  const first = renderHook(useStatefulLayout);
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValue(new ApiRequestError("request-failed", "Too large", { status: 413 }));
  act(() => first.result.current.setCurrent((current) => {
    const edited = structuredClone(current);
    edited.panes.find((pane) => pane.id === "pane-session")!.tabs.push({
      id: "unsaved-tab", kind: "session", sessionId: "session-2",
    });
    return edited;
  }));
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  first.unmount();
  const second = renderHook(useStatefulLayout);
  await act(async () => {});
  expect(second.result.current.current.panes.flatMap((pane) => pane.tabs)).toContainEqual({
    id: "unsaved-tab", kind: "session", sessionId: "session-2",
  });
  expect(createInitialWorkspaceBootstrap(params.workspaceViewId).workspace.panes.flatMap((pane) => pane.tabs))
    .toContainEqual({ id: "unsaved-tab", kind: "session", sessionId: "session-2" });
  vi.mocked(api.saveWorkspaceLayout).mockResolvedValue({} as api.WorkspaceLayoutResponse);
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: second.result.current.current }), undefined);
});

it.each([false, true])("pauses adoption and saves when recovery storage is unreadable (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  vi.mocked(api.saveWorkspaceLayout).mockClear();
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue({ layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "right", workspace,
  } } as api.WorkspaceLayoutResponse);
  const originalGetItem = window.localStorage.getItem.bind(window.localStorage);
  const denyStorage = () => vi.spyOn(window.localStorage, "getItem").mockImplementation((key) => {
    if (key.endsWith(`:${params.workspaceViewId}`)) {
      throw new DOMException("Storage blocked", "SecurityError");
    }
    return originalGetItem(key);
  });
  if (!deferred) denyStorage();
  const hook = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  if (deferred) {
    expect(hook.result.current.isWorkspaceLayoutReady).toBe(false);
    denyStorage();
    hook.rerender({ ...params, isSessionStateReady: true });
  }
  await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(false);
  expect(params.setWorkspace).not.toHaveBeenCalled();
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  expect(hook.result.current.workspaceLayoutStorageError).toBe(new WorkspaceLayoutStorageReadError(null).message);
});

it("keeps a newer pending layout after an older success, including remount", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  let resolveOld!: (response: api.WorkspaceLayoutResponse) => void;
  vi.mocked(api.saveWorkspaceLayout).mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }));
  const first = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  const newer = structuredClone(params.workspace);
  newer.panes[0].tabs.push({ id: "newer-tab", kind: "session", sessionId: "session-2" });
  vi.mocked(api.saveWorkspaceLayout).mockRejectedValue(new ApiRequestError("request-failed", "Too large", { status: 413 }));
  first.rerender({ ...params, workspace: newer });
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  await act(async () => resolveOld({} as api.WorkspaceLayoutResponse));
  expect(hasPendingWorkspaceLayout(params.workspaceViewId)).toBe(true);
  first.unmount();
  vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "left", workspace: params.workspace,
  } }))));
  vi.mocked(api.saveWorkspaceLayout).mockResolvedValue({} as api.WorkspaceLayoutResponse);
  const restoredSessionsRef = { current: [makeSession("session-1"), makeSession("session-2")] };
  function useRestoredLayout() {
    const [current, setWorkspace] = useState(() => createInitialWorkspaceBootstrap(params.workspaceViewId).workspace);
    useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace, isSessionStateReady: true,
      sessionsRef: restoredSessionsRef });
    return current;
  }
  const second = renderHook(useRestoredLayout);
  await act(async () => {});
  expect(second.result.current.panes.flatMap((pane) => pane.tabs)).toContainEqual({
    id: "newer-tab", kind: "session", sessionId: "session-2",
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(hasPendingWorkspaceLayout(params.workspaceViewId)).toBe(false);
});

it.each([false, true])("resumes storage-paused persistence without losing edits (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const sessions = [makeSession("session-1"), makeSession("session-2")];
  params.sessionsRef.current = sessions;
  if (deferred) params.sessionsRef.current = [];
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue({ layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "right", workspace,
  } } as api.WorkspaceLayoutResponse);
  const read = window.localStorage.getItem.bind(window.localStorage);
  let blocked = !deferred;
  vi.spyOn(window.localStorage, "getItem").mockImplementation((key) => {
    if (blocked && key.endsWith(`:${params.workspaceViewId}`)) throw new DOMException("Blocked", "SecurityError");
    return read(key);
  });
  const hook = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  blocked = true;
  params.sessionsRef.current = sessions;
  hook.rerender({ ...params, sessions, isSessionStateReady: true });
  await act(async () => {});
  expect(hook.result.current.workspaceLayoutStorageError).toContain("paused");
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(false);
  const edited = structuredClone(params.workspace);
  edited.panes[0].tabs.push({ id: "paused-edit", kind: "session", sessionId: "session-2" });
  hook.rerender({ ...params, workspace: edited, sessions, isSessionStateReady: true });
  params.setRequestError(null);
  expect(hook.result.current.workspaceLayoutStorageError).toContain("paused");
  blocked = false;
  const denyWrite = vi.spyOn(window.localStorage, "setItem").mockImplementation(() => {
    throw new DOMException("Storage quota exceeded", "QuotaExceededError");
  });
  const fetchesBeforeRetry = vi.mocked(api.fetchWorkspaceLayout).mock.calls.length;
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(false);
  // jsdom's cross-realm DOMException uses getErrorMessage's safe fallback.
  expect(hook.result.current.workspaceLayoutStorageError).toBe(
    "Workspace save is paused. Could not preserve the workspace layout in this browser: The request failed. Keep this tab open and retry after restoring storage access.",
  );
  expect(api.fetchWorkspaceLayout).toHaveBeenCalledTimes(fetchesBeforeRetry);
  denyWrite.mockRestore();
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(hook.result.current.workspaceLayoutStorageError).toBeNull();
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(true);
  expect(params.setWorkspace).not.toHaveBeenCalled();
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: edited }), undefined);
  expect(hasPendingWorkspaceLayout(params.workspaceViewId)).toBe(false);
});

it("retries unchanged storage-paused hydration and still restores the server layout", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  params.sessionsRef.current = [makeSession("session-1")];
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue({ layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "right", workspace,
  } } as api.WorkspaceLayoutResponse);
  const blocked = vi.spyOn(window.localStorage, "getItem").mockImplementation(() => { throw new Error("Blocked"); });
  const hook = renderHook(useAppWorkspaceLayout, { initialProps: params });
  await act(async () => {});
  expect(hook.result.current.workspaceLayoutStorageError).toContain("paused");
  blocked.mockRestore();
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  expect(params.setWorkspace).toHaveBeenCalledOnce();
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(true);
  expect(hook.result.current.workspaceLayoutStorageError).toBeNull();
});

// Stateful recovery fixtures exercise both the rendered tree and the eventual
// PUT. Merely checking that an adoption setter was skipped misses prune effects.
function recoveryFixture(sessionReady = true) {
  const params = paramsForLocalWorkspace();
  const available = [makeSession("session-1"), makeSession("session-2")];
  params.sessionsRef.current = sessionReady ? available : [];
  const response = { layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "left", workspace,
  } } as api.WorkspaceLayoutResponse;
  const fetchLayout = vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(response);
  const read = window.localStorage.getItem.bind(window.localStorage);
  let blocked = true;
  vi.spyOn(window.localStorage, "getItem").mockImplementation((key) => {
    if (blocked && key.endsWith(`:${params.workspaceViewId}`)) throw new Error("Blocked recovery read");
    return read(key);
  });
  const hook = renderHook(({ ready }) => {
    const [current, setCurrent] = useState(params.workspace);
    const state = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions: ready ? available : [], isSessionStateReady: ready });
    return { ...state, current, setCurrent };
  }, { initialProps: { ready: sessionReady } });
  return { params, available, response, fetchLayout, hook, unblock: () => { blocked = false; } };
}

function addRecoveryTab(current: WorkspaceState) {
  const edited = structuredClone(current);
  edited.panes.find((pane) => pane.id === "pane-session")!.tabs.push({
    id: "during-retry", kind: "session", sessionId: "session-2",
  });
  return edited;
}

it.each([false, true])("preserves edits throughout a retry including deferred adoption (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = recoveryFixture(!deferred);
  await act(async () => {});
  expect(f.hook.result.current.workspaceLayoutStorageError).not.toBeNull();
  f.unblock();
  let resolve!: (value: api.WorkspaceLayoutResponse) => void;
  f.fetchLayout.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  if (deferred) await act(async () => resolve(f.response));
  act(() => f.hook.result.current.setCurrent(addRecoveryTab));
  const edited = f.hook.result.current.current;
  if (!deferred) await act(async () => resolve(f.response));
  else {
    f.params.sessionsRef.current = f.available;
    f.hook.rerender({ ready: true });
  }
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(f.hook.result.current.current).toEqual(edited);
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(f.params.workspaceViewId,
    expect.objectContaining({ workspace: edited }), undefined);
  expect(f.hook.result.current.workspaceLayoutStorageError).toBeNull();
});

it.each([false, true])("keeps failed adoption writes paused and can recover (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = recoveryFixture(!deferred);
  await act(async () => {});
  f.unblock();
  const before = f.hook.result.current.current;
  const denyWrite = vi.spyOn(window.localStorage, "setItem").mockImplementation(() => {
    throw new DOMException("No storage space", "QuotaExceededError");
  });
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  if (deferred) {
    f.params.sessionsRef.current = f.available;
    f.hook.rerender({ ready: true });
  }
  expect(f.hook.result.current.current).toEqual(before);
  expect(f.hook.result.current.isWorkspaceLayoutReady).toBe(false);
  expect(f.hook.result.current.workspaceLayoutStorageError).not.toBeNull();
  expect(f.hook.result.current.isWorkspaceLayoutRetrying).toBe(false);
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  denyWrite.mockRestore();
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(f.hook.result.current.isWorkspaceLayoutReady).toBe(true);
  expect(f.hook.result.current.workspaceLayoutStorageError).toBeNull();
  expect(api.saveWorkspaceLayout).toHaveBeenCalledOnce();
});

it.each([false, true])("bounds retry waits and fences abandoned results (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = recoveryFixture(!deferred);
  await act(async () => {});
  f.unblock();
  let resolve!: (value: api.WorkspaceLayoutResponse) => void;
  if (!deferred) f.fetchLayout.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
  expect(f.fetchLayout.mock.lastCall![1]!.signal!.aborted).toBe(true);
  expect(f.hook.result.current.isWorkspaceLayoutRetrying).toBe(false);
  expect(f.hook.result.current.isWorkspaceLayoutReady).toBe(false);
  expect(f.hook.result.current.workspaceLayoutStorageError).toContain("timed out");
  act(() => f.hook.result.current.setCurrent(addRecoveryTab));
  const edited = f.hook.result.current.current;
  if (!deferred) await act(async () => resolve(f.response));
  else {
    f.params.sessionsRef.current = f.available;
    f.hook.rerender({ ready: true });
  }
  expect(f.hook.result.current.current).toEqual(edited);
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(f.params.workspaceViewId,
    expect.objectContaining({ workspace: edited }), undefined);
  expect(f.hook.result.current.workspaceLayoutStorageError).toBeNull();
  f.hook.unmount();
  expect(vi.getTimerCount()).toBe(0);
});

it("does not reclassify current-session child tabs as restored tabs on storage retry", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const sessions = [makeSession("session-1"), makeSession("session-2", { parentDelegationId: "delegation-live" })];
  params.sessionsRef.current = sessions;
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
  const hook = renderHook(() => {
    const [current, setCurrent] = useState(params.workspace);
    const state = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions, isSessionStateReady: true });
    return { ...state, current, setCurrent };
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  vi.mocked(api.saveWorkspaceLayout).mockClear();
  const denyWrite = vi.spyOn(window.localStorage, "setItem").mockImplementation(() => { throw new Error("Blocked"); });
  act(() => hook.result.current.setCurrent(addRecoveryTab));
  act(() => hook.result.current.setCurrent((current) => {
    const edited = structuredClone(current);
    edited.panes.find((pane) => pane.id === "pane-session")!.tabs.push({
      id: "live-canvas", kind: "canvas", originSessionId: null,
      cards: [{ sessionId: "session-2", x: 20, y: 40 }],
    });
    return edited;
  }));
  const edited = hook.result.current.current;
  expect(hook.result.current.workspaceLayoutStorageError).not.toBeNull();
  denyWrite.mockRestore();
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(hook.result.current.current).toEqual(edited);
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: edited }), undefined);
});

it("does not mistake deferred server preference changes for user edits during retry", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = recoveryFixture(false);
  f.response.layout.controlPanelSide = "right";
  await act(async () => {});
  f.unblock();
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  expect(f.params.setControlPanelSide).not.toHaveBeenCalled();
  f.params.sessionsRef.current = f.available;
  f.hook.rerender({ ready: true });
  expect(f.params.setControlPanelSide).toHaveBeenLastCalledWith("right");
  expect(f.hook.result.current.isWorkspaceLayoutReady).toBe(true);
  expect(f.hook.result.current.workspaceLayoutStorageError).toBeNull();
});

it("aborts a retry on unmount and ignores a late result", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = recoveryFixture();
  await act(async () => {});
  f.unblock();
  let resolve!: (value: api.WorkspaceLayoutResponse) => void;
  f.fetchLayout.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  const signal = f.fetchLayout.mock.lastCall![1]!.signal!;
  expect(signal.aborted).toBe(false);
  f.hook.unmount();
  expect(signal.aborted).toBe(true);
  expect(vi.getTimerCount()).toBe(0);
  await act(async () => resolve(f.response));
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
});

it("cancels an outstanding retry through the real workspace GET API", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = recoveryFixture();
  await act(async () => {});
  f.unblock();
  f.fetchLayout.mockRestore();
  let requestSignal: AbortSignal | undefined;
  let cancelled = false;
  vi.stubGlobal("fetch", vi.fn((_url, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
    requestSignal = init?.signal ?? undefined;
    requestSignal?.addEventListener("abort", () => {
      cancelled = true;
      reject(requestSignal!.reason);
    }, { once: true });
  })));
  act(() => f.hook.result.current.retryWorkspaceLayoutStorage());
  expect(requestSignal).toBeDefined();
  await act(async () => { await vi.advanceTimersByTimeAsync(15_000); });
  expect(cancelled).toBe(true);
  expect(f.hook.result.current.isWorkspaceLayoutReady).toBe(false);
  expect(f.hook.result.current.isWorkspaceLayoutRetrying).toBe(false);
  expect(f.hook.result.current.workspaceLayoutStorageError).toContain("timed out");
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
});

it.each([false, true])("restores the server after an initial write failure without inventing user edits (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const available = [makeSession("session-1"), makeSession("session-2")];
  const serverWorkspace = addRecoveryTab(workspace);
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue({ layout: {
    id: params.workspaceViewId, revision: 1, workspace: serverWorkspace,
    controlPanelSide: "right", fontSizePx: 18, densityPercent: 120,
  } } as api.WorkspaceLayoutResponse);
  const write = window.localStorage.setItem.bind(window.localStorage);
  let blocked = true;
  vi.spyOn(window.localStorage, "setItem").mockImplementation((key, value) => {
    if (blocked && key.endsWith(`:${params.workspaceViewId}`)) throw new Error("Quota exceeded");
    write(key, value);
  });
  const hook = renderHook(({ ready }) => {
    const [current, setCurrent] = useState(params.workspace);
    const [side, setSide] = useState<ControlPanelSide>("left");
    const [font, setFont] = useState(params.preferences.fontSizePx);
    const [density, setDensity] = useState(params.preferences.densityPercent);
    params.sessionsRef.current = ready ? available : [];
    const state = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions: params.sessionsRef.current, isSessionStateReady: ready,
      controlPanelSide: side, setControlPanelSide: setSide,
      preferences: { ...params.preferences, fontSizePx: font, densityPercent: density },
      setPreferences: { ...params.setPreferences, setFontSizePx: setFont, setDensityPercent: setDensity },
    });
    return { ...state, current, side, font, density };
  }, { initialProps: { ready: !deferred } });
  await act(async () => {});
  if (deferred) hook.rerender({ ready: true });
  await act(async () => {});
  expect(hook.result.current.workspaceLayoutStorageError).not.toBeNull();
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  // The failed adoption must not publish half of the server layout. This
  // checks real committed React state, not whether a setter was called.
  expect(hook.result.current.side).toBe("left");
  expect(hook.result.current.font).toBe(params.preferences.fontSizePx);
  expect(hook.result.current.density).toBe(params.preferences.densityPercent);
  blocked = false;
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(hook.result.current.workspaceLayoutStorageError).toBeNull();
  expect(hook.result.current.current.panes.flatMap((pane) => pane.tabs))
    .toContainEqual({ id: "during-retry", kind: "session", sessionId: "session-2" });
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: hook.result.current.current,
      controlPanelSide: "right", fontSizePx: 18, densityPercent: 120 }), undefined);
});

it.each([false, true])("prunes server references adopted on retry when child metadata arrives later (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const serverWorkspace = addRecoveryTab(workspace);
  serverWorkspace.panes[0].tabs.push({ id: "restored-canvas", kind: "canvas",
    originSessionId: null, cards: [{ sessionId: "session-2", x: 20, y: 40 }] });
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue({ layout: {
    id: params.workspaceViewId, revision: 1, controlPanelSide: "left", workspace: serverWorkspace,
  } } as api.WorkspaceLayoutResponse);
  const read = window.localStorage.getItem.bind(window.localStorage);
  let blocked = true;
  vi.spyOn(window.localStorage, "getItem").mockImplementation((key) => {
    if (blocked && key.endsWith(`:${params.workspaceViewId}`)) throw new Error("Blocked read");
    return read(key);
  });
  const hook = renderHook(({ ready }) => {
    const [current, setCurrent] = useState(params.workspace);
    const [available, setAvailable] = useState([makeSession("session-1"), makeSession("session-2")]);
    params.sessionsRef.current = ready ? available : [];
    const state = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions: params.sessionsRef.current, isSessionStateReady: ready });
    return { ...state, current, setAvailable };
  }, { initialProps: { ready: !deferred } });
  await act(async () => {});
  blocked = false;
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  if (deferred) hook.rerender({ ready: true });
  expect(hook.result.current.current.panes.flatMap((pane) => pane.tabs))
    .toContainEqual({ id: "during-retry", kind: "session", sessionId: "session-2" });
  act(() => hook.result.current.setAvailable([
    makeSession("session-1"), makeSession("session-2", { parentDelegationId: "late-parent" }),
  ]));
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  const tabs = hook.result.current.current.panes.flatMap((pane) => pane.tabs);
  expect(tabs.some((tab) => tab.kind === "session" && tab.sessionId === "session-2")).toBe(false);
  expect(tabs.flatMap((tab) => tab.kind === "canvas" ? tab.cards : [])).toEqual([]);
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: hook.result.current.current }), undefined);
});

it.each([false, true])("keeps failed recovery paused until a later server adoption (deferred=%s)", async (deferred) => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const available = [makeSession("session-1"), makeSession("session-2")];
  const serverWorkspace = addRecoveryTab(workspace);
  const response = { layout: { id: params.workspaceViewId, revision: 1,
    workspace: serverWorkspace, controlPanelSide: "right" } } as api.WorkspaceLayoutResponse;
  const fetchLayout = vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(response);
  const write = window.localStorage.setItem.bind(window.localStorage);
  let blocked = true;
  vi.spyOn(window.localStorage, "setItem").mockImplementation((key, value) => {
    if (blocked && key.endsWith(`:${params.workspaceViewId}`)) throw new Error("Quota exceeded");
    write(key, value);
  });
  const hook = renderHook(({ ready }) => {
    const [current, setCurrent] = useState(params.workspace);
    const [side, setSide] = useState<ControlPanelSide>("left");
    params.sessionsRef.current = ready ? available : [];
    const state = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions: params.sessionsRef.current, isSessionStateReady: ready,
      controlPanelSide: side, setControlPanelSide: setSide });
    return { ...state, current, side };
  }, { initialProps: { ready: !deferred } });
  await act(async () => {});
  if (deferred) hook.rerender({ ready: true });
  await act(async () => {});
  expect(hook.result.current.workspaceLayoutStorageError).not.toBeNull();
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(false);
  expect(hasPendingWorkspaceLayout(params.workspaceViewId)).toBe(false);
  blocked = false;
  fetchLayout.mockRejectedValueOnce(new ApiRequestError("backend-unavailable", "Service unavailable", { status: 503 }));
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  act(() => window.dispatchEvent(new Event("pagehide")));
  await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
  // No user edit occurred. A recovered browser store cannot authorize the
  // older bootstrap tree to replace the server tree after a failed GET.
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(false);
  expect(hook.result.current.isWorkspaceLayoutRetrying).toBe(false);
  expect(hook.result.current.workspaceLayoutStorageError).toContain("Workspace recovery failed");
  expect(hasPendingWorkspaceLayout(params.workspaceViewId)).toBe(false);
  expect(hook.result.current.current).toEqual(params.workspace);
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(fetchLayout).toHaveBeenCalledTimes(3);
  expect(hook.result.current.workspaceLayoutStorageError).toBeNull();
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(true);
  expect(hook.result.current.current.panes.find((pane) => pane.id === "pane-session"))
    .toEqual(serverWorkspace.panes[0]);
  expect(hook.result.current.side).toBe("right");
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: hook.result.current.current, controlPanelSide: "right" }), undefined);
  hook.unmount();
  expect(vi.getTimerCount()).toBe(0);
});

it("keeps hydrated workspace consumers active while storage is paused and retry is pending", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const params = paramsForLocalWorkspace();
  const fetchLayout = vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
  const hook = renderHook(() => {
    const [current, setCurrent] = useState(params.workspace);
    const [sessions, setSessions] = useState([makeSession("session-1"), makeSession("session-2")]);
    params.sessionsRef.current = sessions;
    const state = useAppWorkspaceLayout({ ...params, workspace: current, setWorkspace: setCurrent,
      sessions, isSessionStateReady: true });
    return { ...state, current, setCurrent, setSessions };
  });
  await act(async () => {});
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  vi.mocked(api.saveWorkspaceLayout).mockClear();
  const denyWrite = vi.spyOn(window.localStorage, "setItem").mockImplementation(() => { throw new Error("Quota exceeded"); });
  act(() => hook.result.current.setCurrent(addRecoveryTab));
  expect(hook.result.current.workspaceLayoutStorageError).not.toBeNull();
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(true);
  // Initial session-1 was restored, session-2 was opened in this UI session.
  act(() => hook.result.current.setSessions([
    makeSession("session-1", { parentDelegationId: "restored-parent" }), makeSession("session-2"),
  ]));
  expect(hook.result.current.current.panes.flatMap((pane) => pane.tabs))
    .not.toContainEqual({ id: "tab-session", kind: "session", sessionId: "session-1" });
  denyWrite.mockRestore();
  fetchLayout.mockRejectedValueOnce(new ApiRequestError("backend-unavailable", "Service unavailable", { status: 503 }));
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  await act(async () => {});
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(true);
  expect(hook.result.current.isWorkspaceLayoutRetrying).toBe(false);
  expect(hook.result.current.workspaceLayoutStorageError).toContain("Workspace recovery failed");
  act(() => window.dispatchEvent(new Event("pagehide")));
  await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  let resolve!: (value: null) => void;
  fetchLayout.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  act(() => hook.result.current.retryWorkspaceLayoutStorage());
  expect(hook.result.current.isWorkspaceLayoutReady).toBe(true);
  act(() => window.dispatchEvent(new Event("pagehide")));
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(api.saveWorkspaceLayout).not.toHaveBeenCalled();
  await act(async () => resolve(null));
  await act(async () => { await vi.advanceTimersByTimeAsync(200); });
  expect(hook.result.current.workspaceLayoutStorageError).toBeNull();
  expect(api.saveWorkspaceLayout).toHaveBeenLastCalledWith(params.workspaceViewId,
    expect.objectContaining({ workspace: hook.result.current.current }), undefined);
});
