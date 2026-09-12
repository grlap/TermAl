import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";

import { WorkspacesPanel } from "./panels/WorkspacesPanel";

function props(): ComponentProps<typeof WorkspacesPanel> {
  return {
    currentWorkspaceId: "workspace-current",
    summaries: [
      { id: "workspace-current", label: "Backend", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-other", revision: 1, updatedAt: "today", controlPanelSide: "left" },
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

describe("workspace labels", () => {
  it("uses labels for display while navigation keeps the saved workspace ID", () => {
    const input = props();
    render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Backend (current)" }));
    expect(input.onOpenWorkspace).toHaveBeenCalledWith("workspace-current");
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace workspace-other" }));
    expect(screen.getByRole("menuitem", { name: "Rename" })).toBeInTheDocument();
  });

  it("saves a trimmed label, waits for success, and supports clearing it", async () => {
    const input = props();
    let finishSave!: () => void;
    input.onRenameWorkspace = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => { finishSave = resolve; }));
    const view = render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Backend" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), { target: { value: "  Reviews  " } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    expect(input.onRenameWorkspace).toHaveBeenCalledWith("workspace-current", "Reviews");
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    await act(async () => finishSave());
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();

    view.rerender(<WorkspacesPanel {...input} summaries={input.summaries.map((summary) => ({ ...summary, label: "Reviews" }))} />);
    expect(screen.getByRole("button", { name: "Reviews (current)" })).toBeInTheDocument();
    const currentRow = screen.getAllByRole("listitem").find((row) => within(row).queryByText("Current"))!;
    fireEvent.click(within(currentRow).getByRole("button", { name: "Actions for workspace Reviews" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: " " } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    await waitFor(() => expect(input.onRenameWorkspace).toHaveBeenLastCalledWith("workspace-current", ""));
  });

  it("keeps the draft and reports save errors; Escape cancels without navigating", async () => {
    const input = props();
    input.onRenameWorkspace = vi.fn().mockRejectedValue(new Error("Backend needs restart"));
    render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Backend" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Planning" } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(screen.getByRole("textbox")).toHaveValue("Planning");
    fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
  });

  it("keeps rows and the current-workspace editor stable while live summaries reorder", async () => {
    const input = props();
    input.summaries = [...input.summaries, { ...input.summaries[1], id: "workspace-another", label: "Reviews" }];
    const view = render(<WorkspacesPanel {...input} />);
    const rowIds = () => screen.getAllByRole("listitem").map((row) =>
      row.querySelector(".visually-hidden")?.textContent,
    );
    const initialRows = rowIds();
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Backend" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Draft name" } });
    view.rerender(<WorkspacesPanel {...input} summaries={[...input.summaries].reverse().map((summary) => ({
      ...summary, updatedAt: "later", revision: summary.revision + 1,
    }))} />);
    expect(rowIds()).toEqual(initialRows);
    expect(screen.getByRole("textbox")).toHaveValue("Draft name");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Save label" })));
    expect(input.onRenameWorkspace).toHaveBeenCalledWith("workspace-current", "Draft name");
  });
});
