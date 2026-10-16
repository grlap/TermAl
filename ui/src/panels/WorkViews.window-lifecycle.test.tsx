// Exercise the real row containers with layout-derived spacers and a bounded
// scroll range. jsdom's default scrollTop never clamps when content changes.
import { Profiler, StrictMode, useCallback, useRef, useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkItem } from "../work-visualizer-api";
import { WorkLabels } from "./WorkLabels";
import { WorkTable } from "./WorkTable";
import { WorkTree } from "./WorkTree";
import { WORK_WINDOW_ROWS } from "./use-work-window";

type View = "table" | "tree" | "labels";
const rows: WorkItem[] = Array.from({ length: 125 }, (_, index) => ({
  id: `item-${index}`, shortRef: `item-${index}`, title: `Work item ${index}`,
  source: "engram", kind: "task", lifecycle: "open", availability: "ready",
  priority: 2, labels: ["shared"], assignedTo: null, parentId: null,
  updatedAt: "2026-10-15T12:00:00Z", blockedBy: [], prerequisites: [],
}));

let width: number;
let viewportHeight: number;
let rowHeight: number;
let commits: number;
let measure: () => void;
let dynamicLayout: boolean;
let inheritedTableLayout: boolean;
const SCROLLBAR = 17;
const horizontalBars = new Set<boolean>();
const quantum = (value: number) => Math.round(value * 64) / 64;
// Native Chrome capture, reduced to synthetic row indices. Including the
// prerequisite-heavy boundary row changes title/Waits-for widths from
// 693.65625/232.96875 to 437.265625/503.96875, even without a scrollbar.
// Replay the resulting row heights; geometry follows the mounted cells.
const crowdedHeights = new Map([
  [64, 79.84375], [65, 98.53125], [66, 82.53125], [67, 98.53125],
  [68, 98.53125], [69, 80.34375], [70, 98.53125], [71, 80.34375],
  [72, 121.5], [73, 121.5], [74, 98.53125], [75, 80.34375],
  [76, 98.53125], [77, 98.53125],
]);

// A small, explicit auto-layout model: the fixed columns and the longest
// unbroken assignment determine the table's minimum width. The title column
// takes the remaining width; narrower titles wrap to additional lines. These
// values depend on the mounted rows, never on a render counter or a forced
// alternating rectangle. This tests a layout hypothesis, not browser pixels.
function tableWidths(table: HTMLElement, available: number) {
  const assignment = Math.max(100, ...[...table.querySelectorAll("tbody[data-work-window-key] td:nth-child(8)")]
    .map(cell => (cell.textContent ?? "").split(/\s+/).reduce((longest, word) => Math.max(longest, word.length * 7), 0) + 14));
  const otherColumns = 582;
  return { minimum: otherColumns + assignment + 224, title: Math.max(224, available - otherColumns - assignment) };
}

function flowHeight(node: HTMLElement, available = width): number {
  if (node.dataset.workWindowKey) {
    if (inheritedTableLayout) {
      const table = node.closest<HTMLElement>("table");
      if (!table) return node.dataset.workWindowKey.startsWith("@") ? 15 : 66.75;
      const crowded = [...table.querySelectorAll("tbody[data-work-window-key] td:nth-child(7)")]
        .some(cell => (cell.textContent?.match(/\(satisfied\)/g)?.length ?? 0) >= 4);
      const index = Number(node.querySelector<HTMLButtonElement>(".work-row-title")?.dataset.workRef?.replace("item-", ""));
      return crowded ? crowdedHeights.get(index) ?? 98.53125 : index === 65 ? 63.84375 : 64.34375;
    }
    if (!dynamicLayout) return rowHeight;
    const table = node.closest<HTMLElement>("table");
    const titleWidth = table ? tableWidths(table, available).title : available - 48;
    const title = node.querySelector(".work-row-title")?.textContent ?? "";
    return quantum(rowHeight + Math.max(0, Math.ceil(title.length * 7 / titleWidth) - 1) * 18);
  }
  const explicit = Number.parseFloat(node.style.height);
  if (Number.isFinite(explicit)) return explicit;
  if (node.tagName === "CAPTION") return inheritedTableLayout ? 17 : 17.3;
  if (node.tagName === "THEAD") return inheritedTableLayout ? 38.46875 : 32.6;
  if (node.tagName === "P") return 24;
  if (node.classList.contains("work-panel-actions")) return 32;
  const children = [...node.children].reduce((sum, child) => sum + flowHeight(child as HTMLElement, available), 0);
  if (dynamicLayout && node.classList.contains("work-table-scroll")) {
    const table = node.querySelector<HTMLElement>("table")!;
    const horizontal = tableWidths(table, available).minimum > available;
    horizontalBars.add(horizontal);
    return children + (horizontal ? SCROLLBAR : 0);
  }
  return children;
}

