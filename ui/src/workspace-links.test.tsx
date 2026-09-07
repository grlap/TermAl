import { createEvent, fireEvent, render, screen, within } from "@testing-library/react";
import { createRef, type ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { WorkspaceSwitcher } from "./workspace-shell-controls";

function props(): ComponentProps<typeof WorkspaceSwitcher> {
  return {
    currentWorkspaceId: "workspace-current",
    summaries: [
      { id: "workspace-current", label: "Termal", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-empty", label: "Empty", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-cv", revision: 1, updatedAt: "today", controlPanelSide: "left" },
    ],
    deletingWorkspaceIds: [], error: null, isLoading: false, isOpen: true,
    switcherRef: createRef<HTMLDivElement>(), onDeleteWorkspace: vi.fn(),
    onRenameWorkspace: vi.fn().mockResolvedValue(undefined),
    onOpenNewWorkspaceHere: vi.fn(), onOpenNewWorkspaceWindow: vi.fn(),
    onOpenWorkspace: vi.fn(), onToggle: vi.fn(),
  };
}

const initialUrl = window.location.href;
afterEach(() => window.history.replaceState(null, "", initialUrl));

describe("saved workspace new-tab links", () => {
  it("links to each other saved ID while retaining the URL context", () => {
    window.history.replaceState(null, "", "/nested/termal/?workspace=workspace-current&mode=review#details");
    const sourceUrl = new URL(window.location.href);
    render(<WorkspaceSwitcher {...props()} />);

    for (const [label, id] of [["Empty", "workspace-empty"], ["workspace-cv", "workspace-cv"]]) {
      const link = screen.getByRole("link", { name: `Open in new tab: workspace ${label}` });
      expect(link.tagName).toBe("A");
      const destination = new URL((link as HTMLAnchorElement).href);
      expect(destination.origin).toBe(sourceUrl.origin);
      expect(destination.pathname).toBe("/nested/termal/");
      expect([...destination.searchParams.entries()]).toEqual([
        ["workspace", id], ["mode", "review"],
      ]);
      expect(destination.hash).toBe("#details");
      expect(link).toHaveAttribute("target", "_blank");
      expect(link).toHaveAttribute("rel", "noopener");
      expect(link).toHaveTextContent("Open in new tab");
    }
    expect(screen.getAllByRole("link")).toHaveLength(2);
  });

  it.each([
    ["primary click", "click", {}],
    ["Ctrl-click", "click", { ctrlKey: true }],
    ["Command-click", "click", { metaKey: true }],
    ["middle click", "auxclick", { button: 1 }],
    ["context menu", "contextmenu", { button: 2 }],
  ] as const)("leaves %s to the browser without mutating the current workspace", (_, type, options) => {
    const input = props();
    render(<WorkspaceSwitcher {...input} />);
    const link = screen.getByRole("link", { name: "Open in new tab: workspace Empty" });
    const sourceUrl = window.location.href;
    // Observe after React's delegated handlers, then suppress jsdom navigation.
    let preventedByProduct: boolean | undefined;
    document.addEventListener(type, (event) => {
      preventedByProduct = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(link, createEvent(type, link, { bubbles: true, cancelable: true, ...options }, { EventType: "MouseEvent" }));

    expect(preventedByProduct).toBe(false);
    expect(window.location.href).toBe(sourceUrl);
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    expect(input.onOpenNewWorkspaceHere).not.toHaveBeenCalled();
    expect(input.onOpenNewWorkspaceWindow).not.toHaveBeenCalled();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
    expect(input.onRenameWorkspace).not.toHaveBeenCalled();
    expect(input.onToggle).not.toHaveBeenCalled();
  });

  it("does not offer a new-tab link on the current row, including its synthetic fallback", () => {
    const input = props();
    const view = render(<WorkspaceSwitcher {...input} />);
    const currentRow = () => screen.getAllByRole("listitem").find((row) => within(row).queryByText("Current"))!;
    expect(within(currentRow()).queryByRole("link")).not.toBeInTheDocument();
    view.rerender(<WorkspaceSwitcher {...input} summaries={input.summaries.slice(1)} />);
    expect(within(currentRow()).queryByRole("link")).not.toBeInTheDocument();
    expect(screen.getAllByRole("link")).toHaveLength(2);
  });

  it("keeps same-tab navigation and deletion as separate existing actions", () => {
    const input = props();
    render(<WorkspaceSwitcher {...input} />);
    fireEvent.click(screen.getByRole("button", { name: /^Empty\s*workspace-empty/ }));
    expect(input.onOpenWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-empty");
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Delete workspace workspace-empty" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-empty");
    expect(input.onOpenWorkspace).toHaveBeenCalledTimes(1);
  });
});
