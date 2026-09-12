// Owns hook-level regressions for rename editor lock, captured
// startRename snapshot lifetime, retry after rejection,
// consume-once restore intent, and mounted-view focus intent.
// Does not own panel confirm UI, overflow focus application,
// or App dock wiring. Evolved coverage for WorkspacesPanel.tsx
// rename lifetime, not a verbatim split.
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { WorkspaceLayoutSummary } from "../api";
import { useWorkspaceRenameEditor } from "./use-workspace-rename-editor";

function summary(overrides: Partial<WorkspaceLayoutSummary> = {}): WorkspaceLayoutSummary {
  return {
    id: "workspace-alpha",
    label: "Alpha",
    revision: 1,
    updatedAt: "yesterday",
    controlPanelSide: "left",
    ...overrides,
  };
}

function deferredRename() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("useWorkspaceRenameEditor", () => {
  it("takes the save lock before the adapter call so a same-tick duplicate cannot start", async () => {
    const onRenameWorkspace = vi.fn(() => new Promise<void>(() => {}));
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.startRename(summary());
    });
    act(() => {
      void result.current.saveLabel();
      void result.current.saveLabel();
    });

    expect(onRenameWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha", "Alpha");
    expect(result.current.isSavingLabel).toBe(true);
    expect(result.current.isSaveLocked()).toBe(true);
  });

  it("retries after a rejected save and the later success clears the editor", async () => {
    const first = deferredRename();
    const second = deferredRename();
    let started = 0;
    const onRenameWorkspace = vi.fn(() => {
      started += 1;
      return started === 1 ? first.promise : second.promise;
    });
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.startRename(summary());
    });
    act(() => {
      void result.current.saveLabel();
    });
    await act(async () => {
      first.reject(new Error("first failed"));
      await first.promise.catch(() => {});
    });
    expect(result.current.labelError).toBe("first failed");

    act(() => {
      result.current.setLabelDraft("Planning");
    });
    act(() => {
      void result.current.saveLabel();
    });
    expect(onRenameWorkspace).toHaveBeenCalledTimes(2);
    expect(onRenameWorkspace).toHaveBeenNthCalledWith(1, "workspace-alpha", "Alpha");
    expect(onRenameWorkspace).toHaveBeenNthCalledWith(2, "workspace-alpha", "Planning");
    expect(result.current.isSavingLabel).toBe(true);
    expect(result.current.labelError).toBeNull();

    await act(async () => {
      second.resolve();
      await second.promise;
    });
    expect(result.current.isSavingLabel).toBe(false);
    expect(result.current.editingWorkspaceId).toBeNull();
    expect(result.current.editingSummarySnapshot).toBeNull();
  });

  it("keeps draft and error after a rejected save and ignores cancel/start while pending", async () => {
    const pending = deferredRename();
    const onRenameWorkspace = vi.fn(() => pending.promise);
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.startRename(summary());
      result.current.setLabelDraft("Planning");
    });
    act(() => {
      void result.current.saveLabel();
    });
    expect(result.current.startRename(summary({ id: "workspace-beta", label: "Beta" }))).toBe(false);
    act(() => {
      result.current.cancelRename("workspace-alpha");
      result.current.clearUnlockedRename();
    });
    expect(result.current.editingWorkspaceId).toBe("workspace-alpha");
    expect(result.current.labelDraft).toBe("Planning");
    expect(result.current.editingSummarySnapshot?.id).toBe("workspace-alpha");

    await act(async () => {
      pending.reject(new Error("Backend needs restart"));
      await pending.promise.catch(() => {});
    });
    expect(result.current.isSavingLabel).toBe(false);
    expect(result.current.labelDraft).toBe("Planning");
    expect(result.current.labelError).toBe("Backend needs restart");
    expect(result.current.editingSummarySnapshot?.id).toBe("workspace-alpha");
  });

  it("keeps the startRename snapshot through draft edits and a rejected save", async () => {
    const pending = deferredRename();
    const onRenameWorkspace = vi.fn(() => pending.promise);
    const captured = summary();
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.startRename(captured);
      result.current.setLabelDraft("Planning");
    });
    expect(result.current.editingSummarySnapshot).toEqual(captured);
    act(() => {
      void result.current.saveLabel();
    });
    await act(async () => {
      pending.reject(new Error("Rename failed."));
      await pending.promise.catch(() => {});
    });
    expect(result.current.labelDraft).toBe("Planning");
    expect(result.current.labelError).toBe("Rename failed.");
    expect(result.current.editingSummarySnapshot).toEqual(captured);
  });

  it("consumes overflow restore intent once after cancel", () => {
    const { result } = renderHook(() => useWorkspaceRenameEditor({
      onRenameWorkspace: vi.fn().mockResolvedValue(undefined),
    }));

    act(() => {
      result.current.startRename(summary());
      result.current.cancelRename("workspace-alpha");
    });
    expect(result.current.takePendingRestoreFocusId()).toBe("workspace-alpha");
    expect(result.current.takePendingRestoreFocusId()).toBeNull();
  });

  it("does not consume restore intent while a save is locked", async () => {
    const pending = deferredRename();
    const onRenameWorkspace = vi.fn(() => pending.promise);
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.setEditorViewMounted(true);
      result.current.startRename(summary());
    });
    act(() => {
      void result.current.saveLabel();
    });
    expect(result.current.isSaveLocked()).toBe(true);
    expect(result.current.takePendingRestoreFocusId()).toBeNull();
    expect(result.current.takePendingRestoreLabelFocus()).toBe(false);

    await act(async () => {
      pending.resolve();
      await pending.promise;
    });
    expect(result.current.takePendingRestoreFocusId()).toBe("workspace-alpha");
    expect(result.current.takePendingRestoreFocusId()).toBeNull();
  });

  it("offers error label-focus intent once after a mounted rejection", async () => {
    const pending = deferredRename();
    const onRenameWorkspace = vi.fn(() => pending.promise);
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.setEditorViewMounted(true);
      result.current.startRename(summary());
    });
    act(() => {
      void result.current.saveLabel();
    });
    await act(async () => {
      pending.reject(new Error("Rename failed."));
      await pending.promise.catch(() => {});
    });
    expect(result.current.labelError).toBe("Rename failed.");
    expect(result.current.takePendingRestoreLabelFocus()).toBe(true);
    expect(result.current.takePendingRestoreLabelFocus()).toBe(false);
  });

  it("does not write overflow focus intent when save succeeds while the editor view is unmounted", async () => {
    const pending = deferredRename();
    const onRenameWorkspace = vi.fn(() => pending.promise);
    const { result } = renderHook(() => useWorkspaceRenameEditor({ onRenameWorkspace }));

    act(() => {
      result.current.setEditorViewMounted(true);
      result.current.startRename(summary());
    });
    act(() => {
      void result.current.saveLabel();
    });
    act(() => {
      result.current.setEditorViewMounted(false);
    });
    await act(async () => {
      pending.resolve();
      await pending.promise;
    });
    expect(result.current.editingWorkspaceId).toBeNull();
    expect(result.current.takePendingRestoreFocusId()).toBeNull();
    expect(result.current.takePendingRestoreLabelFocus()).toBe(false);
  });
});
