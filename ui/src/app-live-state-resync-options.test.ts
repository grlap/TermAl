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
      confirmReconnectRecoveryOnAdoption: false,
      forceAdoptEqualOrNewerRevision: null,
      sseReconnectRequestId: null,
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
      confirmReconnectRecoveryOnAdoption: true,
      forceAdoptEqualOrNewerRevision: 7,
      rearmOnFailure: true,
    });
    const second = coalescePendingStateResyncOptions(first, {
      allowUnknownServerInstance: true,
      forceAdoptEqualOrNewerRevision: 5,
    });

    expect(second).toEqual({
      allowAuthoritativeRollback: true,
      allowUnknownServerInstance: true,
      preserveReconnectFallback: true,
      preserveWatchdogCooldown: true,
      rearmOnSuccess: true,
      rearmUntilLiveEventOnSuccess: true,
      rearmAfterSameInstanceProgressUntilLiveEvent: true,
      confirmReconnectRecoveryOnAdoption: true,
      forceAdoptEqualOrNewerRevision: 7,
      sseReconnectRequestId: null,
      rearmOnFailure: true,
    });
  });

  it("raises the forced adoption revision floor across coalesced requests", () => {
    const first = coalescePendingStateResyncOptions(null, {
      forceAdoptEqualOrNewerRevision: 5,
    });
    const second = coalescePendingStateResyncOptions(first, {
      forceAdoptEqualOrNewerRevision: 7,
    });

    expect(second.forceAdoptEqualOrNewerRevision).toBe(7);
  });

  it("keeps the higher forced adoption revision floor when later requests are lower", () => {
    const first = coalescePendingStateResyncOptions(null, {
      forceAdoptEqualOrNewerRevision: 7,
    });
    const second = coalescePendingStateResyncOptions(first, {
      forceAdoptEqualOrNewerRevision: 5,
    });

    expect(second.forceAdoptEqualOrNewerRevision).toBe(7);
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

  it("retains reconnect confirmation-on-adoption once observed", () => {
    const first = coalescePendingStateResyncOptions(null, {
      confirmReconnectRecoveryOnAdoption: true,
    });
    const second = coalescePendingStateResyncOptions(first, {
      preserveWatchdogCooldown: true,
    });

    expect(second).toMatchObject({
      confirmReconnectRecoveryOnAdoption: true,
      preserveWatchdogCooldown: true,
    });
  });

  it("upgrades reconnect confirmation-on-adoption from false to true", () => {
    const first = coalescePendingStateResyncOptions(null, {
      confirmReconnectRecoveryOnAdoption: false,
    });
    const second = coalescePendingStateResyncOptions(first, {
      confirmReconnectRecoveryOnAdoption: true,
    });

    expect(first.confirmReconnectRecoveryOnAdoption).toBe(false);
    expect(second.confirmReconnectRecoveryOnAdoption).toBe(true);
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
      confirmReconnectRecoveryOnAdoption: false,
      forceAdoptEqualOrNewerRevision: null,
      sseReconnectRequestId: null,
      rearmOnFailure: false,
    });
  });

  it("retains the highest SSE reconnect request token across coalesced requests", () => {
    const first = coalescePendingStateResyncOptions(null, {
      sseReconnectRequestId: 3,
    });
    const second = coalescePendingStateResyncOptions(first, {
      sseReconnectRequestId: 9,
    });
    const third = coalescePendingStateResyncOptions(second, {
      sseReconnectRequestId: 5,
    });

    expect(third.sseReconnectRequestId).toBe(9);
  });
});
