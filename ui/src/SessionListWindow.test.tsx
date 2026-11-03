// Real list adapter and measured-window lifecycle, with independent browser geometry.
import { useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionListWindow } from "./SessionListWindow";
import { makeSession, makeOrchestrator } from "./app-test-harness";
import type { ControlSurfaceSessionListEntry } from "./control-surface-state";

let measure: () => void;
let width: number;
let rowHeight: number;
const rect = (top: number, height: number) => ({ top, bottom: top + height, left: 0, right: width, width, height, x: 0, y: top, toJSON() {} });
const entries = (count: number): ControlSurfaceSessionListEntry[] => Array.from({ length: count }, (_, i) => ({ kind: "session", session: makeSession(`r${i}`) }));
const open = vi.fn();
const rename = vi.fn();
const drag = vi.fn();
function Harness({ rows, selected, pending }: { rows: ControlSurfaceSessionListEntry[]; selected?: string; pending?: string }) {
  const [collapsed, setCollapsed] = useState<string[]>([]);
  return <SessionListWindow surfaceId="test" entries={rows} collapsedIds={collapsed} selectedSessionId={selected} pendingSessionId={pending}
    renderSession={session => <div className="session-row-shell" key={session.id}>
      <button draggable onClick={() => open(session.id)} onDragStart={() => drag(session.id)} onContextMenu={() => rename(session.id)}>{session.id}</button>
      <button>Actions {session.id}</button>
    </div>}
    renderGroupHeader={group => <header><button aria-expanded={!collapsed.includes(group.orchestrator.id)} aria-controls={`test-orchestrator-group-list-${group.orchestrator.id}`}
      onClick={() => setCollapsed(current => current.includes(group.orchestrator.id) ? [] : [group.orchestrator.id])}>Toggle {group.orchestrator.id}</button></header>}
  >No matches</SessionListWindow>;
}

