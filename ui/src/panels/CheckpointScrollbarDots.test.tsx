// Owns checkpoint dot selection, grouping, and keyboard chooser behavior.
// Does not substitute for pane history loading or native scrollbar tests.
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationMarker } from "../types";
import { CheckpointScrollbarDots } from "./CheckpointScrollbarDots";

const scrollNodes: HTMLElement[] = [];
afterEach(() => { cleanup(); scrollNodes.splice(0).forEach(node => node.remove()); });

function marker(id: string, position: number, kind: ConversationMarker["kind"] = "checkpoint"): ConversationMarker {
  return { id, name: id, kind, sessionId: "session", messageId: `message-${position}`, messageIndexHint: position, color: "#888888", createdAt: "2026-01-01", updatedAt: "2026-01-01", createdBy: "user" };
}

function setup(markers: ConversationMarker[]) {
  const node = document.createElement("div");
  Object.defineProperties(node, { clientHeight: { value: 600 }, clientWidth: { value: 400 } });
  document.body.append(node);
  scrollNodes.push(node);
  const onJump = vi.fn();
  const view = render(<CheckpointScrollbarDots markers={markers} messageCount={1_000} scrollContainerRef={{ current: node }} isActive onJump={onJump} />);
  return { ...view, onJump, node };
}

async function readStylesheet(relativeFromTest: string) {
  const nodeFsModule = "node:fs";
  const nodeUrlModule = "node:url";
  const { readFileSync } = await import(nodeFsModule) as {
    readFileSync: (path: string, encoding: "utf8") => string;
  };
  const { fileURLToPath } = await import(nodeUrlModule) as {
    fileURLToPath: (url: string) => string;
  };
  const moduleUrl = import.meta.url;
  return readFileSync(fileURLToPath(new URL(relativeFromTest, moduleUrl).href), "utf8");
}

describe("CheckpointScrollbarDots", () => {
  it("offers every checkpoint independently of resident messages and excludes other marker kinds", () => {
    const first = marker("First", 0);
    const unloaded = marker("Unloaded", 700);
    const last = marker("Last", 999);
    const { onJump } = setup([first, unloaded, last, marker("Decision", 100, "decision")]);
    expect(screen.getAllByRole("button")).toHaveLength(3);
    for (const checkpoint of [first, unloaded, last]) {
      fireEvent.click(screen.getByRole("button", { name: `Go to checkpoint ${checkpoint.name}` }));
      expect(onJump).toHaveBeenLastCalledWith(checkpoint);
    }
    expect(screen.queryByRole("button", { name: /Decision/ })).toBeNull();
  });

  it("groups nearby checkpoints and lets the chooser navigate to each one", () => {
    const checkpoints = [marker("Alpha", 400), marker("Beta", 401), marker("Gamma", 402)];
    const { onJump } = setup(checkpoints);
    const group = screen.getByRole("button", { name: "3 checkpoints" });
    for (const checkpoint of checkpoints) {
      fireEvent.click(group);
      const menu = screen.getByRole("menu", { name: "Choose checkpoint" });
      expect(within(menu).getAllByRole("menuitem")).toHaveLength(3);
      fireEvent.click(within(menu).getByRole("menuitem", { name: checkpoint.name }));
      expect(onJump).toHaveBeenLastCalledWith(checkpoint);
      expect(screen.queryByRole("menu")).toBeNull();
      expect(group).toHaveFocus();
    }
  });

  it("supports keyboard movement and returns focus on Escape without navigating", () => {
    const { onJump } = setup([marker("Alpha", 400), marker("Beta", 401)]);
    const group = screen.getByRole("button", { name: "2 checkpoints" });
    fireEvent.click(group);
    const menu = screen.getByRole("menu");
    expect(screen.getByRole("menuitem", { name: "Alpha" })).toHaveFocus();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Beta" })).toHaveFocus();
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(group).toHaveFocus();
    expect(screen.queryByRole("menu")).toBeNull();
    expect(onJump).not.toHaveBeenCalled();
  });

  it("adds no overlay or controls for a checkpoint-free conversation", () => {
    const { container, node } = setup([marker("Review", 800, "review")]);
    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByRole("button")).toBeNull();
    expect(node).toBeInTheDocument();
  });

  it.each(["dark", "warm-light"])("keeps dot and chooser borders visible in the %s theme", async (theme) => {
    const previousTheme = document.documentElement.getAttribute("data-theme");
    const style = document.createElement("style");
    style.textContent = await readStylesheet("../styles.css") + await readStylesheet(`../themes/${theme}.css`) + await readStylesheet("./checkpoint-scrollbar-dots.css");
    document.head.append(style);
    document.documentElement.setAttribute("data-theme", theme);
    try {
      setup([marker("Alpha", 400), marker("Beta", 401)]);
      const dot = screen.getByRole("button", { name: "2 checkpoints" });
      fireEvent.click(dot);
      for (const node of [dot, screen.getByRole("menu")]) {
        const computed = getComputedStyle(node);
        expect(computed.borderTopStyle).toBe("solid");
        expect(Number.parseFloat(computed.borderTopWidth)).toBeGreaterThan(0);
        expect(computed.borderTopColor).not.toBe("");
        expect(computed.borderTopColor).not.toBe("transparent");
        expect(computed.borderTopColor).not.toBe("rgba(0, 0, 0, 0)");
      }
    } finally {
      style.remove();
      if (previousTheme === null) document.documentElement.removeAttribute("data-theme");
      else document.documentElement.setAttribute("data-theme", previousTheme);
    }
  });

  it.each(["global menu", "modal"])("keeps checkpoint controls below a covering %s and dismisses the chooser when it takes focus", async (kind) => {
    const style = document.createElement("style");
    // Vitest disables CSS imports by default, including ?raw. Use the actual
    // cascade through the same file-URL fixture path as other production-CSS tests.
    style.textContent = await readStylesheet("../styles.css") + await readStylesheet("./checkpoint-scrollbar-dots.css");
    document.head.append(style);
    const covering = document.createElement("div");
    covering.className = kind === "modal" ? "dialog-backdrop" : "pane-tab-context-menu";
    const action = document.createElement("button");
    action.textContent = "Covering action";
    covering.append(action);
    document.body.append(covering);
    try {
      const { onJump } = setup([marker("Alpha", 400), marker("Beta", 401)]);
      const group = screen.getByRole("button", { name: "2 checkpoints" });
      fireEvent.click(group);
      expect(screen.getByRole("menu")).toBeInTheDocument();
      const dots = group.closest<HTMLElement>(".checkpoint-scrollbar-dots")!;
      const layer = (node: HTMLElement): number => {
        const value = getComputedStyle(node).zIndex;
        const variable = /^var\((--[^)]+)\)$/.exec(value);
        return Number(variable ? getComputedStyle(document.documentElement).getPropertyValue(variable[1]) : value);
      };
      // jsdom checks the production stacking contract; browser hit-testing is
      // separate. Moving focus into the covering control must dismiss the menu.
      expect(layer(dots)).toBeLessThan(layer(covering));
      fireEvent.focusIn(action);
      expect(screen.queryByRole("menu")).toBeNull();
      fireEvent.click(action);
      expect(onJump).not.toHaveBeenCalled();
      fireEvent.click(group);
      fireEvent.pointerDown(action);
      expect(screen.queryByRole("menu")).toBeNull();
      expect(onJump).not.toHaveBeenCalled();
    } finally {
      style.remove();
      covering.remove();
    }
  });
});
