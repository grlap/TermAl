import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ApiRequestError,
  runTerminalCommandStream,
  type TerminalCommandResponse,
} from "../api";
import {
  TerminalPanel,
  formatTerminalResult,
  formatTerminalWorkdirLabel,
  getTerminalPanelHistoryForTests,
  getTerminalPanelListenerCountForTests,
  pruneTerminalPanelHistory,
  resetTerminalPanelStateForTests,
  setTerminalPanelHistoryForTests,
  type TerminalHistoryEntry,
} from "./TerminalPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    runTerminalCommandStream: vi.fn(),
  };
});

const runTerminalCommandStreamMock = vi.mocked(runTerminalCommandStream);

let terminalCounter = 0;

function makeTerminalResponse(
  overrides: Partial<TerminalCommandResponse> = {},
): TerminalCommandResponse {
  return {
    command: "npm test",
    durationMs: 42,
    exitCode: 0,
    outputTruncated: false,
    shell: "sh",
    stderr: "",
    stdout: "ok\n",
    success: true,
    timedOut: false,
    workdir: "/repo",
    ...overrides,
  };
}

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

/**
 * Stubs the scroll-geometry properties on a DOM element that jsdom leaves
 * unimplemented. jsdom reports `scrollHeight` and `clientHeight` as 0 and
 * treats `scrollTop` as a readonly zero because it does not implement
 * layout; tests that exercise scroll behavior in `TerminalPanel` need to
 * pretend the `<div role="log">` has overflowed content and to observe
 * the panel's subsequent writes to `scrollTop`. Callers pass a single
 * config object; every field is optional so individual tests can stub
 * only what they care about.
 */
function stubScrollGeometry(
  element: HTMLElement,
  config: { scrollHeight?: number; clientHeight?: number; scrollTop?: number } = {},
) {
  if (config.scrollHeight !== undefined) {
    Object.defineProperty(element, "scrollHeight", {
      configurable: true,
      value: config.scrollHeight,
    });
  }
  if (config.clientHeight !== undefined) {
    Object.defineProperty(element, "clientHeight", {
      configurable: true,
      value: config.clientHeight,
    });
  }
  if (config.scrollTop !== undefined) {
    Object.defineProperty(element, "scrollTop", {
      configurable: true,
      value: config.scrollTop,
      writable: true,
    });
  }
}

function renderTerminal(
  overrides: Partial<{
    onOpenWorkdir: (path: string) => void;
    projectId: string | null;
    sessionId: string | null;
    showPathControls: boolean;
    terminalId: string;
    workdir: string | null;
  }> = {},
) {
  const onOpenWorkdir = overrides.onOpenWorkdir ?? vi.fn();
  const view = render(
    <TerminalPanel
      onOpenWorkdir={onOpenWorkdir}
      projectId={"projectId" in overrides ? overrides.projectId : "project-a"}
      sessionId={"sessionId" in overrides ? overrides.sessionId : null}
      showPathControls={overrides.showPathControls ?? true}
      terminalId={overrides.terminalId ?? `terminal-${++terminalCounter}`}
      workdir={"workdir" in overrides ? (overrides.workdir ?? null) : "/repo"}
    />,
  );

  return { ...view, onOpenWorkdir };
}

async function expectTerminalStdout(container: HTMLElement, expected: string) {
  await waitFor(() => {
    expect(container.querySelector("pre.terminal-output-stdout")?.textContent).toBe(
      expected,
    );
  });
}

async function expectTerminalStderr(container: HTMLElement, expected: string) {
  await waitFor(() => {
    expect(container.querySelector("pre.terminal-output-stderr")?.textContent).toBe(
      expected,
    );
  });
}

