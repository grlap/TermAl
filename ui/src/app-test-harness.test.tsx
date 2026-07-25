import { useEffect } from "react";
import { cleanup } from "@testing-library/react";
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
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";

describe("renderApp workspace-layout coordination", () => {
  afterEach(() => {
    cleanup();
    appMock.mockReset();
    setAppTestHooksForTests(null);
  });

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
