import { act, createEvent, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  WorkspacesPanelHarness,
  type WorkspacesPanelHarnessProps,
} from "./panels/workspaces-panel-test-harness";

function props(): WorkspacesPanelHarnessProps {
  return {
    currentWorkspaceId: "workspace-current",
    summaries: [
      { id: "workspace-current", label: "Termal", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-empty", label: "Empty", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-cv", revision: 1, updatedAt: "today", controlPanelSide: "left" },
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

const initialUrl = window.location.href;

async function flushNativeNewTabDismiss() {
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });
}

afterEach(async () => {
  window.history.replaceState(null, "", initialUrl);
  await flushNativeNewTabDismiss();
});

describe("saved workspace new-tab links", () => {
  it("links to each other saved ID while retaining the URL context", () => {
    window.history.replaceState(null, "", "/nested/termal/?workspace=workspace-current&mode=review#details");
    const sourceUrl = new URL(window.location.href);
    render(<WorkspacesPanelHarness {...props()} />);

    for (const [label, id] of [["Empty", "workspace-empty"], ["workspace-cv", "workspace-cv"]] as const) {
      fireEvent.click(screen.getByRole("button", { name: `Actions for workspace ${label}` }));
      const link = screen.getByRole("menuitem", { name: `Open in new tab: workspace ${label}` });
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
      fireEvent.click(screen.getByRole("button", { name: `Actions for workspace ${label}` }));
    }
  });

  it.each([
    ["primary click", "click", {}],
    ["Ctrl-click", "click", { ctrlKey: true }],
    ["Command-click", "click", { metaKey: true }],
    ["middle click", "auxclick", { button: 1 }],
    ["context menu", "contextmenu", { button: 2 }],
  ] as const)("leaves %s to the browser without mutating the current workspace", (_, type, options) => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    const sourceUrl = window.location.href;
    let preventedByProduct: boolean | undefined;
    document.addEventListener(type, (event) => {
      preventedByProduct = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(link, createEvent(type, link, { bubbles: true, cancelable: true, ...options }, { EventType: "MouseEvent" }));

    expect(preventedByProduct).toBe(false);
    expect(window.location.href).toBe(sourceUrl);
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
    expect(input.onRenameWorkspace).not.toHaveBeenCalled();
  });

  it.each([
    ["primary click", "click", { button: 0 }],
    ["Ctrl-click", "click", { button: 0, ctrlKey: true }],
    ["Command-click", "click", { button: 0, metaKey: true }],
    ["middle click", "auxclick", { button: 1 }],
  ] as const)("dismisses the menu after an unprevented %s without replacing the anchor", async (_, type, options) => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    const trigger = screen.getByRole("button", { name: "Actions for workspace Empty" });
    fireEvent.click(trigger);
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    expect(link.tagName).toBe("A");
    const href = (link as HTMLAnchorElement).href;
    expect(href).toContain("workspace=workspace-empty");
    const sourceUrl = window.location.href;
    let preventedByProduct: boolean | undefined;
    document.addEventListener(type, (event) => {
      preventedByProduct = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(
      link,
      createEvent(type, link, { bubbles: true, cancelable: true, ...options }, { EventType: "MouseEvent" }),
    );
    expect(preventedByProduct).toBe(false);
    expect(link).toBeInTheDocument();
    await flushNativeNewTabDismiss();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(window.location.href).toBe(sourceUrl);
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
    expect(input.onRenameWorkspace).not.toHaveBeenCalled();
  });

  it("dismisses from the native click that follows Enter, not from keydown alone", async () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    const trigger = screen.getByRole("button", { name: "Actions for workspace Empty" });
    fireEvent.click(trigger);
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    let keyPrevented: boolean | undefined;
    let clickPrevented: boolean | undefined;
    document.addEventListener("keydown", (event) => {
      keyPrevented = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(link, createEvent("keydown", link, { bubbles: true, cancelable: true, key: "Enter" }, { EventType: "KeyboardEvent" }));
    expect(keyPrevented).toBe(false);
    expect(screen.getByRole("menu")).toBeInTheDocument();
    document.addEventListener("click", (event) => {
      clickPrevented = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(link, createEvent("click", link, { bubbles: true, cancelable: true, button: 0 }, { EventType: "MouseEvent" }));
    expect(clickPrevented).toBe(false);
    await flushNativeNewTabDismiss();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
  });

  it("keeps the new-tab anchor after contextmenu and right-button auxclick", async () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    let contextPrevented: boolean | undefined;
    let auxPrevented: boolean | undefined;
    document.addEventListener("contextmenu", (event) => {
      contextPrevented = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    document.addEventListener("auxclick", (event) => {
      auxPrevented = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(link, createEvent("contextmenu", link, { bubbles: true, cancelable: true, button: 2 }, { EventType: "MouseEvent" }));
    fireEvent(link, createEvent("auxclick", link, { bubbles: true, cancelable: true, button: 2 }, { EventType: "MouseEvent" }));
    expect(contextPrevented).toBe(false);
    expect(auxPrevented).toBe(false);
    await flushNativeNewTabDismiss();
    await flushNativeNewTabDismiss();
    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" })).toBe(link);
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
  });

  it("does not steal focus that moved outside before the deferred dismiss", async () => {
    const input = props();
    render(
      <>
        <textarea aria-label="Composer" />
        <WorkspacesPanelHarness {...input} />
      </>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    document.addEventListener("click", (event) => event.preventDefault(), { once: true });
    fireEvent(link, createEvent("click", link, { bubbles: true, cancelable: true, button: 0 }, { EventType: "MouseEvent" }));
    const composer = screen.getByRole("textbox", { name: "Composer" });
    await act(async () => {
      composer.focus();
    });
    expect(composer).toHaveFocus();
    await flushNativeNewTabDismiss();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(composer).toHaveFocus();
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
  });

  it("does not close a newer row menu from a stale new-tab dismiss", async () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    const emptyLink = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    document.addEventListener("click", (event) => event.preventDefault(), { once: true });
    fireEvent(emptyLink, createEvent("click", emptyLink, { bubbles: true, cancelable: true, button: 0 }, { EventType: "MouseEvent" }));
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace workspace-cv" }));
    expect(screen.getByRole("menu")).toHaveAccessibleName("Workspace actions workspace-cv");
    await flushNativeNewTabDismiss();
    expect(screen.getByRole("menu")).toHaveAccessibleName("Workspace actions workspace-cv");
    expect(screen.getByRole("menuitem", { name: "Open in new tab: workspace workspace-cv" })).toBeInTheDocument();
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
  });

  it("does not offer a new-tab link on the current row, including its synthetic fallback", () => {
    const input = props();
    const view = render(<WorkspacesPanelHarness {...input} />);
    const currentRow = () => screen.getAllByRole("listitem").find((row) => within(row).queryByText("Current"))!;
    fireEvent.click(within(currentRow()).getByRole("button", { name: "Actions for workspace Termal" }));
    expect(screen.queryByRole("menuitem", { name: "Open in new tab: workspace Termal" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /Open in new tab/ })).not.toBeInTheDocument();
    view.rerender(<WorkspacesPanelHarness {...input} summaries={input.summaries.slice(1)} />);
    expect(screen.queryByRole("menuitem", { name: /Open in new tab/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /Open in new tab/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Actions for workspace Termal" })).not.toBeInTheDocument();
  });

  it("keeps same-tab navigation and deletion as separate existing actions", () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Empty" }));
    expect(input.onOpenWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-empty");
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete workspace Empty" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-empty");
    expect(input.onOpenWorkspace).toHaveBeenCalledTimes(1);
  });

  it("activates an enabled new-tab menuitem from Space once and dismisses after the generated click", async () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    const trigger = screen.getByRole("button", { name: "Actions for workspace Empty" });
    fireEvent.click(trigger);
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    let keyPrevented: boolean | undefined;
    let clickCount = 0;
    let clickPrevented: boolean | undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === " ") {
        keyPrevented = event.defaultPrevented;
      }
    };
    const onClick = (event: MouseEvent) => {
      clickCount += 1;
      clickPrevented = event.defaultPrevented;
      event.preventDefault();
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("click", onClick);
    try {
      fireEvent.keyDown(link, { key: " ", bubbles: true, cancelable: true });
      expect(keyPrevented).toBe(true);
      expect(clickCount).toBe(1);
      expect(clickPrevented).toBe(false);
      expect(link).toHaveAttribute("target", "_blank");
      expect(link).toHaveAttribute("rel", "noopener");
      await flushNativeNewTabDismiss();
      expect(screen.queryByRole("menu")).not.toBeInTheDocument();
      expect(trigger).toHaveFocus();
      expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    } finally {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("click", onClick);
    }
  });

  it("does not activate new-tab from Space when composing, repeating, or already consumed", async () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Empty" });
    let clickCount = 0;
    const onClick = () => {
      clickCount += 1;
    };
    document.addEventListener("click", onClick);
    try {
      fireEvent.keyDown(link, { key: " ", bubbles: true, cancelable: true, isComposing: true });
      fireEvent.keyDown(link, { key: " ", bubbles: true, cancelable: true, repeat: true });
      link.addEventListener("keydown", (event) => event.preventDefault(), { once: true, capture: true });
      fireEvent.keyDown(link, { key: " ", bubbles: true, cancelable: true });
      expect(clickCount).toBe(0);
      expect(screen.getByRole("menu")).toBeInTheDocument();
      expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    } finally {
      document.removeEventListener("click", onClick);
    }
  });

  it("does not hijack Space on overflow menu buttons", async () => {
    const input = props();
    render(<WorkspacesPanelHarness {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Empty" }));
    const rename = screen.getByRole("menuitem", { name: "Rename" });
    let keyPrevented: boolean | undefined;
    let clickCount = 0;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === " ") {
        keyPrevented = event.defaultPrevented;
      }
    };
    const onClick = () => {
      clickCount += 1;
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("click", onClick);
    try {
      fireEvent.keyDown(rename, { key: " ", bubbles: true, cancelable: true });
      expect(keyPrevented).toBe(false);
      expect(clickCount).toBe(0);
      expect(screen.getByRole("menu")).toBeInTheDocument();
      expect(input.onRenameWorkspace).not.toHaveBeenCalled();
    } finally {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("click", onClick);
    }
  });
});
