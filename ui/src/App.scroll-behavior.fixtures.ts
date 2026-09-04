// Owns App scroll-test setup, teardown and scroll-write queries.
// Does not own scenarios or production scrolling.
// Split from App.scroll-behavior.test.tsx.
import { act, cleanup } from "@testing-library/react";
import { beforeEach, afterEach, vi } from "vitest";
import * as api from "./api";
import { setAppTestHooksForTests } from "./app-test-hooks";
import {
  createScheduledAnimationFrameMocks,
  EventSourceMock,
  makeWorkspaceLayoutResponse,
  flushUiWork,
} from "./app-test-harness";

export function scrollToTopsWithBehavior(
  scrollToMock: ReturnType<typeof vi.fn>,
  behavior: ScrollBehavior,
) {
  return scrollToMock.mock.calls.flatMap((call) => {
    const options = call[0];
    return typeof options === "object" &&
      options !== null &&
      options.behavior === behavior &&
      typeof options.top === "number"
      ? [options.top]
      : [];
  });
}

export function scrollToTopsForElementWithBehavior(
  scrollToMock: ReturnType<typeof vi.fn>,
  element: HTMLElement,
  behavior: ScrollBehavior,
) {
  return scrollToMock.mock.calls.flatMap((call, index) => {
    const options = call[0];
    return scrollToMock.mock.contexts[index] === element &&
      typeof options === "object" &&
      options !== null &&
      options.behavior === behavior &&
      typeof options.top === "number"
      ? [options.top]
      : [];
  });
}

export function installAppScrollTestHarness() {
  const originalScrollTo = HTMLElement.prototype.scrollTo;
  const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
  const originalCancelAnimationFrame = globalThis.cancelAnimationFrame;

  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createScheduledAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({
      workspaces: [],
    });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(
      makeWorkspaceLayoutResponse(),
    );
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    if (originalRequestAnimationFrame === undefined) {
      delete (globalThis as Partial<typeof globalThis>).requestAnimationFrame;
    } else {
      globalThis.requestAnimationFrame = originalRequestAnimationFrame;
    }
    if (originalCancelAnimationFrame === undefined) {
      delete (globalThis as Partial<typeof globalThis>).cancelAnimationFrame;
    } else {
      globalThis.cancelAnimationFrame = originalCancelAnimationFrame;
    }
    window.localStorage.clear();
    if (vi.isFakeTimers()) {
      vi.useRealTimers();
    }
    setAppTestHooksForTests(null);
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });
}
