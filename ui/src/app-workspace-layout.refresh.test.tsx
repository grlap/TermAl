// Owns hook-level workspace-summary refresh/delete races: in-flight GET
// reuse, token/generation apply guards, list-versus-delete error
// provenance, and post-DELETE resync.
//
// Does not own: layout persistence/retry, App chrome integration, or
// the shared local-workspace fixture builder.
//
// Split out of: inline App.tsx refresh tests; fixture lives in
// app-workspace-layout.test-support.ts.
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import * as api from "./api";
import type { WorkspaceLayoutsResponse } from "./api";
import { useAppWorkspaceLayout } from "./app-workspace-layout";
import { paramsForLocalWorkspace } from "./app-workspace-layout.test-support";
import type { WorkspaceDeleteRequest } from "./workspace-delete-request";

function deferredLayouts() {
  let resolve!: (value: WorkspaceLayoutsResponse) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<WorkspaceLayoutsResponse>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(
    JSON.stringify({ error: "workspace layout not found" }),
    { status: 404, headers: { "Content-Type": "application/json" } },
  )));
  vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue({} as api.WorkspaceLayoutResponse);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.localStorage.clear();
});

it("reuses an in-flight workspace list GET while its request token is still current", async () => {
  const first = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValueOnce(first.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  let secondRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
    secondRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(secondRefresh).toBe(firstRefresh);
  expect(fetchSpy).toHaveBeenCalledTimes(1);

  await act(async () => {
    first.resolve({ workspaces: [] });
    await firstRefresh;
  });
});

it("starts a fresh list GET after delete invalidates the in-flight token", async () => {
  const first = deferredLayouts();
  const second = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(first.promise)
    .mockReturnValueOnce(second.promise)
    .mockResolvedValue({
      workspaces: [{
        id: "fresh",
        revision: 2,
        updatedAt: "today",
        controlPanelSide: "left",
      }],
    });
  const deletePending = deferredLayouts();
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });

  let secondRefresh!: Promise<void>;
  await act(async () => {
    secondRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(secondRefresh).not.toBe(firstRefresh);
  expect(fetchSpy).toHaveBeenCalledTimes(2);

  let reusedRefresh!: Promise<void>;
  await act(async () => {
    first.resolve({
      workspaces: [{
        id: "stale",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
    await firstRefresh;
    reusedRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(reusedRefresh).toBe(secondRefresh);
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  expect(result.current.workspaceSummaries.some((summary) => summary.id === "stale")).toBe(false);

  await act(async () => {
    second.resolve({
      workspaces: [{
        id: "fresh",
        revision: 2,
        updatedAt: "today",
        controlPanelSide: "left",
      }],
    });
    await secondRefresh;
    deletePending.resolve({ workspaces: [] });
  });
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual(["fresh"]);
});

it("starts a fresh list GET after a settled delete success or rejection", async () => {
  const successGet = deferredLayouts();
  const failureGet = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(successGet.promise)
    .mockReturnValueOnce(failureGet.promise);
  const deleteSuccess = deferredLayouts();
  const deleteFailure = deferredLayouts();
  const deleteSpy = vi.spyOn(api, "deleteWorkspaceLayout")
    .mockReturnValueOnce(deleteSuccess.promise)
    .mockReturnValueOnce(deleteFailure.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  expect(fetchSpy).toHaveBeenCalledTimes(0);
  await act(async () => {
    deleteSuccess.resolve({ workspaces: [] });
  });
  await act(async () => {
    successGet.resolve({ workspaces: [] });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-third");
  });
  await act(async () => {
    deleteFailure.reject(new Error("Delete failed."));
  });
  await act(async () => {
    failureGet.resolve({ workspaces: [] });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  expect(deleteSpy).toHaveBeenCalledTimes(2);
});

it("clears GET loading when a pre-DELETE GET settles after the apply token changes", async () => {
  const first = deferredLayouts();
  const deletePending = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValueOnce(first.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(result.current.isWorkspacesListLoading).toBe(true);
  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);
  expect(result.current.isWorkspacesListLoading).toBe(true);

  await act(async () => {
    first.resolve({ workspaces: [] });
    await firstRefresh;
  });
  expect(result.current.isWorkspacesListLoading).toBe(false);
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);
});

it("starts a GET after DELETE and clears loading when that GET settles first", async () => {
  const deletePending = deferredLayouts();
  const laterGet = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValueOnce(laterGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  expect(result.current.isWorkspacesListLoading).toBe(false);
  await act(async () => {
    void result.current.refreshWorkspaceSummaries();
  });
  expect(result.current.isWorkspacesListLoading).toBe(true);
  await act(async () => {
    laterGet.resolve({ workspaces: [] });
  });
  expect(result.current.isWorkspacesListLoading).toBe(false);
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);
});

it("does not let a superseded GET clear a newer in-flight GET", async () => {
  const first = deferredLayouts();
  const second = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(first.promise)
    .mockReturnValueOnce(second.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  let secondRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
  });
  await act(async () => {
    secondRefresh = result.current.refreshWorkspaceSummaries({ forceFresh: true });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  expect(result.current.isWorkspacesListLoading).toBe(true);

  await act(async () => {
    first.resolve({
      workspaces: [{
        id: "stale",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
    await firstRefresh;
  });
  expect(result.current.isWorkspacesListLoading).toBe(true);
  expect(result.current.workspaceSummaries.some((summary) => summary.id === "stale")).toBe(false);

  await act(async () => {
    second.resolve({
      workspaces: [{
        id: "fresh",
        revision: 2,
        updatedAt: "today",
        controlPanelSide: "left",
      }],
    });
    await secondRefresh;
  });
  expect(result.current.isWorkspacesListLoading).toBe(false);
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual(["fresh"]);
});

it("clears GET loading when a remount GET finishes before a pending DELETE", async () => {
  const deletePending = deferredLayouts();
  const remountGet = deferredLayouts();
  const autoGet = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(remountGet.promise)
    .mockReturnValueOnce(autoGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  expect(result.current.isWorkspacesListLoading).toBe(false);
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);

  await act(async () => {
    void result.current.refreshWorkspaceSummaries();
  });
  expect(result.current.isWorkspacesListLoading).toBe(true);
  await act(async () => {
    remountGet.resolve({ workspaces: [] });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);
  expect(result.current.isWorkspacesListLoading).toBe(false);
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);

  await act(async () => {
    deletePending.resolve({ workspaces: [] });
  });
  await act(async () => {
    autoGet.resolve({ workspaces: [] });
  });
  expect(result.current.isWorkspacesListLoading).toBe(false);
  expect(result.current.deletingWorkspaceIds).toEqual([]);
});

it("starts a fresh explicit refresh after rename or SSE changes the list identity", async () => {
  const first = deferredLayouts();
  const second = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(first.promise)
    .mockReturnValueOnce(second.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);

  const nextSummaries = [{
    id: "from-sse",
    revision: 2,
    updatedAt: "now",
    controlPanelSide: "left" as const,
  }];
  await act(async () => {
    result.current.workspaceSummariesRef.current = nextSummaries;
    result.current.setWorkspaceSummaries(nextSummaries);
  });

  let secondRefresh!: Promise<void>;
  await act(async () => {
    secondRefresh = result.current.refreshWorkspaceSummaries();
  });
  expect(secondRefresh).not.toBe(firstRefresh);
  expect(fetchSpy).toHaveBeenCalledTimes(2);

  await act(async () => {
    first.resolve({
      workspaces: [{
        id: "stale-first-get",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
    await firstRefresh;
  });
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual(["from-sse"]);

  await act(async () => {
    second.resolve({
      workspaces: [
        ...nextSummaries,
        {
          id: "extra-row",
          revision: 3,
          updatedAt: "later",
          controlPanelSide: "left",
        },
      ],
    });
    await secondRefresh;
  });
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual([
    "from-sse",
    "extra-row",
  ]);
});

it("keeps the original delete error when automatic resync GET fails", async () => {
  const deletePending = deferredLayouts();
  const autoGet = deferredLayouts();
  const explicitGet = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(autoGet.promise)
    .mockReturnValueOnce(explicitGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  const deleteError = "Delete failed.";
  await act(async () => {
    deletePending.reject(new Error(deleteError));
  });
  expect(result.current.workspacesListError).toBe(deleteError);

  await act(async () => {
    autoGet.reject(new Error("Resync failed."));
  });
  expect(result.current.workspacesListError).toBe(deleteError);

  await act(async () => {
    void result.current.refreshWorkspaceSummaries();
  });
  expect(result.current.workspacesListError).toBeNull();
  await act(async () => {
    explicitGet.resolve({ workspaces: [] });
  });
  expect(result.current.workspacesListError).toBeNull();
});

it("keeps a delete error through automatic GET when B fails before A succeeds", async () => {
  const layouts = { workspaces: [] as WorkspaceLayoutsResponse["workspaces"] };
  const deleteA = deferredLayouts();
  const deleteB = deferredLayouts();
  const autoGet = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValue(autoGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout")
    .mockReturnValueOnce(deleteA.promise)
    .mockReturnValueOnce(deleteB.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-a");
    void result.current.handleDeleteWorkspace("workspace-b");
  });
  await act(async () => {
    deleteB.reject(new Error("Delete B failed."));
    deleteA.resolve(layouts);
  });
  await act(async () => {
    autoGet.resolve(layouts);
  });
  expect(result.current.workspacesListError).toMatch(/Delete B failed/i);
});

it("keeps a delete error through automatic GET when A succeeds before B fails", async () => {
  const layouts = { workspaces: [] as WorkspaceLayoutsResponse["workspaces"] };
  const deleteB = deferredLayouts();
  const deleteA = deferredLayouts();
  const autoGet = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValue(autoGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout")
    .mockReturnValueOnce(deleteB.promise)
    .mockReturnValueOnce(deleteA.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-b");
    void result.current.handleDeleteWorkspace("workspace-a");
  });
  await act(async () => {
    deleteA.resolve(layouts);
    deleteB.reject(new Error("Delete B failed."));
  });
  await act(async () => {
    autoGet.resolve(layouts);
  });
  expect(result.current.workspacesListError).toMatch(/Delete B failed/i);
});

it("keeps a single delete failure through automatic GET and clears it on explicit refresh", async () => {
  const deletePending = deferredLayouts();
  const autoGet = deferredLayouts();
  const explicitGet = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(autoGet.promise)
    .mockReturnValueOnce(explicitGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  await act(async () => {
    deletePending.reject(new Error("Delete failed."));
  });
  await act(async () => {
    autoGet.resolve({ workspaces: [] });
  });
  expect(result.current.workspacesListError).toMatch(/Delete failed/i);

  await act(async () => {
    void result.current.refreshWorkspaceSummaries();
  });
  expect(result.current.workspacesListError).toBeNull();
  await act(async () => {
    explicitGet.resolve({ workspaces: [] });
  });
  expect(result.current.workspacesListError).toBeNull();
});

it("clears a delete error on explicit refresh while the automatic GET is in flight", async () => {
  const deletePending = deferredLayouts();
  const autoGet = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValue(autoGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  await act(async () => {
    deletePending.reject(new Error("Delete failed."));
  });
  expect(result.current.workspacesListError).toMatch(/Delete failed/i);
  expect(fetchSpy).toHaveBeenCalledTimes(1);

  await act(async () => {
    void result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);
  expect(result.current.workspacesListError).toBeNull();
  await act(async () => {
    autoGet.resolve({ workspaces: [] });
  });
  expect(result.current.workspacesListError).toBeNull();
});

it("starts a fresh post-DELETE GET when the remount GET is still in flight", async () => {
  const remountGet = deferredLayouts();
  const postDeleteGet = deferredLayouts();
  const deletePending = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(remountGet.promise)
    .mockReturnValueOnce(postDeleteGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
    void result.current.refreshWorkspaceSummaries();
  });
  await act(async () => {
    deletePending.resolve({
      workspaces: [{
        id: "stale-from-delete",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  const summariesBeforeStaleRemount = result.current.workspaceSummaries;
  expect(summariesBeforeStaleRemount.some((summary) => summary.id === "stale-from-remount")).toBe(false);

  await act(async () => {
    remountGet.resolve({
      workspaces: [{
        id: "stale-from-remount",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
  });
  expect(result.current.workspaceSummaries).toBe(summariesBeforeStaleRemount);
  expect(result.current.workspaceSummaries.some((summary) => summary.id === "stale-from-remount")).toBe(false);
  expect(result.current.workspaceSummaries.some((summary) => summary.id === "appeared-on-server")).toBe(false);

  await act(async () => {
    postDeleteGet.resolve({
      workspaces: [{
        id: "appeared-on-server",
        revision: 3,
        updatedAt: "now",
        controlPanelSide: "left",
      }],
    });
  });
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual(["appeared-on-server"]);
});

it("starts a fresh post-DELETE GET after a remount GET finishes first", async () => {
  const remountGet = deferredLayouts();
  const postDeleteGet = deferredLayouts();
  const deletePending = deferredLayouts();
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(remountGet.promise)
    .mockReturnValueOnce(postDeleteGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
    void result.current.refreshWorkspaceSummaries();
  });
  await act(async () => {
    remountGet.resolve({
      workspaces: [{
        id: "stale-from-remount",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
  });
  await act(async () => {
    deletePending.resolve({
      workspaces: [{
        id: "stale-from-delete",
        revision: 1,
        updatedAt: "yesterday",
        controlPanelSide: "left",
      }],
    });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  await act(async () => {
    postDeleteGet.resolve({
      workspaces: [{
        id: "appeared-on-server",
        revision: 3,
        updatedAt: "now",
        controlPanelSide: "left",
      }],
    });
  });
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual(["appeared-on-server"]);
});

it("clears a list-GET error when a later preserveError GET succeeds", async () => {
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValueOnce({
      workspaces: [{
        id: "recovered-list",
        revision: 2,
        updatedAt: "now",
        controlPanelSide: "left",
      }],
    });
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    await result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);
  expect(result.current.workspacesListError).toMatch(/offline/i);

  await act(async () => {
    await result.current.refreshWorkspaceSummaries({ preserveError: true });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  expect(result.current.workspaceSummaries.map((summary) => summary.id)).toEqual(["recovered-list"]);
  expect(result.current.workspacesListError).toBeNull();
});

it("records the latest list error when preserveError GET fails after a list error", async () => {
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockRejectedValueOnce(new Error("offline"))
    .mockRejectedValueOnce(new Error("needs-restart"));
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    await result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);
  expect(result.current.workspacesListError).toMatch(/offline/i);

  await act(async () => {
    await result.current.refreshWorkspaceSummaries({ preserveError: true });
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  expect(result.current.workspacesListError).toMatch(/needs-restart/i);
});

it("does not let a stale GET failure overwrite a newer list error", async () => {
  const first = deferredLayouts();
  const second = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(first.promise)
    .mockReturnValueOnce(second.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  let secondRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
  });
  await act(async () => {
    secondRefresh = result.current.refreshWorkspaceSummaries({ forceFresh: true });
  });
  await act(async () => {
    second.reject(new Error("newer-offline"));
    await secondRefresh;
  });
  expect(result.current.workspacesListError).toMatch(/newer-offline/i);

  await act(async () => {
    first.reject(new Error("older-offline"));
    await firstRefresh;
  });
  expect(result.current.workspacesListError).toMatch(/newer-offline/i);
});

it("keeps a delete error when the automatic GET fails", async () => {
  const deletePending = deferredLayouts();
  const autoGet = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts").mockReturnValueOnce(autoGet.promise);
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    void result.current.handleDeleteWorkspace("workspace-other");
  });
  await act(async () => {
    deletePending.reject(new Error("Delete failed."));
  });
  expect(result.current.workspacesListError).toMatch(/Delete failed/i);

  await act(async () => {
    autoGet.reject(new Error("offline"));
  });
  expect(result.current.workspacesListError).toMatch(/Delete failed/i);
});

it("does not let a stale GET success clear a newer list error", async () => {
  const first = deferredLayouts();
  const second = deferredLayouts();
  vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockReturnValueOnce(first.promise)
    .mockReturnValueOnce(second.promise);
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let firstRefresh!: Promise<void>;
  let secondRefresh!: Promise<void>;
  await act(async () => {
    firstRefresh = result.current.refreshWorkspaceSummaries();
  });
  await act(async () => {
    secondRefresh = result.current.refreshWorkspaceSummaries({ forceFresh: true });
  });
  await act(async () => {
    second.reject(new Error("newer-offline"));
    await secondRefresh;
  });
  expect(result.current.workspacesListError).toMatch(/newer-offline/i);

  await act(async () => {
    first.resolve({ workspaces: [] });
    await firstRefresh;
  });
  expect(result.current.workspacesListError).toMatch(/newer-offline/i);
});

it("fetches again when refresh runs after a failed list GET", async () => {
  const fetchSpy = vi.spyOn(api, "fetchWorkspaceLayouts")
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValueOnce({ workspaces: [] });
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  await act(async () => {
    await result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(1);
  expect(result.current.workspacesListError).toMatch(/offline/i);

  await act(async () => {
    await result.current.refreshWorkspaceSummaries();
  });
  expect(fetchSpy).toHaveBeenCalledTimes(2);
  expect(result.current.workspacesListError).toBeNull();
});

it("reports whether a delete request started and when it completes", async () => {
  const deletePending = deferredLayouts();
  vi.spyOn(api, "deleteWorkspaceLayout").mockReturnValue(deletePending.promise);
  vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
  const { result } = renderHook(useAppWorkspaceLayout, { initialProps: paramsForLocalWorkspace() });
  await act(async () => {});

  let currentRequest!: WorkspaceDeleteRequest;
  await act(async () => {
    currentRequest = result.current.handleDeleteWorkspace("workspace-local-only");
  });
  expect(currentRequest.started).toBe(false);
  await expect(currentRequest.completed).resolves.toBeUndefined();
  expect(result.current.deletingWorkspaceIds).toEqual([]);

  let startedRequest!: WorkspaceDeleteRequest;
  await act(async () => {
    startedRequest = result.current.handleDeleteWorkspace("workspace-other");
  });
  expect(startedRequest.started).toBe(true);
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);

  let duplicateRequest!: WorkspaceDeleteRequest;
  await act(async () => {
    duplicateRequest = result.current.handleDeleteWorkspace("workspace-other");
  });
  expect(duplicateRequest.started).toBe(false);
  await expect(duplicateRequest.completed).resolves.toBeUndefined();
  expect(result.current.deletingWorkspaceIds).toEqual(["workspace-other"]);

  await act(async () => {
    deletePending.resolve({ workspaces: [] });
    await startedRequest.completed;
  });
  expect(result.current.deletingWorkspaceIds).toEqual([]);
});
