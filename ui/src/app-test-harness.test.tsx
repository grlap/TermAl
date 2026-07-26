import { useEffect, useState } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const appMock = vi.hoisted(() => vi.fn());

vi.mock("./App", () => ({
  default: appMock,
}));

import {
  appTestHooks,
  setAppTestHooksForTests,
  type AppTestHooks,
} from "./app-test-hooks";
import {
  createDeferred,
  renderApp,
  submitButtonWithoutSettling,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";

afterEach(() => {
  cleanup();
  appMock.mockReset();
  setAppTestHooksForTests(null);
});

describe("renderApp workspace-layout coordination", () => {
  it("restores the previous hooks when App throws during render", async () => {
    const previousHooks: AppTestHooks = {
      onDeleteProjectPostAwaitPath: vi.fn(),
    };
    setAppTestHooksForTests(previousHooks);
    appMock.mockImplementation(() => {
      throw new Error("render failed");
    });
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});
    const preventExpectedWindowError = (event: ErrorEvent) => {
      const message =
        event.error instanceof Error ? event.error.message : event.message;
      if (message === "render failed") {
        event.preventDefault();
      }
    };
    window.addEventListener("error", preventExpectedWindowError);

    try {
      await expect(renderApp()).rejects.toThrow("render failed");
      expect(appTestHooks).toBe(previousHooks);
    } finally {
      window.removeEventListener("error", preventExpectedWindowError);
      consoleError.mockRestore();
    }
  });

  it("waits until the app enters the commit barrier before releasing it", async () => {
    const barrierEntryReady = createDeferred<() => Promise<void>>();
    appMock.mockImplementation(() => {
      useEffect(() => {
        barrierEntryReady.resolve(async () => {
          const beforeCommit =
            appTestHooks?.beforeWorkspaceLayoutLoadCommit;
          if (!beforeCommit) {
            throw new Error("workspace-layout commit hook was not installed");
          }
          await beforeCommit();
        });
      }, []);
      return <div>App fixture</div>;
    });

    let renderSettled = false;
    const renderPromise = renderApp().then(() => {
      renderSettled = true;
    });
    const enterBarrier = await barrierEntryReady.promise;
    await Promise.resolve();

    expect(renderSettled).toBe(false);

    const barrierPromise = enterBarrier();
    await renderPromise;
    await barrierPromise;

    expect(renderSettled).toBe(true);
    expect(appTestHooks).toBeNull();
  });

  it("fails diagnostically and restores hooks when the barrier is never entered", async () => {
    const previousHooks: AppTestHooks = {
      onDeleteProjectPostAwaitPath: vi.fn(),
    };
    setAppTestHooksForTests(previousHooks);
    appMock.mockImplementation(() => <div>App fixture</div>);

    await expect(renderApp()).rejects.toThrow(
      "renderApp did not reach the workspace-layout commit barrier",
    );
    expect(appTestHooks).toBe(previousHooks);
  });

  it.each([
    "Warning: An update was not wrapped in act(...)",
    "Warning: You seem to have overlapping act() calls",
  ])("fails when a test flow emits %s", async (warning) => {
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    try {
      await expect(
        withVerifiedNoReactActWarnings(async () => {
          console.error(warning);
        }),
      ).rejects.toThrow("React act warning emitted during test flow");
    } finally {
      consoleError.mockRestore();
    }
  });
});

describe("submitButtonWithoutSettling", () => {
  it("keeps an intentionally pending submit out of an async act scope", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const request = createDeferred<void>();

      function PendingForm() {
        const [status, setStatus] = useState("idle");
        return (
          <form
            onSubmit={async (event) => {
              event.preventDefault();
              setStatus("pending");
              await request.promise;
              setStatus("complete");
            }}
          >
            <button type="submit">Submit</button>
            <output>{status}</output>
          </form>
        );
      }

      render(<PendingForm />);
      submitButtonWithoutSettling(
        screen.getByRole("button", { name: "Submit" }),
      );
      expect(screen.getByText("pending")).toBeInTheDocument();

      await act(async () => {
        request.resolve();
        await request.promise;
      });
      expect(screen.getByText("complete")).toBeInTheDocument();
    });
  });
});
