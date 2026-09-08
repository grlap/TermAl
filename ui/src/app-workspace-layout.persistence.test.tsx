import { act, cleanup, renderHook } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "./api";
import { ApiRequestError, createBackendUnavailableError } from "./api-request";
import { useAppWorkspaceLayout, type UseAppWorkspaceLayoutParams } from "./app-workspace-layout";
import { createInitialWorkspaceBootstrap } from "./initial-workspace-bootstrap";
import { persistWorkspaceLayout } from "./workspace-storage";
import type { WorkspaceState } from "./workspace-types";
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