describe("TerminalPanel", () => {
  beforeEach(() => {
    runTerminalCommandStreamMock.mockReset();
  });

  afterEach(() => {
    resetTerminalPanelStateForTests();
  });

  it("formats terminal labels and results", () => {
    expect(formatTerminalWorkdirLabel("")).toBe("No working directory");
    expect(formatTerminalWorkdirLabel("/repo/packages/app")).toBe("app");
    expect(formatTerminalWorkdirLabel(String.raw`C:\repo\service`)).toBe("service");

    expect(formatTerminalResult(null)).toBe("Done");
    expect(formatTerminalResult(makeTerminalResponse({ durationMs: 999 }))).toBe(
      "Exit 0 in 999ms",
    );
    expect(
      formatTerminalResult(makeTerminalResponse({ durationMs: 1250, exitCode: 7 })),
    ).toBe("Exit 7 in 1.3s");
    expect(
      formatTerminalResult(
        makeTerminalResponse({ durationMs: 60_000, exitCode: null, timedOut: true }),
      ),
    ).toBe("Timed out after 60.0s");
    expect(
      formatTerminalResult(makeTerminalResponse({ durationMs: 1250, exitCode: null })),
    ).toBe("Exit signal in 1.3s");
  });

  it("runs commands, renders output, clears history, and edits workdir", async () => {
    runTerminalCommandStreamMock.mockResolvedValue(
      makeTerminalResponse({
        command: "npm test",
        durationMs: 42,
        stdout: "passed\n",
        workdir: "/repo",
      }),
    );
    const { container, onOpenWorkdir } = renderTerminal();

    const workdirInput = screen.getByLabelText("Working directory");
    fireEvent.change(workdirInput, { target: { value: "  /repo/packages/app  " } });
    fireEvent.click(screen.getByRole("button", { name: "Use" }));
    expect(onOpenWorkdir).toHaveBeenCalledWith("/repo/packages/app");

    const commandInput = screen.getByLabelText("Terminal command");
    fireEvent.change(commandInput, { target: { value: "  npm test  " } });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await waitFor(() => {
      expect(runTerminalCommandStreamMock).toHaveBeenCalledWith(
        {
          command: "npm test",
          projectId: "project-a",
          sessionId: null,
          workdir: "/repo",
        },
        expect.objectContaining({ onOutput: expect.any(Function) }),
      );
    });
    expect(commandInput).toHaveValue("");
    await expectTerminalStdout(container, "passed\n");
    expect(screen.getByText(/Exit 0 in 42ms/)).toBeInTheDocument();
    expect(
      Array.from(container.querySelectorAll(".terminal-io-label")).map((label) =>
        label.getAttribute("aria-hidden"),
      ),
    ).toEqual(["true", "true"]);

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByText("Run a command in this workspace.")).toBeInTheDocument();
  });

  it("keeps the live terminal subscription after clearing history", async () => {
    runTerminalCommandStreamMock
      .mockResolvedValueOnce(makeTerminalResponse({ stdout: "first\n" }))
      .mockResolvedValueOnce(makeTerminalResponse({ stdout: "second\n" }));
    const { container } = renderTerminal({ terminalId: "terminal-clear-subscription" });

    const commandInput = screen.getByLabelText("Terminal command");
    fireEvent.change(commandInput, { target: { value: "echo first" } });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));
    await expectTerminalStdout(container, "first\n");

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByText("Run a command in this workspace.")).toBeInTheDocument();

    fireEvent.change(commandInput, { target: { value: "echo second" } });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await expectTerminalStdout(container, "second\n");
    expect(runTerminalCommandStreamMock).toHaveBeenCalledTimes(2);
  });

  it("renders streamed output before the command completes", async () => {
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockImplementation((_payload, options) => {
      options?.onOutput?.({ stream: "stdout", text: "building...\n" });
      return deferred.promise;
    });
    const { container } = renderTerminal({ terminalId: "terminal-streaming-output" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await expectTerminalStdout(container, "building...\n");
    expect(screen.getByRole("button", { name: "Running" })).toBeDisabled();
    expect(screen.queryByText("Waiting for command output...")).not.toBeInTheDocument();

    await act(async () => {
      deferred.resolve(
        makeTerminalResponse({ stdout: "building...\ndone\n" }),
      );
      await deferred.promise;
    });

    await expectTerminalStdout(container, "building...\ndone\n");
    expect(
      container.querySelector("pre.terminal-output-stderr")?.textContent ?? "",
    ).toBe("");
    expect(screen.getByText(/Exit 0 in 42ms/)).toBeInTheDocument();
  });

  it("appends multiple streamed stdout and stderr callbacks before completion", async () => {
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockImplementation((_payload, options) => {
      options?.onOutput?.({ stream: "stdout", text: "one\n" });
      options?.onOutput?.({ stream: "stdout", text: "two\n" });
      options?.onOutput?.({ stream: "stderr", text: "warn\n" });
      return deferred.promise;
    });
    const { container } = renderTerminal({
      terminalId: "terminal-streaming-multiple-output",
    });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await expectTerminalStdout(container, "one\ntwo\n");
    await expectTerminalStderr(container, "warn\n");

    await act(async () => {
      deferred.resolve(
        makeTerminalResponse({ stderr: "warn\n", stdout: "one\ntwo\nthree\n" }),
      );
      await deferred.promise;
    });

    await expectTerminalStdout(container, "one\ntwo\nthree\n");
    await expectTerminalStderr(container, "warn\n");
  });

  it("records command errors", async () => {
    runTerminalCommandStreamMock.mockRejectedValue(new Error("Permission denied"));
    renderTerminal({ terminalId: "terminal-error" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "rm -rf build" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    expect(await screen.findByText("Permission denied")).toBeInTheDocument();
    expect(screen.getByText(/Failed/)).toBeInTheDocument();
  });

  it("adds retry guidance for terminal rate limits", async () => {
    runTerminalCommandStreamMock.mockRejectedValue(
      new ApiRequestError(
        "request-failed",
        "too many local terminal commands are already running; limit is 4",
        { status: 429 },
      ),
    );
    renderTerminal({ terminalId: "terminal-rate-limit" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    expect(
      await screen.findByText(
        "too many local terminal commands are already running; limit is 4 (rate limit - try again in a moment)",
      ),
    ).toBeInTheDocument();
  });

  it("does not add rate-limit guidance to non-rate-limit errors", async () => {
    runTerminalCommandStreamMock.mockRejectedValue(
      new ApiRequestError("request-failed", "server error", { status: 500 }),
    );
    renderTerminal({ terminalId: "terminal-server-error" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    const errorOutput = await screen.findByText("server error", {
      selector: "pre.terminal-output-error",
    });
    expect(errorOutput.textContent).toBe("server error");
    expect(
      screen.queryByText(/rate limit - try again in a moment/),
    ).not.toBeInTheDocument();
  });

  it("guards command submission until workdir, scope, command, and idle state are ready", async () => {
    const missingWorkdir = renderTerminal({ terminalId: "terminal-no-workdir", workdir: null });
    expect(screen.getByLabelText("Terminal command")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    missingWorkdir.unmount();

    const missingScope = renderTerminal({
      projectId: null,
      sessionId: null,
      terminalId: "terminal-no-scope",
    });
    expect(screen.getByRole("alert")).toHaveTextContent(
      "This terminal is no longer associated with a live session or project.",
    );
    expect(screen.getByLabelText("Terminal command")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    missingScope.unmount();

    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockReturnValue(deferred.promise);
    renderTerminal({ terminalId: "terminal-running" });

    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Running" })).toBeDisabled();
    });
    expect(screen.getByLabelText("Terminal command")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Clear" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Running" }));
    expect(runTerminalCommandStreamMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      deferred.resolve(makeTerminalResponse());
      await deferred.promise;
    });
  });

  it("renders stderr, truncation notices, and docked path controls", async () => {
    runTerminalCommandStreamMock.mockResolvedValue(
      makeTerminalResponse({
        outputTruncated: true,
        stderr: "warn\n",
        stdout: "",
      }),
    );
    const { container } = renderTerminal({
      showPathControls: false,
      terminalId: "terminal-docked",
    });

    expect(screen.queryByLabelText("Working directory")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await expectTerminalStderr(container, "warn\n");
    expect(screen.getByText("Output was truncated.")).toBeInTheDocument();
    expect(runTerminalCommandStreamMock).toHaveBeenCalledWith(
      {
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "/repo",
      },
      expect.objectContaining({ onOutput: expect.any(Function) }),
    );
  });

  it("preserves in-flight command results across remounts", async () => {
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockReturnValue(deferred.promise);
    const view = renderTerminal({ terminalId: "terminal-remount" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await screen.findByText("Waiting for command output...");
    view.unmount();

    const remounted = renderTerminal({ terminalId: "terminal-remount" });
    expect(screen.getByText("Waiting for command output...")).toBeInTheDocument();

    await act(async () => {
      deferred.resolve(makeTerminalResponse({ stdout: "after remount\n" }));
      await deferred.promise;
    });

    await expectTerminalStdout(remounted.container, "after remount\n");
    expect(screen.getByText(/Exit 0 in 42ms/)).toBeInTheDocument();
    expect(runTerminalCommandStreamMock).toHaveBeenCalledTimes(1);
  });

  it("marks stale running history as interrupted when no command is in flight", async () => {
    const staleEntry: TerminalHistoryEntry = {
      command: "npm test",
      error: null,
      id: "entry-stale",
      response: null,
      startedAt: "12:00:00",
      status: "running",
      workdir: "/repo",
    };
    setTerminalPanelHistoryForTests("terminal-stale", [staleEntry]);

    renderTerminal({ terminalId: "terminal-stale" });

    expect(await screen.findByText("Interrupted")).toBeInTheDocument();
    expect(await screen.findByText(/Failed/)).toBeInTheDocument();
  });

  // NOTE: this test exercises the running-entry guard and the
  // `liveTerminalIds` membership guard only. Stores seeded via
  // `setTerminalPanelHistoryForTests` always have empty listener sets, so
  // the `listeners.size === 0` branch in `pruneClosedTerminalHistory` is
  // trivially satisfied here. The separate "does not prune a mounted
  // terminal store even when the tab id is absent" test below covers the
  // listeners-populated branch by rendering a real `TerminalPanel`.
  it("prunes closed terminal histories but keeps running or still-active stores", () => {
    const doneEntry: TerminalHistoryEntry = {
      command: "echo done",
      error: null,
      id: "entry-done",
      response: makeTerminalResponse(),
      startedAt: "12:00:00",
      status: "done",
      workdir: "/repo",
    };
    const runningEntry: TerminalHistoryEntry = {
      ...doneEntry,
      id: "entry-running",
      response: null,
      status: "running",
    };
    setTerminalPanelHistoryForTests("terminal-active", [doneEntry]);
    setTerminalPanelHistoryForTests("terminal-closed", [doneEntry]);
    setTerminalPanelHistoryForTests("terminal-running-closed", [runningEntry]);

    pruneTerminalPanelHistory(["terminal-active"]);

    expect(getTerminalPanelHistoryForTests("terminal-active")).toEqual([doneEntry]);
    expect(getTerminalPanelHistoryForTests("terminal-closed")).toBeNull();
    expect(getTerminalPanelHistoryForTests("terminal-running-closed")).toEqual([
      runningEntry,
    ]);
  });

  it("does not prune a mounted terminal store even when the tab id is absent", async () => {
    runTerminalCommandStreamMock.mockResolvedValueOnce(
      makeTerminalResponse({
        command: "echo mounted",
        stdout: "mounted\n",
      }),
    );
    const { container } = renderTerminal({ terminalId: "terminal-mounted" });

    // The mounted TerminalPanel should have exactly one live subscriber
    // (its own `useSyncExternalStore` subscription) attached to the store.
    expect(getTerminalPanelListenerCountForTests("terminal-mounted")).toBe(1);

    pruneTerminalPanelHistory(["other-id"]);

    // The load-bearing assertion: after prune, the store entry must still
    // be the same instance with its listener set intact. Without this,
    // a wipe-and-recreate refactor (delete the store in prune, then
    // recreate it lazily inside the next `getOrCreateTerminalHistoryStore`
    // call) could silently pass — the new store would have an empty
    // listener set and the component's stale subscription would never be
    // notified, but the command output below could still appear if the
    // refactor also re-ran subscribe. Pinning `listeners.size === 1` here
    // proves the existing store survived untouched.
    expect(getTerminalPanelListenerCountForTests("terminal-mounted")).toBe(1);

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "echo mounted" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await expectTerminalStdout(container, "mounted\n");
    expect(runTerminalCommandStreamMock).toHaveBeenCalledTimes(1);
  });

  it("does not start two commands from same-tick submit events", async () => {
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockReturnValue(deferred.promise);
    renderTerminal({ terminalId: "terminal-double-submit" });

    const commandInput = screen.getByLabelText("Terminal command");
    fireEvent.change(commandInput, { target: { value: "npm test" } });
    const form = commandInput.closest("form");
    expect(form).not.toBeNull();

    act(() => {
      form!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
      form!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });

    expect(runTerminalCommandStreamMock).toHaveBeenCalledTimes(1);
    await act(async () => {
      deferred.resolve(makeTerminalResponse());
      await deferred.promise;
    });
  });

  it("does not force terminal history to the bottom after the user scrolls up", async () => {
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockReturnValue(deferred.promise);
    const { container } = renderTerminal({ terminalId: "terminal-scroll" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));
    const log = await screen.findByRole("log");
    stubScrollGeometry(log, { scrollHeight: 1000, clientHeight: 200, scrollTop: 100 });
    fireEvent.scroll(log);

    await act(async () => {
      deferred.resolve(makeTerminalResponse({ stdout: "new output\n" }));
      await deferred.promise;
    });

    await expectTerminalStdout(container, "new output\n");
    expect(log.scrollTop).toBe(100);
  });

  it("never writes a negative scrollTop when content is shorter than the viewport", async () => {
    // Regression for the `Math.max(scrollHeight - clientHeight, 0)` clamp in
    // `scrollTerminalHistoryToBottom`. When the terminal history is shorter
    // than its viewport (e.g. empty command, narrow terminal window that
    // shrank after a completed command), `scrollHeight - clientHeight` is
    // negative. Without the clamp, `scrollTerminalHistoryToBottom` would
    // write that negative value to `node.scrollTop`, which is harmless in
    // real browsers (they silently clamp) but violates the invariant
    // "never write a value the DOM would reject." Pin the invariant so a
    // future edit that drops `Math.max` fails this test.
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockReturnValue(deferred.promise);
    const { container } = renderTerminal({ terminalId: "terminal-clamp" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    const log = await screen.findByRole("log");
    // Content is shorter than the viewport: `scrollHeight - clientHeight`
    // is `-100`, which the clamp must rewrite to `0`. `scrollTop` starts
    // at `0` and the caller is already at bottom (bottomGap = -100 <= 4),
    // so `shouldStickToBottomRef` stays `true` and the next history
    // update triggers a new `scrollTerminalHistoryToBottom` call.
    stubScrollGeometry(log, {
      scrollHeight: 300,
      clientHeight: 400,
      scrollTop: 0,
    });
    fireEvent.scroll(log);

    await act(async () => {
      deferred.resolve(makeTerminalResponse({ stdout: "tiny\n" }));
      await deferred.promise;
    });

    await expectTerminalStdout(container, "tiny\n");
    expect(log.scrollTop).toBe(0);
  });

  it("skips the scrollTop write when already within 1 px of the bottom", async () => {
    // Regression for the `Math.abs(node.scrollTop - nextScrollTop) > 1`
    // steady-state tolerance inside `scrollTerminalHistoryToBottom`.
    // jsdom's `Object.defineProperty`-backed `scrollTop` is a live mutable
    // value: every assignment shows up verbatim. So if the tolerance
    // regressed to a strict `!==`, a node whose current `scrollTop` is
    // `799` (exactly 1 px above the computed target `800`) would get its
    // `scrollTop` overwritten to `800`, producing a pointless DOM write
    // on every layout pass. The check must leave `scrollTop === 799`
    // untouched.
    const deferred = createDeferred<TerminalCommandResponse>();
    runTerminalCommandStreamMock.mockReturnValue(deferred.promise);
    const { container } = renderTerminal({ terminalId: "terminal-tolerance" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "npm test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    const log = await screen.findByRole("log");
    // `scrollHeight - clientHeight = 800`, `scrollTop = 799`, so
    // `bottomGap = 1 <= 4` → `isTerminalHistoryScrolledToBottom` returns
    // `true` and `shouldStickToBottomRef` stays `true`. When the next
    // history update fires the `useLayoutEffect`, the helper computes
    // `nextScrollTop = max(800, 0) = 800` and `|799 - 800| = 1`, which
    // is NOT strictly greater than `1`, so the helper must short-circuit
    // without touching `scrollTop`.
    stubScrollGeometry(log, {
      scrollHeight: 1000,
      clientHeight: 200,
      scrollTop: 799,
    });
    fireEvent.scroll(log);

    await act(async () => {
      deferred.resolve(makeTerminalResponse({ stdout: "steady\n" }));
      await deferred.promise;
    });

    await expectTerminalStdout(container, "steady\n");
    expect(log.scrollTop).toBe(799);
  });

  it("does not clobber dirty workdir edits when the prop changes", () => {
    const { onOpenWorkdir, rerender } = renderTerminal({
      terminalId: "terminal-workdir-draft",
      workdir: "/repo",
    });
    const workdirInput = screen.getByLabelText("Working directory");

    fireEvent.focus(workdirInput);
    fireEvent.change(workdirInput, { target: { value: "/repo/typed" } });
    fireEvent.blur(workdirInput);
    rerender(
      <TerminalPanel
        onOpenWorkdir={onOpenWorkdir}
        projectId="project-a"
        sessionId={null}
        terminalId="terminal-workdir-draft"
        workdir="/repo/from-reconcile"
      />,
    );

    expect(workdirInput).toHaveValue("/repo/typed");
  });

  it("does not clobber focused dirty workdir edits when the prop changes", () => {
    const { onOpenWorkdir, rerender } = renderTerminal({
      terminalId: "terminal-focused-workdir-draft",
      workdir: "/repo",
    });
    const workdirInput = screen.getByLabelText("Working directory") as HTMLInputElement;

    // Use the DOM `.focus()` method rather than `fireEvent.focus(input)`:
    // `fireEvent.focus` only dispatches the event, it does not change
    // `document.activeElement` in jsdom, so `toHaveFocus()` would trivially
    // pass for whatever element happened to be active (or fail if nothing
    // was). We want to actually move the active element to the input so the
    // rerender's draft-sync effect runs against a truly focused input.
    workdirInput.focus();
    fireEvent.change(workdirInput, { target: { value: "/repo/focused" } });
    rerender(
      <TerminalPanel
        onOpenWorkdir={onOpenWorkdir}
        projectId="project-a"
        sessionId={null}
        terminalId="terminal-focused-workdir-draft"
        workdir="/repo/from-reconcile"
      />,
    );

    expect(workdirInput).toHaveFocus();
    expect(workdirInput).toHaveValue("/repo/focused");
  });
});
