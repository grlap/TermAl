// Owns hook-level coverage for owned-delete pending/generation/observed
// lifecycle. Does not own panel confirm UI, retained summaries, or focus.
// Split from WorkspacesPanel.tsx.
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { WorkspaceDeleteRequest } from "../workspace-delete-request";
import { useOwnedWorkspaceDeletes } from "./use-owned-workspace-deletes";

function deferredRequest(): WorkspaceDeleteRequest & {
  resolve: () => void;
  reject: (error: Error) => void;
} {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const completed = new Promise<void>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { started: true, completed, resolve, reject };
}

describe("useOwnedWorkspaceDeletes", () => {
  it("takes the pending lock before the adapter call so a same-id duplicate cannot start", () => {
    const onDeleteWorkspace = vi.fn(() => ({
      started: true,
      completed: new Promise<void>(() => {}),
    }));
    const { result } = renderHook(() => useOwnedWorkspaceDeletes({
      deletingWorkspaceIds: [],
      onDeleteWorkspace,
    }));

    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
      result.current.confirmOwnedDelete("workspace-alpha");
    });

    expect(onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(true);
  });

  it("releases the lock on no-start and a synchronous throw so a later confirm can start", () => {
    const onDeleteWorkspace = vi.fn((): WorkspaceDeleteRequest => {
      throw new Error("sync delete failed");
    });
    const { result } = renderHook(() => useOwnedWorkspaceDeletes({
      deletingWorkspaceIds: [],
      onDeleteWorkspace,
    }));

    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);

    onDeleteWorkspace.mockImplementationOnce(() => ({
      started: false,
      completed: Promise.resolve(),
    }));
    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(onDeleteWorkspace).toHaveBeenCalledTimes(2);
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);

    onDeleteWorkspace.mockImplementationOnce(() => ({
      started: true,
      completed: new Promise<void>(() => {}),
    }));
    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(onDeleteWorkspace).toHaveBeenCalledTimes(3);
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(true);
  });

  it("does not retain a lock from stale unowned delete observation", async () => {
    const onDeleteWorkspace = vi.fn((): WorkspaceDeleteRequest => ({
      started: false,
      completed: Promise.resolve(),
    }));
    const { result, rerender } = renderHook(
      ({ deletingWorkspaceIds }) => useOwnedWorkspaceDeletes({
        deletingWorkspaceIds,
        onDeleteWorkspace,
      }),
      { initialProps: { deletingWorkspaceIds: [] as string[] } },
    );

    rerender({ deletingWorkspaceIds: ["workspace-alpha"] });
    rerender({ deletingWorkspaceIds: [] });

    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(onDeleteWorkspace).toHaveBeenCalledTimes(1);
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);

    onDeleteWorkspace.mockImplementationOnce(() => {
      throw new Error("sync delete failed");
    });
    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(onDeleteWorkspace).toHaveBeenCalledTimes(2);
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);

    const completed = deferredRequest();
    onDeleteWorkspace.mockImplementationOnce(() => completed);
    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(onDeleteWorkspace).toHaveBeenCalledTimes(3);
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(true);
    await act(async () => {
      completed.resolve();
      await completed.completed;
    });
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);
  });

  it("keeps ownership after completion while observed or current deleting state remains", async () => {
    const first = deferredRequest();
    const onOwnedDeletesSettled = vi.fn();
    const onDeleteWorkspace = vi.fn(() => first);
    const { result, rerender } = renderHook(
      ({ deletingWorkspaceIds }) => useOwnedWorkspaceDeletes({
        deletingWorkspaceIds,
        onDeleteWorkspace,
        onOwnedDeletesSettled,
      }),
      { initialProps: { deletingWorkspaceIds: [] as string[] } },
    );

    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    rerender({ deletingWorkspaceIds: ["workspace-alpha"] });
    await act(async () => {
      first.resolve();
      await first.completed;
    });
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(true);
    expect(onOwnedDeletesSettled).not.toHaveBeenCalled();

    rerender({ deletingWorkspaceIds: [] });
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);
    expect(onOwnedDeletesSettled).toHaveBeenCalledExactlyOnceWith(["workspace-alpha"]);
  });

  it("does not let a late first completion unlock a newer same-id pending delete", async () => {
    const first = deferredRequest();
    const second = deferredRequest();
    let started = 0;
    const onDeleteWorkspace = vi.fn(() => {
      started += 1;
      return started === 1 ? first : second;
    });
    const { result, rerender } = renderHook(
      ({ deletingWorkspaceIds }) => useOwnedWorkspaceDeletes({
        deletingWorkspaceIds,
        onDeleteWorkspace,
      }),
      { initialProps: { deletingWorkspaceIds: [] as string[] } },
    );

    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    rerender({ deletingWorkspaceIds: ["workspace-alpha"] });
    rerender({ deletingWorkspaceIds: [] });
    act(() => {
      result.current.confirmOwnedDelete("workspace-alpha");
    });
    expect(onDeleteWorkspace).toHaveBeenCalledTimes(2);
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(true);

    await act(async () => {
      first.resolve();
      await first.completed;
    });
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(true);

    await act(async () => {
      second.resolve();
      await second.completed;
    });
    expect(result.current.isOwnedDeletePending("workspace-alpha")).toBe(false);
  });
});
