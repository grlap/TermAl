import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useContext } from "react";
import { TestRunsProvider, TestRunSessionMarker, TestRunsOpenContext } from "./test-runs-context";
import { makeTestRun } from "./test-runs-fixtures";
import { applyTestRunDelta } from "./test-runs";
import { ControlPanelSurface } from "./panels/ControlPanelSurface";
import { TestRunsRailAction } from "./panels/TestRunsRailAction";
import { TestRunsPanel } from "./panels/TestRunsPanel";
afterEach(cleanup);
it("retains missing-artifact uncertainty in the panel without a rail warning", () => {
  let runs = [makeTestRun({ state: "passed", restartWouldVoidEvidence: null })];
  const view = () => <TestRunsProvider snapshotReady runs={runs} open={vi.fn()}>
    <TestRunsRailAction />
    <TestRunsPanel projects={[]} sessions={[]} />
  </TestRunsProvider>;
  const rendered = render(view());
  runs = applyTestRunDelta(runs, {
    type: "testRunChanged", revision: 2,
    run: { ...runs[0], state: "unknown", interrupted: true, unknownReason: "resultsUnreadable" },
  });
  rendered.rerender(view());
  expect(runs).toHaveLength(1);
  expect(screen.getByText("Evidence impact on restart: unknown")).toBeInTheDocument();
  expect(screen.getByText("unknown · interrupted")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Open Test Runs" }).textContent).toBe("");
});
it("opens all projects without a badge for finished runs regardless of evidence impact", () => {
  const open = vi.fn();
  const renderStatus = (impact: boolean | null) => <TestRunsProvider snapshotReady
    runs={[makeTestRun({ state: "passed", projectId: "another-project", restartWouldVoidEvidence: impact })]} open={open}>
    <TestRunsRailAction />
  </TestRunsProvider>;
  const rendered = render(renderStatus(true));
  fireEvent.click(screen.getByRole("button", { name: "Open Test Runs" }));
  expect(open).toHaveBeenCalledWith(null, undefined, true);
  expect(screen.getByRole("button").textContent).toBe("");
  rendered.rerender(renderStatus(null));
  expect(screen.getByRole("button").textContent).toBe("");
  rendered.rerender(renderStatus(false));
  expect(screen.getByRole("button").textContent).toBe("");
});
it("waits for the snapshot before displaying a count and keeps the panel's loading state", () => {
  render(<TestRunsProvider snapshotReady={false} runs={[]} open={vi.fn()}>
    <TestRunsRailAction /><TestRunsPanel projects={[]} sessions={[]} />
  </TestRunsProvider>);
  expect(screen.getByRole("button", { name: "Open Test Runs" }).textContent).toBe("");
  expect(screen.getByText("Waiting for the test-run index…")).toBeInTheDocument();
});
it.each([
  [1, "1", "Open Test Runs (1 active run)"],
  [2, "2", "Open Test Runs (2 active runs)"],
  [99, "99", "Open Test Runs (99 active runs)"],
  [100, "99", "Open Test Runs (100 active runs)"],
  [145, "99", "Open Test Runs (145 active runs)"],
] as const)("shows the capped badge and exact accessible count for %i active runs", (activeCount, badgeText, label) => {
  const runs = [
    ...Array.from({ length: activeCount }, (_, index) => makeTestRun({ runId: `active-${index}` })),
    makeTestRun({ runId: "finished", state: "passed" }),
  ];
  render(<TestRunsProvider snapshotReady runs={runs} open={vi.fn()}>
    <TestRunsRailAction />
  </TestRunsProvider>);
  const button = screen.getByRole("button", { name: label });
  expect(button).toHaveAttribute("title", label);
  expect(button.querySelector(".control-panel-activity-badge")?.textContent).toBe(badgeText);
});
it("keeps action-only consumers independent of run updates", () => {
  let renders = 0;
  const open = vi.fn();
  function ActionOnly() {
    const action = useContext(TestRunsOpenContext);
    renders++;
    return <button onClick={() => action?.()}>Open</button>;
  }
  const child = <ActionOnly />;
  const rendered = render(<TestRunsProvider snapshotReady runs={[]} open={open}>{child}</TestRunsProvider>);
  rendered.rerender(<TestRunsProvider snapshotReady runs={[makeTestRun()]} open={open}>{child}</TestRunsProvider>);
  expect(renders).toBe(1);
  fireEvent.click(screen.getByRole("button", { name: "Open" }));
  expect(open).toHaveBeenCalledTimes(1);
});
it("truncates the marker in existing toolbar space without a reserved row or overlay", async () => {
  const moduleName = "node:fs";
  const fs = await import(moduleName) as { readFileSync: (path: string, encoding: string) => string };
  const runtime = globalThis as typeof globalThis & { process: { cwd: () => string } };
  const css = fs.readFileSync(`${runtime.process.cwd()}/src/panels/test-runs-panel.css`, "utf8");
  const marker = css.match(/\.pane-view-strip \.test-run-session-marker \{([^}]+)\}/)?.[1] ?? "";
  expect(css).not.toContain("test-run-session-chrome");
  expect(marker).toContain("text-overflow: ellipsis");
  expect(marker).toContain("white-space: nowrap");
  expect(marker).not.toMatch(/position:\s*(absolute|fixed)/);
  expect(css).not.toContain(".session-activity-strip > .test-run-session-marker");
});
it("offers the Test Runs dock action separately from section navigation", () => {
  const open = vi.fn();
  render(<TestRunsProvider snapshotReady runs={[]} open={open}><ControlPanelSurface
    gitStatusCount={0} isPreferencesOpen={false} onOpenPreferences={vi.fn()}
    projectCount={0} sessionCount={0} renderSection={() => null} /></TestRunsProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Open Test Runs" }));
  expect(open).toHaveBeenCalledWith(null, undefined, true);
  expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
});
it("draws the Test Runs dock action as an icon like the other rail actions", () => {
  render(<TestRunsProvider snapshotReady runs={[]} open={vi.fn()}><ControlPanelSurface
    gitStatusCount={0} isPreferencesOpen={false} onOpenPreferences={vi.fn()}
    projectCount={0} sessionCount={0} renderSection={() => null} /></TestRunsProvider>);
  const button = screen.getByRole("button", { name: "Open Test Runs" });
  const preferences = screen.getByRole("button", { name: "Open preferences" });
  // A text glyph rendered as an empty square in the rail; every action draws an SVG.
  expect(button.querySelector("svg[viewBox='0 0 20 20']")).not.toBeNull();
  expect(preferences.querySelector("svg[viewBox='0 0 20 20']")).not.toBeNull();
  expect(button.textContent?.replace(/\s/g, "") ?? "").toBe("");
});
it("opens the owning session filter and disappears on terminal transition", () => {
  const open = vi.fn();
  const run = makeTestRun();
  const view = (state: "running" | "passed") => <TestRunsProvider snapshotReady runs={[{ ...run, state }]} open={open}>
    <TestRunSessionMarker sessionId="session-owner" />
  </TestRunsProvider>;
  const rendered = render(view("running"));
  fireEvent.click(screen.getByRole("button", { name: "running tests · rust-tests" }));
  expect(open).toHaveBeenCalledWith("session-owner");
  rendered.rerender(view("passed"));
  expect(screen.queryByRole("button")).not.toBeInTheDocument();
});
