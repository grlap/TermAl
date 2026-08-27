// Owns: focused lifecycle tests for deferred bottom-restore timer authority.
// Does not own: page measurement, user-scroll classification, or scroll writes.

import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useVirtualizedConversationDeferredBottomRestore } from "./virtualized-conversation-deferred-bottom-restore";

afterEach(() => {
  vi.useRealTimers();
});

describe("useVirtualizedConversationDeferredBottomRestore", () => {
  it("retains explicit pending authority until the consumer clears it", () => {
    vi.useFakeTimers();
    const bumpLayoutVersion = vi.fn();
    const hook = renderHook(() =>
      useVirtualizedConversationDeferredBottomRestore({ bumpLayoutVersion }),
    );

    act(() => hook.result.current.scheduleLayoutVersion(200));
    expect(hook.result.current.isPending()).toBe(true);

    act(() => vi.advanceTimersByTime(200));
    expect(bumpLayoutVersion).toHaveBeenCalledTimes(1);
    expect(hook.result.current.isPending()).toBe(true);

    act(() => hook.result.current.clear());
    expect(hook.result.current.isPending()).toBe(false);
  });

  it("cancels the pending retry on unmount", () => {
    vi.useFakeTimers();
    const bumpLayoutVersion = vi.fn();
    const hook = renderHook(() =>
      useVirtualizedConversationDeferredBottomRestore({ bumpLayoutVersion }),
    );

    act(() => hook.result.current.scheduleLayoutVersion(200));
    hook.unmount();
    act(() => vi.advanceTimersByTime(200));

    expect(bumpLayoutVersion).not.toHaveBeenCalled();
  });
});