function contentHeight(root: HTMLElement): number {
  return [...root.children].reduce((sum, child) => sum + flowHeight(child as HTMLElement, contentWidth(root)), 0);
}

function contentWidth(root: HTMLElement): number {
  if (inheritedTableLayout) return width - 15;
  if (!dynamicLayout) return width;
  // A root scrollbar consumes client width, not its border-box width.
  const unconstrained = [...root.children].reduce((sum, child) => sum + flowHeight(child as HTMLElement, width), 0);
  return width - (unconstrained > viewportHeight ? SCROLLBAR : 0);
}

function installViewport(root: HTMLDivElement) {
  let scroll = 0;
  const clamp = (value: number) => Math.max(0, Math.min(value, Math.ceil(contentHeight(root)) - viewportHeight));
  Object.defineProperties(root, {
    clientHeight: { configurable: true, get: () => viewportHeight },
    clientWidth: { configurable: true, get: () => contentWidth(root) },
    scrollHeight: { configurable: true, get: () => Math.ceil(contentHeight(root)) },
    scrollTop: {
      configurable: true,
      get: () => (scroll = clamp(scroll)),
      set: (value: number) => { scroll = clamp(dynamicLayout ? quantum(value) : value); },
    },
  });
}

function Harness({ view, items = rows }: { view: View; items?: readonly WorkItem[] }) {
  const viewport = useRef<HTMLDivElement | null>(null);
  const attach = useCallback((node: HTMLDivElement | null) => {
    viewport.current = node;
    if (node) installViewport(node);
  }, []);
  return <div ref={attach} data-testid="viewport" className="work-panel-list">
    {view === "table" ? <WorkTable rows={items} selection={null} onSelect={() => {}}
      sort={null} onSortChange={() => {}} viewport={viewport} />
      : view === "tree" ? <WorkTree rows={items} universe={items} mode="hierarchy"
        selection={null} onSelect={() => {}} viewport={viewport} />
        : <WorkLabels rows={items} mixedSources={false} selection={null}
          onSelect={() => {}} onLabel={() => {}} viewport={viewport} />}
  </div>;
}

