// Registered-wait projection and pane/board visibility, not launcher liveness.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { makeTestRun, makeTestRunWait } from "./test-runs-fixtures";
import { applyTestRunWaitDelta, testRunWaitPrompt } from "./test-run-waits";
import { TestRunsProvider } from "./test-runs-context";
import { TestRunWaitIndicator, TestRunWaitFailureNotice, TestRunWaitsContext } from "./test-run-waits-context";
import { useContext } from "react";
import { SessionActivityStrip } from "./panels/session-activity-cards";
import { isSessionDeltaEvent } from "./app-live-state-delta-events";

afterEach(() => { cleanup(); vi.useRealTimers(); });

it("uses wait identity, not run identity, and removes only the consumed wait", () => {
  const first = makeTestRunWait();
  const second = makeTestRunWait({ id: "wait-two" });
  const both = applyTestRunWaitDelta([first], { type: "testRunWaitCreated", serverInstanceId: "test-server", revision: 2, wait: second });
  expect(both).toHaveLength(2);
  const consumed = { type: "testRunWaitConsumed" as const, serverInstanceId: "test-server", revision: 3, sessionId: first.sessionId, waitId: first.id, reason: "sessionStopped" as const };
  expect(applyTestRunWaitDelta(both, consumed)).toEqual([second]);
  expect(isSessionDeltaEvent(consumed)).toBe(false);
  expect(isSessionDeltaEvent({ type: "testRunWaitResumeDispatchFailed", revision: 4, sessionId: first.sessionId, error: "failed" })).toBe(false);
});

it("never infers waiting from a run or notify target and retains labels when the index drops a run", () => {
  const wait = makeTestRunWait();
  const now = Date.parse("2026-09-25T12:03:12Z");
  expect(testRunWaitPrompt([], [makeTestRun()], wait.sessionId, now)).toBeNull();
  expect(testRunWaitPrompt([wait], [makeTestRun()], "other", now)).toBeNull();
  expect(testRunWaitPrompt([wait], [makeTestRun()], wait.sessionId, now)).toContain("stage rust-tests, 3m12s");
  expect(testRunWaitPrompt([wait], [], wait.sessionId, now)).toContain("test-one (full, not indexed)");
  expect(testRunWaitPrompt([wait], [], wait.sessionId, now + 60000)).toBe(testRunWaitPrompt([wait], [], wait.sessionId, now));
});

it("updates visible wait time without re-announcing it in the live region", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-09-25T12:03:12Z"));
  const wait = makeTestRunWait();
  render(<TestRunsProvider snapshotReady runs={[makeTestRun()]} waits={[wait]} open={vi.fn()}>
    <SessionActivityStrip session={{ id: wait.sessionId, agent: "Codex", status: "idle" }} />
    <TestRunWaitIndicator sessionId={wait.sessionId} status="idle" />
  </TestRunsProvider>);
  const live = screen.getByRole("status");
  const announcement = live.textContent;
  expect(announcement).toContain("test-one");
  expect(announcement).not.toContain("3m12s");
  expect(screen.getByRole("tooltip")).toHaveTextContent("3m12s");
  act(() => vi.advanceTimersByTime(3000));
  expect(screen.getByRole("tooltip")).toHaveTextContent("3m15s");
  expect(live.textContent).toBe(announcement);
});

it("keeps consumers of an empty wait context stable across run updates", () => {
  let renders = 0;
  function Consumer() { useContext(TestRunWaitsContext); renders++; return null; }
  const child = <Consumer />;
  const rendered = render(<TestRunsProvider snapshotReady runs={[]} open={vi.fn()}>{child}</TestRunsProvider>);
  rendered.rerender(<TestRunsProvider snapshotReady runs={[makeTestRun()]} waits={[]} open={vi.fn()}>{child}</TestRunsProvider>);
  expect(renders).toBe(1);
});

it("keeps the failure notice session-scoped and explicitly dismissible", () => {
  const dismiss = vi.fn();
  const failure = { type: "testRunWaitResumeDispatchFailed" as const, revision: 4, sessionId: "owner", error: "resume unavailable" };
  const view = (runs = [makeTestRun()]) => <TestRunsProvider snapshotReady runs={runs} failures={{ owner: failure }} dismissFailure={dismiss} open={vi.fn()}>
    <TestRunWaitFailureNotice sessionId="owner" /><TestRunWaitFailureNotice sessionId="other" />
  </TestRunsProvider>;
  const rendered = render(view());
  expect(screen.getAllByText("Test-run resume failed")).toHaveLength(1);
  fireEvent.click(screen.getByText("Test-run resume failed"));
  expect(screen.getByText("resume unavailable")).toBeInTheDocument();
  rendered.rerender(view([]));
  expect(screen.getByText("resume unavailable")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss test-run resume failure" }));
  expect(dismiss).toHaveBeenCalledWith("owner");
});

it("announces a failure from one surface only: the pane notice, not the board's", () => {
  const failure = { type: "testRunWaitResumeDispatchFailed" as const, revision: 4, sessionId: "owner", error: "resume unavailable" };
  render(<TestRunsProvider snapshotReady runs={[]} failures={{ owner: failure }} open={vi.fn()}>
    <TestRunWaitFailureNotice sessionId="owner" />
    <TestRunWaitFailureNotice sessionId="owner" announce={false} />
  </TestRunsProvider>);
  expect(screen.getAllByText("Test-run resume failed")).toHaveLength(2);
  expect(screen.getAllByRole("status")).toHaveLength(1);
  expect(screen.getAllByTitle("resume unavailable")).toHaveLength(2);
});


it("constrains the board wait label to one ellipsized line", async () => {
  const nodeFsModule = "node:fs";
  const nodeUrlModule = "node:url";
  const { readFileSync } = await import(nodeFsModule) as { readFileSync: (path: string, encoding: "utf8") => string };
  const { fileURLToPath } = await import(nodeUrlModule) as { fileURLToPath: (url: string) => string };
  // Resolve from this module, not the runner's working directory.
  const moduleUrl = import.meta.url;
  const css = readFileSync(fileURLToPath(new URL("./panels/test-runs-panel.css", moduleUrl).href), "utf8");
  const rule = css.match(/\.test-run-wait-indicator \{([^}]+)\}/)?.[1] ?? "";
  expect(rule).toContain("white-space: nowrap");
  expect(rule).toContain("text-overflow: ellipsis");
  expect(rule).toContain("overflow: hidden");
});

it("shows pane and board waits only when idle and clears them without adding transcript content", () => {
  const wait = makeTestRunWait();
  const view = (status: "idle" | "active", pending = true) => <TestRunsProvider snapshotReady runs={[makeTestRun()]} waits={pending ? [wait] : []} open={vi.fn()}>
    <SessionActivityStrip session={{ id: wait.sessionId, agent: "Codex", status }} />
    <TestRunWaitIndicator sessionId={wait.sessionId} status={status} />
  </TestRunsProvider>;
  const rendered = render(view("idle"));
  expect(screen.getByRole("status")).toHaveTextContent("is waiting for test runs");
  expect(rendered.container.querySelector(".test-run-wait-indicator")).toHaveTextContent("test-one");
  expect(rendered.container.querySelector(".message-card")).toBeNull();
  rendered.rerender(view("active"));
  expect(screen.getByRole("status")).toHaveTextContent("is working");
  expect(rendered.container.querySelector(".test-run-wait-indicator")).toBeNull();
  rendered.rerender(view("idle", false));
  expect(screen.getByRole("status")).toHaveTextContent("is idle");
});