describe("session list measured window", () => {
  beforeEach(() => {
    width = 600; rowHeight = 40;
    const extent = (node: HTMLElement): number => {
      if (node.dataset.workWindowKey) return node.dataset.workWindowKey.startsWith("group:") ? 60 : node.dataset.workWindowKey.startsWith("end:") ? 20 : rowHeight;
      if (node.style.height) return Number.parseFloat(node.style.height) || 0;
      return Array.from(node.children).reduce((sum, child) => sum + extent(child as HTMLElement), 0);
    };
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const root = this.closest<HTMLElement>(".session-list");
      if (!root || this === root) return rect(0, 120);
      let top = -root.scrollTop;
      for (let node: HTMLElement | null = this; node && node !== root; node = node.parentElement) {
        for (let previous = node.previousElementSibling as HTMLElement | null; previous; previous = previous.previousElementSibling as HTMLElement | null) top += extent(previous);
      }
      return rect(top, extent(this));
    });
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (this: HTMLElement) { return this.classList.contains("session-list") ? 120 : 0; });
    vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(function (this: HTMLElement) { return extent(this); });
    vi.stubGlobal("ResizeObserver", class {
      private nodes = new Set<Element>();
      constructor(callback: ResizeObserverCallback) { measure = () => callback([...this.nodes].map(target => ({ target })) as ResizeObserverEntry[], this as unknown as ResizeObserver); }
      observe(node: Element) { this.nodes.add(node); }
      unobserve(node: Element) { this.nodes.delete(node); }
      disconnect() { this.nodes.clear(); }
    });
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.clearAllMocks(); });

  it.each([100, 8327])("bounds mounted rows at %i sessions and reaches the final session with its actions", count => {
    const view = render(<Harness rows={entries(count)} />);
    const root = view.container.querySelector<HTMLElement>(".session-list")!;
    act(() => measure());
    expect(root.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(15);
    // Logical endpoints remain available for native entry from outside.
    expect(screen.getByRole("button", { name: `r${count - 1}` })).toBeInTheDocument();
    act(() => { root.scrollTop = root.scrollHeight - root.clientHeight; fireEvent.scroll(root); });
    act(() => measure());
    const last = screen.getByRole("button", { name: `r${count - 1}` });
    fireEvent.click(last); fireEvent.contextMenu(last); fireEvent.dragStart(last);
    expect(open).toHaveBeenCalledWith(`r${count - 1}`);
    expect(rename).toHaveBeenCalledWith(`r${count - 1}`);
    expect(drag).toHaveBeenCalledWith(`r${count - 1}`);
    expect(root.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(16);
  });

  it.each([100, 8327])("enters and exits logical endpoints from adjacent controls at %i sessions", async count => {
    const user = userEvent.setup();
    const view = render(<><input aria-label="Search sessions" /><Harness rows={entries(count)} selected="r50" /><button>Status filters</button></>);
    const root = view.container.querySelector<HTMLElement>(".session-list")!;
    act(() => measure());
    act(() => { root.scrollTop = 1800; fireEvent.scroll(root); });
    act(() => measure());
    // Neither endpoint is in the viewport, but native DOM order still reaches it.
    const first = screen.getByRole("button", { name: "r0" });
    const last = screen.getByRole("button", { name: `Actions r${count - 1}` });
    expect(first.getBoundingClientRect().bottom).toBeLessThan(0);
    expect(last.getBoundingClientRect().top).toBeGreaterThan(120);
    const search = screen.getByRole("textbox", { name: "Search sessions" });
    const filters = screen.getByRole("button", { name: "Status filters" });
    act(() => search.focus());
    await user.tab();
    expect(first).toHaveFocus();
    await user.tab({ shift: true });
    expect(search).toHaveFocus();
    act(() => filters.focus());
    await user.tab({ shift: true });
    expect(last).toHaveFocus();
    await user.tab();
    expect(filters).toHaveFocus();
    expect(root.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(18);
  });

  it("traverses mixed standalone and grouped rows, skipping outside footer gaps", () => {
    const rows: ControlSurfaceSessionListEntry[] = [entries(1)[0],
      { kind: "orchestratorGroup", orchestrator: makeOrchestrator({ status: "stopped" }), sessions: [makeSession("child")] },
      ...entries(100).slice(1)];
    const view = render(<Harness rows={rows} />);
    act(() => measure());
    const group = view.container.querySelector<HTMLElement>(".session-window-group")!;
    expect(group).toHaveAttribute("data-status", "stopped");
    expect(group.querySelector(".session-window-footer")).toBeNull();
    expect(group.nextElementSibling).toHaveClass("session-window-footer");
    const root = view.container.querySelector<HTMLElement>(".session-list")!;
    const top = root.scrollTop;
    const firstActions = screen.getByRole("button", { name: "Actions r0" });
    act(() => firstActions.focus());
    fireEvent.keyDown(firstActions, { key: "Tab" });
    const toggle = screen.getByRole("button", { name: "Toggle orchestrator-1" });
    expect(toggle).toHaveFocus();
    expect(root.scrollTop).toBe(top); // A mounted target is not revealed at the top.
    fireEvent.keyDown(toggle, { key: "Tab" });
    expect(screen.getByRole("button", { name: "child" })).toHaveFocus();
    const childActions = screen.getByRole("button", { name: "Actions child" });
    act(() => childActions.focus());
    fireEvent.keyDown(childActions, { key: "Tab" });
    expect(screen.getByRole("button", { name: "r1" })).toHaveFocus();
    fireEvent.keyDown(screen.getByRole("button", { name: "r1" }), { key: "Tab", shiftKey: true });
    expect(childActions).toHaveFocus();
    fireEvent.click(toggle);
    expect(toggle.closest(".session-window-header")).toHaveClass("collapsed");
    expect(screen.queryByRole("button", { name: "child" })).not.toBeInTheDocument();
  });

  it.each(["contextmenu", "dragstart"])("retains the %s trigger after scrolling away", kind => {
    const view = render(<Harness rows={entries(200)} />);
    act(() => measure());
    const trigger = screen.getByRole("button", { name: "r3" });
    if (kind === "contextmenu") fireEvent.contextMenu(trigger);
    else fireEvent.dragStart(trigger);
    const root = view.container.querySelector<HTMLElement>(".session-list")!;
    act(() => { root.scrollTop = 10000; fireEvent.scroll(root); });
    act(() => measure());
    expect(screen.getByRole("button", { name: "r3" })).toBe(trigger);
    expect(root.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(18);
  });

  it("does not revive a pending Tab focus after its target is filtered out", () => {
    const all = entries(100);
    const content = (rows: ControlSurfaceSessionListEntry[]) => <><input aria-label="Search sessions" /><Harness rows={rows} /></>;
    const view = render(content(all));
    act(() => measure());
    const root = view.container.querySelector<HTMLElement>(".session-list")!;
    const mounted = Array.from(root.querySelectorAll<HTMLElement>("[data-work-window-key]"))
      .map(node => Number(node.dataset.workWindowKey!.replace("session:r", "")))
      .filter(index => index < 99);
    const edge = Math.max(...mounted);
    const actions = screen.getByRole("button", { name: `Actions r${edge}` });
    act(() => {
      actions.focus();
      fireEvent.keyDown(actions, { key: "Tab" });
      view.rerender(content(all.filter((_, index) => index !== edge + 1)));
    });
    const search = screen.getByRole("textbox", { name: "Search sessions" });
    act(() => search.focus());
    view.rerender(content(all));
    act(() => measure());
    expect(search).toHaveFocus();
  });

  it("keeps Tab and Shift+Tab in logical row order across unmounted rows", () => {
    render(<Harness rows={entries(100)} />);
    act(() => measure());
    for (let i = 0; i < 25; i++) {
      const actions = screen.getByRole("button", { name: `Actions r${i}` });
      act(() => actions.focus());
      fireEvent.keyDown(actions, { key: "Tab" });
      expect(screen.getByRole("button", { name: `r${i + 1}` })).toHaveFocus();
    }
    const current = screen.getByRole("button", { name: "r25" });
    fireEvent.keyDown(current, { key: "Tab", shiftKey: true });
    expect(screen.getByRole("button", { name: "Actions r24" })).toHaveFocus();
  });

  it("windows the children of one huge group and preserves collapse and filter membership", () => {
    const grouped: ControlSurfaceSessionListEntry[] = [{ kind: "orchestratorGroup", orchestrator: makeOrchestrator(), sessions: entries(3000).map(entry => (entry as Extract<ControlSurfaceSessionListEntry, { kind: "session" }>).session) }];
    const view = render(<Harness rows={grouped} />);
    act(() => measure());
    expect(view.container.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(15);
    const toggle = screen.getByRole("button", { name: "Toggle orchestrator-1" });
    expect(document.getElementById(toggle.getAttribute("aria-controls")!)).not.toBeNull();
    fireEvent.click(toggle);
    expect(view.container.querySelectorAll(".session-row-shell")).toHaveLength(0);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(toggle);
    expect(view.container.querySelectorAll(".session-row-shell").length).toBeGreaterThan(0);
    view.rerender(<Harness rows={[{ ...grouped[0] as Extract<ControlSurfaceSessionListEntry, { kind: "orchestratorGroup" }>, sessions: [makeSession("r2999")] }]} />);
    expect(screen.getByRole("button", { name: "r2999" })).toBeInTheDocument();
    expect(view.container.querySelectorAll(".session-row-shell")).toHaveLength(1);
  });

  it("retains focused and pending-action triggers while scrolling and remeasuring", () => {
    const view = render(<Harness rows={entries(200)} selected="r2" pending="r4" />);
    act(() => measure());
    const focused = screen.getByRole("button", { name: "r3" });
    const pending = screen.getByRole("button", { name: "r4" });
    act(() => focused.focus());
    const root = view.container.querySelector<HTMLElement>(".session-list")!;
    act(() => { root.scrollTop = 10000; fireEvent.scroll(root); });
    act(() => { width = 300; rowHeight = 80; measure(); });
    expect(screen.getByRole("button", { name: "r3" })).toBe(focused);
    expect(focused).toHaveFocus();
    expect(screen.getByRole("button", { name: "r4" })).toBe(pending);
    expect(root.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(16);
  });
});
