import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { WorkItem } from "../work-visualizer-api";
import { WorkTree } from "./WorkTree";

function waits(id: string, on: string[]): WorkItem {
  return { id, shortRef: id, title: id, kind: "task", lifecycle: "open", availability: "blocked", priority: 2, labels: [], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: on, source: "engram", prerequisites: on.map(dep => ({ id: dep, satisfied: false })) };
}

describe("WorkTree", () => {
  it("renders a 4,000-row linear chain without exhausting the stack", () => {
    const rows = Array.from({ length: 4000 }, (_, index) => waits(`c${index}`, index ? [`c${index - 1}`] : []));
    const { container } = render(<WorkTree rows={rows} universe={rows} mode="dependencies" selection={null} onSelect={() => {}} />);
    expect(container.querySelectorAll(".work-row-title")).toHaveLength(4000);
    expect(container.querySelectorAll("[data-deeper]").length).toBeGreaterThan(0);
  });

  it("discloses a parent hidden by the loaded-row filter in the hierarchy view", () => {
    const parent = { ...waits("root", []), kind: "epic" };
    const child = { ...waits("root.1", []), parentId: "root" };
    const { container } = render(<WorkTree rows={[child]} universe={[parent, child]} mode="hierarchy" selection={null} onSelect={() => {}} />);
    expect(container.querySelector("[data-hidden-by-filter]")?.textContent).toBe("parent hidden by filter");
  });

  it("reveals a shared prerequisite's row when its first branch is collapsed", () => {
    const rows = [waits("goal", ["a", "b"]), waits("a", ["c"]), waits("b", ["c"]), waits("c", [])];
    const { container } = render(<WorkTree rows={rows} universe={rows} mode="dependencies" selection={null} onSelect={() => {}} />);
    expect(screen.getByRole("button", { name: "c — c" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Collapse a" }));
    expect(screen.queryByRole("button", { name: "c — c" })).not.toBeInTheDocument();
    // b's reference is not a dead end: it expands a again and focuses c.
    fireEvent.click(screen.getByRole("button", { name: /\+1 shown above/ }));
    const revealed = screen.getByRole("button", { name: "c — c" });
    expect(document.activeElement).toBe(revealed);
    expect(container.querySelectorAll("[data-repeated]")).toHaveLength(1);
  });
});
