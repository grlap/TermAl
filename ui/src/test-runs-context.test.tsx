import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useContext } from "react";
import { TestRunsProvider, TestRunSessionMarker, TestRunsOpenContext } from "./test-runs-context";
import { makeTestRun } from "./test-runs-fixtures";
import { ControlPanelSurface } from "./panels/ControlPanelSurface";
afterEach(cleanup);
it("keeps action-only consumers independent of run updates", () => {
  let renders = 0;
  const open = vi.fn();
  function ActionOnly() {
    const action = useContext(TestRunsOpenContext);
    renders++;
    return <button onClick={() => action?.()}>Open</button>;
  }
  const child = <ActionOnly />;
  const rendered = render(<TestRunsProvider runs={[]} open={open}>{child}</TestRunsProvider>);
  rendered.rerender(<TestRunsProvider runs={[makeTestRun()]} open={open}>{child}</TestRunsProvider>);
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
  render(<TestRunsProvider runs={[]} open={open}><ControlPanelSurface
    gitStatusCount={0} isPreferencesOpen={false} onOpenPreferences={vi.fn()}
    projectCount={0} sessionCount={0} renderSection={() => null} /></TestRunsProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Open Test Runs" }));
  expect(open).toHaveBeenCalledWith();
  expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
});
it("opens the owning session filter and disappears on terminal transition", () => {
  const open = vi.fn();
  const run = makeTestRun();
  const view = (state: "running" | "passed") => <TestRunsProvider runs={[{ ...run, state }]} open={open}>
    <TestRunSessionMarker sessionId="session-owner" />
  </TestRunsProvider>;
  const rendered = render(view("running"));
  fireEvent.click(screen.getByRole("button", { name: "running tests · rust-tests" }));
  expect(open).toHaveBeenCalledWith("session-owner");
  rendered.rerender(view("passed"));
  expect(screen.queryByRole("button")).not.toBeInTheDocument();
});
