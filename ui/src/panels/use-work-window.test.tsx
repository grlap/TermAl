// Measured viewport lifecycle witnesses. Geometry follows mounted rows and
// real spacer heights, independently of the hook's estimated offsets.
import { Fragment, useMemo, useRef } from "react";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkWindow, WORK_WINDOW_ROWS } from "./use-work-window";
import { WorkTree } from "./WorkTree";
import type { WorkItem } from "../work-visualizer-api";

let measure: () => void;
let width: number;
let rowHeight: number;
const rect = (top: number, height: number) => ({ top, bottom: top + height, left: 0, right: width, width, height, x: 0, y: top, toJSON() {} });
function Harness({ count, selected }: { count: number; selected?: string }) {
  const viewport = useRef<HTMLDivElement>(null);
  const keys = useMemo(() => Array.from({ length: count }, (_, i) => `r${i}`), [count]);
  const window = useWorkWindow(keys, viewport, selected);
  return <><button onClick={() => window.reveal("r90")}>Reveal last</button><div ref={viewport} data-testid="viewport">
    <div ref={window.content}>{window.indices.map((index, position) => <Fragment key={keys[index]}>
      <div aria-hidden="true" style={{ height: window.before(position) }} />
      <div ref={window.rowRef(keys[index])}><button>{keys[index]}</button></div>
    </Fragment>)}<div aria-hidden="true" style={{ height: window.after }} /></div>
  </div></>;
}
describe("measured Work window", () => {
  beforeEach(() => {
    width = 600; rowHeight = 40;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const root = this.closest<HTMLElement>('[data-testid="viewport"]');
      if (!root || this === root) return rect(0, 120);
      let top = -root.scrollTop;
      for (let node: HTMLElement | null = this; node && node !== root; node = node.parentElement) {
        for (let previous = node.previousElementSibling as HTMLElement | null; previous; previous = previous.previousElementSibling as HTMLElement | null)
          top += previous.dataset.workWindowKey ? rowHeight : Number.parseFloat(previous.style.height) || 0;
      }
      return rect(top, this.dataset.workWindowKey ? rowHeight : 0);
    });
    vi.stubGlobal("ResizeObserver", class {
      private nodes = new Set<Element>();
      constructor(callback: ResizeObserverCallback) { measure = () => callback([...this.nodes].map(target => ({ target })) as ResizeObserverEntry[], this as unknown as ResizeObserver); }
      observe(node: Element) { this.nodes.add(node); }
      unobserve(node: Element) { this.nodes.delete(node); }
      disconnect() { this.nodes.clear(); }
    });
  });
  afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

  it("keeps the actual visible row and DOM through the threshold, measurements, append and width changes", () => {
    const view = render(<Harness count={55} />);
    const root = screen.getByTestId("viewport");
    root.scrollTop = 400; fireEvent.scroll(root);
    const anchor = screen.getByRole("button", { name: "r10" });
    expect(anchor.parentElement!.getBoundingClientRect().top).toBe(0);
    view.rerender(<Harness count={80} />);
    expect(screen.getByRole("button", { name: "r10" })).toBe(anchor);
    expect(root.scrollTop).toBe(400);
    act(() => measure());
    expect(anchor.parentElement!.getBoundingClientRect().top).toBe(0);
    view.rerender(<Harness count={100} />);
    expect(screen.getByRole("button", { name: "r10" })).toBe(anchor);
    expect(anchor.parentElement!.getBoundingClientRect().top).toBe(0);
    act(() => { width = 300; rowHeight = 80; measure(); });
    expect(screen.getByRole("button", { name: "r10" })).toBe(anchor);
    expect(anchor.parentElement!.getBoundingClientRect().top).toBe(0);
    expect(view.container.querySelectorAll("[data-work-window-key]").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
  });

  it("reveals an unmounted row and retains a selected and keyboard-focused row while scrolling", () => {
    const view = render(<Harness count={100} selected="r2" />);
    act(() => measure());
    const focused = screen.getByRole("button", { name: "r3" });
    act(() => focused.focus());
    expect(screen.queryByRole("button", { name: "r90" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Reveal last" }));
    expect(screen.getByRole("button", { name: "r90" })).toBeInTheDocument();
    act(() => measure());
    expect(screen.getByRole("button", { name: "r90" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "r2" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "r3" })).toBe(focused);
    expect(document.activeElement).toBe(focused);
    expect(view.container.querySelectorAll("[data-work-window-key]").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
  });

  it("reveals and focuses a real shared prerequisite from an offscreen branch", () => {
    const item = (id: string, prerequisites: string[]): WorkItem => ({ id, shortRef: id, title: id,
      source: "engram", kind: "task", lifecycle: "open", availability: "ready", priority: 2, labels: [],
      assignedTo: null, parentId: null, updatedAt: "today", blockedBy: prerequisites,
      prerequisites: prerequisites.map(id => ({ id, satisfied: false })) });
    const rows = [...Array.from({ length: 100 }, (_, i) => item(`goal${i}`, ["common"])), item("common", [])];
    function Tree() {
      const viewport = useRef<HTMLDivElement>(null);
      return <div ref={viewport} data-testid="viewport"><WorkTree rows={rows} universe={rows} mode="dependencies"
        viewport={viewport} selection={null} onSelect={() => {}} /></div>;
    }
    render(<Tree />);
    const root = screen.getByTestId("viewport");
    root.scrollTop = 9000; fireEvent.scroll(root);
    expect(screen.queryByRole("button", { name: "common — common" })).not.toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: /\+1 shown above/ })[0]);
    expect(screen.getByRole("button", { name: "common — common" })).toHaveFocus();
    expect(root.scrollTop).toBeLessThan(9000);
  });
});
