import { describe, expect, it } from "vitest";
import type { MutableRefObject } from "react";

import {
  coalescePendingStateResyncOptions,
  consumePendingStateResyncOptions,
  type PendingStateResyncOptions,
} from "./app-live-state-resync-options";

describe("state resync option coalescing", () => {
  it("creates an empty option bag from a null start and undefined options", () => {
    expect(coalescePendingStateResyncOptions(null, undefined)).toEqual({
      allowAuthoritativeRollback: false,
      allowUnknownServerInstance: false,
      preserveReconnectFallback: false,
      preserveWatchdogCooldown: false,
      rearmOnSuccess: false,
      rearmUntilLiveEventOnSuccess: false,
      rearmAfterSameInstanceProgressUntilLiveEvent: false,
      rearmOnFailure: false,
    });
  });

  it("retains monotonic boolean flags across later narrower requests", () => {
    const first = coalescePendingStateResyncOptions(null, {
      allowAuthoritativeRollback: true,
      preserveReconnectFallback: true,
      preserveWatchdogCooldown: true,
      rearmOnSuccess: true,
      rearmUntilLiveEventOnSuccess: true,
      rearmAfterSameInstanceProgressUntilLiveEvent: true,
      rearmOnFailure: true,
    });
    const second = coalescePendingStateResyncOptions(first, {
      allowUnknownServerInstance: true,
    });

    expect(second).toEqual({
      allowAuthoritativeRollback: true,
      allowUnknownServerInstance: true,
      preserveReconnectFallback: true,
      preserveWatchdogCooldown: true,
      rearmOnSuccess: true,
      rearmUntilLiveEventOnSuccess: true,
      rearmAfterSameInstanceProgressUntilLiveEvent: true,
      rearmOnFailure: true,
    });
  });

  it("retains live-event rearming across coalesced narrower requests", () => {
    const first = coalescePendingStateResyncOptions(null, {
      rearmOnSuccess: true,
      rearmUntilLiveEventOnSuccess: true,
    });
    const second = coalescePendingStateResyncOptions(first, {
      preserveWatchdogCooldown: true,
    });

    expect(second).toMatchObject({
      preserveWatchdogCooldown: true,
      rearmOnSuccess: true,
      rearmUntilLiveEventOnSuccess: true,
    });
  });

  it("keeps the pending navigation target until another explicit session target replaces it", () => {
    const first = coalescePendingStateResyncOptions(null, {
      openSessionId: "old-session",
      paneId: "left-pane",
    });
    const second = coalescePendingStateResyncOptions(first, {
      paneId: "ignored-pane",
    });
    const third = coalescePendingStateResyncOptions(second, {
      openSessionId: "new-session",
    });

    expect(second.openSessionId).toBe("old-session");
    expect(second.paneId).toBe("left-pane");
    expect(third.openSessionId).toBe("new-session");
    expect(third.paneId).toBeNull();
  });

  it("consumes pending options and clears the ref", () => {
    const pending = coalescePendingStateResyncOptions(null, {
      allowUnknownServerInstance: true,
      openSessionId: "session-1",
      paneId: "pane-1",
    });
    const ref: MutableRefObject<PendingStateResyncOptions | null> = {
      current: pending,
    };

    expect(consumePendingStateResyncOptions(ref)).toEqual(pending);
    expect(ref.current).toBeNull();
    expect(consumePendingStateResyncOptions(ref)).toEqual({
      allowAuthoritativeRollback: false,
      allowUnknownServerInstance: false,
      preserveReconnectFallback: false,
      preserveWatchdogCooldown: false,
      rearmOnSuccess: false,
      rearmUntilLiveEventOnSuccess: false,
      rearmAfterSameInstanceProgressUntilLiveEvent: false,
      rearmOnFailure: false,
    });
  });
});
