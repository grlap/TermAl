import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createRef, type ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import { WorkspaceSwitcher } from "./workspace-shell-controls";

function props(): ComponentProps<typeof WorkspaceSwitcher> {
  return {
    currentWorkspaceId: "workspace-current",
    summaries: [
      { id: "workspace-current", label: "Backend", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-other", revision: 1, updatedAt: "today", controlPanelSide: "left" },
    ],
    deletingWorkspaceIds: [], error: null, isLoading: false, isOpen: true,
    switcherRef: createRef<HTMLDivElement>(), onDeleteWorkspace: vi.fn(),
    onRenameWorkspace: vi.fn().mockResolvedValue(undefined),
    onOpenNewWorkspaceHere: vi.fn(), onOpenNewWorkspaceWindow: vi.fn(),
    onOpenWorkspace: vi.fn(), onToggle: vi.fn(),
  };
}

describe("workspace labels", () => {
  it("uses labels for display while navigation keeps the saved workspace ID", () => {
    const input = props();
    render(<WorkspaceSwitcher {...input} />);
    expect(screen.getByRole("button", { name: "Workspace Backend" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Backend\s*Current\s*workspace-current/ }));
    expect(input.onOpenWorkspace).toHaveBeenCalledWith("workspace-current");
    expect(screen.queryByRole("button", { name: "Edit label for workspace workspace-other" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /^Edit label for workspace/ })).toHaveLength(1);
  });

  it("saves a trimmed label, waits for success, and supports clearing it", async () => {
    const input = props();
    let finishSave!: () => void;
    input.onRenameWorkspace = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => { finishSave = resolve; }));
    const view = render(<WorkspaceSwitcher {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit label for workspace workspace-current" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), { target: { value: "  Reviews  " } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    expect(input.onRenameWorkspace).toHaveBeenCalledWith("workspace-current", "Reviews");
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    await act(async () => finishSave());
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();

    view.rerender(<WorkspaceSwitcher {...input} summaries={input.summaries.map((summary) => ({ ...summary, label: "Reviews" }))} />);
    expect(screen.getByRole("button", { name: "Workspace Reviews" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Edit label for workspace workspace-current" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: " " } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    await waitFor(() => expect(input.onRenameWorkspace).toHaveBeenLastCalledWith("workspace-current", ""));
  });

  it("keeps the draft and reports save errors; Escape cancels without navigating", async () => {
    const input = props();
    input.onRenameWorkspace = vi.fn().mockRejectedValue(new Error("Backend needs restart"));
    render(<WorkspaceSwitcher {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit label for workspace workspace-current" }));
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
    const view = render(<WorkspaceSwitcher {...input} />);
    const rowIds = () => screen.getAllByRole("listitem").map((row) =>
      row.querySelector(".workspace-switcher-item-meta")?.textContent,
    );
    const initialRows = rowIds();
    const edit = screen.getByRole("button", { name: "Edit label for workspace workspace-current" });
    expect(edit.closest('[role="list"]')).toBeNull();
    fireEvent.click(edit);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Draft name" } });
    view.rerender(<WorkspaceSwitcher {...input} summaries={[...input.summaries].reverse().map((summary) => ({
      ...summary, updatedAt: "later", revision: summary.revision + 1,
    }))} />);
    expect(rowIds()).toEqual(initialRows);
    expect(screen.getByRole("textbox")).toHaveValue("Draft name");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Save label" })));
    expect(input.onRenameWorkspace).toHaveBeenCalledWith("workspace-current", "Draft name");
  });
});
