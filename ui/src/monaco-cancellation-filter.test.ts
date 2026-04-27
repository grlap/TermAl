import { describe, expect, it, vi } from "vitest";
import {
  installMonacoCancellationRejectionFilter,
  isBenignMonacoCancellationReason,
} from "./monaco-cancellation-filter";

function createCanceledError(stack: string) {
  const error = new Error("Canceled");
  error.name = "Canceled";
  error.stack = stack;
  return error;
}

describe("monaco cancellation rejection filter", () => {
  it("no-ops without a DOM window when no target is provided", () => {
    const originalWindow = globalThis.window;

    vi.stubGlobal("window", undefined);
    try {
      expect(() => installMonacoCancellationRejectionFilter()).not.toThrow();
    } finally {
      vi.stubGlobal("window", originalWindow);
    }
  });

  it("recognizes Monaco diff worker cancellations", () => {
    expect(
      isBenignMonacoCancellationReason(
        createCanceledError(
          [
            "Canceled: Canceled",
            "    at canceled (chunk-OW7E3VRM.js:202:17)",
            "    at EditorWorkerClient2.workerWithSyncedResources (chunk-OW7E3VRM.js:82934:29)",
            "    at async StandaloneEditorWorkerService2.computeDiff (chunk-OW7E3VRM.js:82676:20)",
          ].join("\n"),
        ),
      ),
    ).toBe(true);
  });

  it("does not treat unrelated canceled errors as Monaco worker cancellations", () => {
    expect(
      isBenignMonacoCancellationReason(
        createCanceledError("Canceled: Canceled\n    at cancelUpload"),
      ),
    ).toBe(false);
    expect(
      isBenignMonacoCancellationReason(
        createCanceledError(
          "Canceled: Canceled\n    at UserSearchService.computeDiff",
        ),
      ),
    ).toBe(false);
    expect(isBenignMonacoCancellationReason(new Error("Canceled"))).toBe(false);
  });

  it("prevents only matching unhandled rejections", () => {
    const captured: { listener?: (event: PromiseRejectionEvent) => void } = {};
    const target = {
      addEventListener: vi.fn(
        (type: string, nextListener: EventListenerOrEventListenerObject) => {
          if (
            type === "unhandledrejection" &&
            typeof nextListener === "function"
          ) {
            captured.listener = nextListener as (
              event: PromiseRejectionEvent,
            ) => void;
          }
        },
      ) as Window["addEventListener"],
    };

    installMonacoCancellationRejectionFilter(target);
    installMonacoCancellationRejectionFilter(target);

    expect(target.addEventListener).toHaveBeenCalledTimes(1);
    expect(captured.listener).toBeDefined();

    const matchingEvent = {
      reason: createCanceledError(
        "Canceled: Canceled\n    at StandaloneEditorWorkerService2.computeDiff",
      ),
      preventDefault: vi.fn(),
    } as unknown as PromiseRejectionEvent;
    captured.listener?.(matchingEvent);
    expect(matchingEvent.preventDefault).toHaveBeenCalledTimes(1);

    const unrelatedEvent = {
      reason: createCanceledError("Canceled: Canceled\n    at cancelUpload"),
      preventDefault: vi.fn(),
    } as unknown as PromiseRejectionEvent;
    captured.listener?.(unrelatedEvent);
    expect(unrelatedEvent.preventDefault).not.toHaveBeenCalled();

    const computeDiffOnlyEvent = {
      reason: createCanceledError(
        "Canceled: Canceled\n    at UserSearchService.computeDiff",
      ),
      preventDefault: vi.fn(),
    } as unknown as PromiseRejectionEvent;
    captured.listener?.(computeDiffOnlyEvent);
    expect(computeDiffOnlyEvent.preventDefault).not.toHaveBeenCalled();
  });
});
