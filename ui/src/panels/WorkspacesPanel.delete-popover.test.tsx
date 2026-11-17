// Owns delete-popup behavior at the workspace panel boundary.
// Does not own the API adapter or overflow menu's separate placement.
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { WorkspacesPanelHarness, type WorkspacesPanelHarnessProps } from "./workspaces-panel-test-harness";

function props(): WorkspacesPanelHarnessProps {
  return {
    currentWorkspaceId: "current",
    summaries: [
      { id: "current", label: "Current", revision: 1, updatedAt: "", controlPanelSide: "left" },
      { id: "alpha", label: "Alpha", revision: 1, updatedAt: "", controlPanelSide: "left" },
      { id: "beta", label: "Beta", revision: 1, updatedAt: "", controlPanelSide: "left" },
    ],
    deletingWorkspaceIds: [],
    error: null,
    isLoading: false,
    onDeleteWorkspace: vi.fn(() => ({ started: false, completed: Promise.resolve() })),
    onRenameWorkspace: vi.fn().mockResolvedValue(undefined),
    onRefresh: vi.fn(),
    onOpenWorkspace: vi.fn(),
  };
}

function mockPopupGeometry(anchorTop: () => number) {
  vi.stubGlobal("innerWidth", 320);
  vi.stubGlobal("innerHeight", 240);
  const originalRect = HTMLElement.prototype.getBoundingClientRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.classList.contains("workspaces-panel-overflow-trigger")) {
      return new DOMRect(284, anchorTop(), 24, 28);
    }
    if (this.classList.contains("workspaces-panel-confirm")) {
      return new DOMRect(0, 0, 260, 100);
    }
    return originalRect.call(this);
  });
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("workspace delete popup", () => {
  it("opens from the keyboard without moving the list, and Escape returns focus to its entry", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanelHarness {...input} />);
    const list = screen.getByRole("list");
    const rows = within(list).getAllByRole("listitem");
    const trigger = screen.getByRole("button", { name: "Actions for workspace Alpha" });
    trigger.focus();
    await user.keyboard("{Enter}");
    const remove = screen.getByRole("menuitem", { name: "Delete workspace Alpha" });
    remove.focus();
    const cancelVisibilityAtFocus: string[] = [];
    const observeFocus = (event: FocusEvent) => {
      const target = event.target;
      if (target instanceof HTMLButtonElement && target.textContent === "Cancel") {
        const dialog = target.closest<HTMLElement>("[role='dialog']");
        if (dialog) cancelVisibilityAtFocus.push(getComputedStyle(dialog).visibility);
      }
    };
    document.addEventListener("focusin", observeFocus);
    try {
      await user.keyboard("{Enter}");
    } finally {
      document.removeEventListener("focusin", observeFocus);
    }
    expect(cancelVisibilityAtFocus).toEqual(["visible"]);

    const popup = screen.getByRole("dialog", { name: "Delete workspace alpha" });
    expect(popup.parentElement).toBe(document.body);
    expect(list.contains(popup)).toBe(false);
    const remainingRows = within(list).getAllByRole("listitem");
    expect(remainingRows).toHaveLength(rows.length);
    remainingRows.forEach((row, index) => expect(row).toBe(rows[index]));
    expect(popup).toHaveAccessibleDescription(
      "Delete Alpha? Only the saved layout is removed. Files are not deleted.",
    );
    expect(within(popup).getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
  });

  it("cancels on an outside pointer without swallowing its default action", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanelHarness {...input} />);
    const trigger = screen.getByRole("button", { name: "Actions for workspace Alpha" });
    await user.click(trigger);
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    expect(fireEvent.pointerDown(document.body)).toBe(true);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
  });

  it("keeps outside controls usable and focused during a pending delete", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => ({ started: true, completed: new Promise<void>(() => {}) }));
    const outsideAction = vi.fn();
    const pointerPrevented: boolean[] = [];
    const user = userEvent.setup();
    const view = render(<>
      <WorkspacesPanelHarness {...input} />
      <button onClick={outsideAction} onPointerDown={(event) => pointerPrevented.push(event.defaultPrevented)}>Outside action</button>
    </>);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(document.querySelector(".session-kill-popover-backdrop")).toBeNull();
    const outside = screen.getByRole("button", { name: "Outside action" });
    await user.click(outside);
    expect(outsideAction).toHaveBeenCalledOnce();
    expect(pointerPrevented).toEqual([false]);
    expect(outside).toHaveFocus();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    // Once the request settles, Escape also works from outside the panel.
    view.rerender(<>
      <WorkspacesPanelHarness {...input} deletingWorkspaceIds={["alpha"]} />
      <button onClick={outsideAction}>Outside action</button>
    </>);
    view.rerender(<>
      <WorkspacesPanelHarness {...input} />
      <button onClick={outsideAction}>Outside action</button>
    </>);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(outside).toHaveFocus();
  });

  it("remeasures the same anchor when a pending rename form closes without resize or scroll", async () => {
    mockPopupGeometry(() => document.querySelector(".workspace-label-form") ? 60 : 12);
    const input = props();
    let finishRename!: () => void;
    input.onRenameWorkspace = vi.fn(() => new Promise<void>((resolve) => { finishRename = resolve; }));
    const user = userEvent.setup();
    render(<WorkspacesPanelHarness {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Current" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    await user.click(screen.getByRole("button", { name: "Save label" }));
    const anchor = screen.getByRole("button", { name: "Actions for workspace Alpha" });
    await user.click(anchor);
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    const popup = screen.getByRole("dialog");
    expect(popup).toHaveStyle({ top: "96px" });
    const cancel = within(popup).getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    await act(async () => finishRename());
    expect(screen.queryByRole("form")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Actions for workspace Alpha" })).toBe(anchor);
    expect(popup).toHaveStyle({ top: "48px" });
    expect(cancel).toHaveFocus();
  });

  it("resolves a remounted anchor after filtering and follows it on later scroll", async () => {
    let anchorTop = 12;
    mockPopupGeometry(() => anchorTop);
    const user = userEvent.setup();
    render(<WorkspacesPanelHarness {...props()} />);
    const originalAnchor = screen.getByRole("button", { name: "Actions for workspace Alpha" });
    await user.click(originalAnchor);
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    const popup = screen.getByRole("dialog");
    expect(popup).toHaveStyle({ top: "48px" });
    const search = screen.getByRole("searchbox");
    search.focus();
    fireEvent.change(search, { target: { value: "Beta" } });
    expect(originalAnchor).not.toBeInTheDocument();
    expect(popup).toHaveStyle({ top: "48px" });
    anchorTop = 60;
    fireEvent.change(search, { target: { value: "" } });
    expect(screen.getByRole("button", { name: "Actions for workspace Alpha" })).not.toBe(originalAnchor);
    expect(popup).toHaveStyle({ top: "96px" });
    anchorTop = 80;
    act(() => window.dispatchEvent(new Event("scroll")));
    expect(popup).toHaveStyle({ top: "116px" });
    expect(search).toHaveFocus();
  });

  it("retains the popup position when the row disappears during deletion, then restores survivor focus", async () => {
    mockPopupGeometry(() => 12);
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => ({ started: true, completed: new Promise<void>(() => {}) }));
    const user = userEvent.setup();
    const view = render(<WorkspacesPanelHarness {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    const popup = screen.getByRole("dialog");
    expect(popup).toHaveStyle({ left: "48px", top: "48px" });
    const remaining = input.summaries.filter((summary) => summary.id !== "alpha");
    view.rerender(<WorkspacesPanelHarness {...input} deletingWorkspaceIds={["alpha"]} summaries={remaining} />);
    expect(screen.queryByRole("button", { name: "Actions for workspace Alpha" })).not.toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBe(popup);
    expect(popup).toHaveStyle({ left: "48px", top: "48px" });
    view.rerender(<WorkspacesPanelHarness {...input} summaries={remaining} />);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Actions for workspace Current" })).toHaveFocus();
  });

  it.each([false, true])("focuses a surviving entry when its row disappears (after settlement: %s)", async (removeAfterSettlement) => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => ({ started: true, completed: new Promise<void>(() => {}) }));
    const user = userEvent.setup();
    const view = render(<WorkspacesPanelHarness {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    const confirm = screen.getByRole("button", { name: "Confirm delete" });
    confirm.focus();
    await user.keyboard("{Enter}");
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("alpha");
    view.rerender(<WorkspacesPanelHarness {...input} deletingWorkspaceIds={["alpha"]} />);
    await user.keyboard("{Escape}");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    if (removeAfterSettlement) {
      view.rerender(<WorkspacesPanelHarness {...input} />);
      expect(screen.getByRole("dialog")).toBeInTheDocument();
    }
    view.rerender(
      <WorkspacesPanelHarness {...input} summaries={input.summaries.filter((summary) => summary.id !== "alpha")} />,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Actions for workspace Current" })).toHaveFocus();
  });

  it.each([
    { name: "first", label: "Alpha", top: 12, expectedTop: 48 },
    { name: "last", label: "Beta", top: 200, expectedTop: 92 },
  ])("keeps the $name entry's popup visible and follows resize/scroll without refocusing", async ({ label, top, expectedTop }) => {
    vi.stubGlobal("innerWidth", 320);
    vi.stubGlobal("innerHeight", 240);
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    let triggerTop = top;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.classList.contains("workspaces-panel-overflow-trigger")) {
        return new DOMRect(284, triggerTop, 24, 28);
      }
      if (this.classList.contains("workspaces-panel-confirm")) {
        return new DOMRect(0, 0, 260, 100);
      }
      return originalRect.call(this);
    });
    const user = userEvent.setup();
    render(<WorkspacesPanelHarness {...props()} />);
    await user.click(screen.getByRole("button", { name: `Actions for workspace ${label}` }));
    await user.click(screen.getByRole("menuitem", { name: `Delete workspace ${label}` }));
    const popup = screen.getByRole("dialog");
    expect(popup).toHaveStyle({ left: "48px", top: `${expectedTop}px` });
    const confirm = within(popup).getByRole("button", { name: "Confirm delete" });
    confirm.focus();
    triggerTop = 60;
    act(() => window.dispatchEvent(new Event("scroll")));
    expect(popup).toHaveStyle({ left: "48px", top: "96px" });
    expect(confirm).toHaveFocus();
    vi.stubGlobal("innerWidth", 300);
    act(() => window.dispatchEvent(new Event("resize")));
    expect(popup).toHaveStyle({ left: "28px", top: "96px" });
    expect(confirm).toHaveFocus();
  });
});