describe("real Work views at the clamped end of a measured window", () => {
  beforeEach(() => {
    width = 700.5;
    viewportHeight = 267;
    rowHeight = 43.2;
    commits = 0;
    dynamicLayout = false;
    inheritedTableLayout = false;
    horizontalBars.clear();
    measure = () => { throw new Error("ResizeObserver was not attached"); };
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const root = this.closest<HTMLElement>('[data-testid="viewport"]');
      const available = root ? contentWidth(root) : width;
      let top = inheritedTableLayout ? 381.234375 : 11.3;
      if (root && this !== root) {
        top -= root.scrollTop;
        for (let node: HTMLElement | null = this; node && node !== root; node = node.parentElement) {
          for (let sibling = node.previousElementSibling; sibling; sibling = sibling.previousElementSibling)
            top += flowHeight(sibling as HTMLElement, available);
        }
      }
      if (dynamicLayout) top = quantum(top);
      const height = this === root ? viewportHeight : flowHeight(this, available);
      const measuredWidth = this === root ? width : available;
      return { top, bottom: top + height, left: 0, right: measuredWidth, width: measuredWidth, height, x: 0, y: top, toJSON() {} };
    });
    vi.stubGlobal("ResizeObserver", class {
      private nodes = new Set<Element>();
      constructor(callback: ResizeObserverCallback) {
        measure = () => callback([...this.nodes].map(target => ({ target })) as ResizeObserverEntry[], this as unknown as ResizeObserver);
      }
      observe(node: Element) { this.nodes.add(node); }
      unobserve(node: Element) { this.nodes.delete(node); }
      disconnect() { this.nodes.clear(); }
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it.each<View>(["table", "tree", "labels"])("settles %s measurements, end clamping and resize without a render loop", view => {
    const bounded = (action: () => void) => {
      const before = commits;
      expect(action).not.toThrow();
      // A single external scroll/measurement must settle, rather than run
      // layout -> setView -> layout until React's depth guard fires.
      expect(commits - before).toBeLessThan(20);
    };
    bounded(() => {
      render(<Profiler id="work-window" onRender={() => { commits++; }}><Harness view={view} /></Profiler>);
    });
    const root = screen.getByTestId("viewport");
    if (view === "table") {
      expect(root.querySelector(".work-table-scroll > table > thead")).not.toBeNull();
      expect(root.querySelectorAll("tbody[data-work-window-key]").length).toBeGreaterThan(0);
    }
    expect(rows.length).toBeGreaterThan(WORK_WINDOW_ROWS);
    bounded(() => act(() => measure()));
    const end = () => {
      root.scrollTop = Number.MAX_SAFE_INTEGER;
      expect(root.scrollTop).toBe(root.scrollHeight - root.clientHeight);
      fireEvent.scroll(root);
    };
    bounded(end);
    bounded(() => act(() => measure()));
    bounded(() => act(() => {
      width = 320.5;
      rowHeight = 74.4;
      viewportHeight = 201;
      measure();
    }));
    bounded(end);
    bounded(() => act(() => measure()));
    expect(root.querySelectorAll("[data-work-window-key]").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
    const settled = commits;
    bounded(() => fireEvent.scroll(root));
    expect(commits - settled).toBeLessThan(3);
  });

  it("settles a wide 119-row auto-layout table as mounted rows change its scrollbar and wrapping", () => {
    dynamicLayout = true;
    width = 1137;
    viewportHeight = 487;
    const items = rows.slice(0, 119).map((item, index) => ({
      ...item,
      title: index % 5 === 0 ? `${item.title} ${"longer readable work description ".repeat(4)}` : item.title,
      assignedTo: index === 80 ? "LongUnbrokenRuntimeEvidenceOwnerForTheDesktopReview" : "reviewer",
    }));
    const bounded = (action: () => void) => {
      const before = commits;
      expect(action).not.toThrow();
      expect(commits - before).toBeLessThan(20);
    };
    bounded(() => {
      render(<Profiler id="auto-table" onRender={() => { commits++; }}><Harness view="table" items={items} /></Profiler>);
    });
    const root = screen.getByTestId("viewport");
    expect(root.querySelector(".work-table-scroll > table")).not.toBeNull();
    bounded(() => act(() => measure()));
    // Visit the long assignment and then the clamped end. Its presence is a
    // data-dependent change in the table's intrinsic column widths.
    bounded(() => { root.scrollTop = 75 * 96; fireEvent.scroll(root); });
    bounded(() => act(() => measure()));
    bounded(() => { root.scrollTop = Number.MAX_SAFE_INTEGER; fireEvent.scroll(root); });
    bounded(() => act(() => measure()));
    bounded(() => act(() => { width = 1017; viewportHeight = 391; measure(); }));
    bounded(() => { root.scrollTop = Number.MAX_SAFE_INTEGER; fireEvent.scroll(root); });
    bounded(() => act(() => measure()));
    expect(horizontalBars).toEqual(new Set([false, true]));
    expect(root.querySelectorAll("[data-work-window-key]").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
  });

  it("converges when a table inherits the scrolled tree viewport and a boundary row changes column allocation", () => {
    inheritedTableLayout = true;
    width = 1505;
    viewportHeight = 460;
    // The captured 119-item project had 92 open rows. Synthetic names retain
    // the observed structure: row 64 has four long prerequisites, its peers
    // have one. The geometry derives from these cells, not render parity.
    const items = rows.slice(0, 92).map((item, index) => ({ ...item,
      title: "Common wrapping description ".repeat(3),
      prerequisites: Array.from({ length: index === 64 ? 4 : 1 }, (_, prerequisite) => ({
        id: `${"p".repeat(35)}${prerequisite}`, satisfied: true,
      })),
    }));
    function SwitchView() {
      const viewport = useRef<HTMLDivElement | null>(null);
      const [table, setTable] = useState(false);
      const attach = useCallback((node: HTMLDivElement | null) => {
        viewport.current = node;
        if (node) installViewport(node);
      }, []);
      return <><button onClick={() => setTable(true)}>Switch to table</button>
        <div ref={attach} data-testid="viewport" className="work-panel-list">
          {table ? <WorkTable rows={items} selection={null} onSelect={() => {}}
            sort={null} onSortChange={() => {}} viewport={viewport} />
            : <WorkTree rows={items} universe={items} mode="hierarchy"
              selection={null} onSelect={() => {}} viewport={viewport} />}
          <div style={{ height: 37 }}>End of work list.</div>
        </div></>;
    }
    render(<StrictMode><Profiler id="switch-window" onRender={() => { commits++; }}><SwitchView /></Profiler></StrictMode>);
    const root = screen.getByTestId("viewport");
    act(() => measure());
    root.scrollTop = 6603;
    fireEvent.scroll(root);
    act(() => measure());
    expect(root.scrollTop).toBeGreaterThan(6000);
    const before = commits;
    expect(() => {
      fireEvent.click(screen.getByRole("button", { name: "Switch to table" }));
      act(() => measure());
    }).not.toThrow();
    expect(commits - before).toBeLessThan(20);
    expect(root.querySelector("tbody[data-work-window-key]")).not.toBeNull();
    expect(root.querySelectorAll("[data-work-window-key]").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
  });
});
