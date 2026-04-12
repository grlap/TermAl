import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { runTerminalCommand, type TerminalCommandResponse } from "../api";
import {
  TerminalPanel,
  formatTerminalResult,
  formatTerminalWorkdirLabel,
} from "./TerminalPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    runTerminalCommand: vi.fn(),
  };
});

const runTerminalCommandMock = vi.mocked(runTerminalCommand);

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
      projectId={overrides.projectId ?? "project-a"}
      sessionId={overrides.sessionId ?? null}
      showPathControls={overrides.showPathControls ?? true}
      terminalId={overrides.terminalId ?? `terminal-${++terminalCounter}`}
      workdir={overrides.workdir ?? "/repo"}
    />,
  );

  return { ...view, onOpenWorkdir };
}

describe("TerminalPanel", () => {
  beforeEach(() => {
    runTerminalCommandMock.mockReset();
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
  });

  it("runs commands, renders output, clears history, and edits workdir", async () => {
    runTerminalCommandMock.mockResolvedValue(
      makeTerminalResponse({
        command: "npm test",
        durationMs: 42,
        stdout: "passed\n",
        workdir: "/repo",
      }),
    );
    const { onOpenWorkdir } = renderTerminal();

    const workdirInput = screen.getByLabelText("Working directory");
    fireEvent.change(workdirInput, { target: { value: "  /repo/packages/app  " } });
    fireEvent.click(screen.getByRole("button", { name: "Use" }));
    expect(onOpenWorkdir).toHaveBeenCalledWith("/repo/packages/app");

    const commandInput = screen.getByLabelText("Terminal command");
    fireEvent.change(commandInput, { target: { value: "  npm test  " } });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await waitFor(() => {
      expect(runTerminalCommandMock).toHaveBeenCalledWith({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "/repo",
      });
    });
    expect(commandInput).toHaveValue("");
    expect(await screen.findByText("passed")).toBeInTheDocument();
    expect(screen.getByText(/Exit 0 in 42ms/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByText("Run a command in this workspace.")).toBeInTheDocument();
  });

  it("records command errors", async () => {
    runTerminalCommandMock.mockRejectedValue(new Error("Permission denied"));
    renderTerminal({ terminalId: "terminal-error" });

    fireEvent.change(screen.getByLabelText("Terminal command"), {
      target: { value: "rm -rf build" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    expect(await screen.findByText("Permission denied")).toBeInTheDocument();
    expect(screen.getByText(/Failed/)).toBeInTheDocument();
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
    runTerminalCommandMock.mockReturnValue(deferred.promise);
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
    expect(runTerminalCommandMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      deferred.resolve(makeTerminalResponse());
      await deferred.promise;
    });
  });
});
