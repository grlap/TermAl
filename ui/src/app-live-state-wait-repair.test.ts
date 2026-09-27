import { afterEach, expect, it, vi } from "vitest";
import { createWaitSnapshotRepairRetry } from "./app-live-state-wait-repair";
import { coalescePendingStateResyncOptions } from "./app-live-state-resync-options";
import { RECONNECT_STATE_RESYNC_DELAY_MS } from "./app-shell-internals";

afterEach(() => { vi.useRealTimers(); });

it("retains wait evidence but strips coalesced one-shot proof and permissions", () => {
  vi.useFakeTimers();
  const request = vi.fn();
  const retry = createWaitSnapshotRepairRetry(request, () => true);
  retry.schedule(coalescePendingStateResyncOptions(null, {
    allowSameServerEqualRevision: true, openSessionId: "owner", paneId: "left",
    sseReconnectRequestId: 7, preserveWatchdogCooldown: true,
    confirmReconnectRecoveryOnAdoption: true,
    allowAuthoritativeRollback: true, allowUnknownServerInstance: true,
    waitRepair: { observedServerInstanceId: "a", replacementServerInstanceId: "b" },
  }));
  retry.schedule(coalescePendingStateResyncOptions(null, {
    allowSameServerEqualRevision: true, sseReconnectRequestId: 3,
    rearmOnFailure: true, forceAdoptEqualOrNewerRevision: 10,
    waitRepair: { observedServerInstanceId: "a" },
  }));
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS);
  expect(request).toHaveBeenCalledTimes(1);
  expect(request).toHaveBeenCalledWith({
    allowSameServerEqualRevision: true,
    waitRepair: { observedServerInstanceId: "a", replacementServerInstanceId: "b" },
  });
  expect(retry.take()).toBeUndefined();
  retry.dispose();
});

it("keeps the obligation while offline, then retries once online", () => {
  vi.useFakeTimers();
  const request = vi.fn();
  let online = false;
  const retry = createWaitSnapshotRepairRetry(request, () => online);
  retry.schedule(coalescePendingStateResyncOptions(null, {
    allowSameServerEqualRevision: true, waitRepair: { observedServerInstanceId: "a" },
  }));
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS);
  expect(request).not.toHaveBeenCalled();
  online = true;
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS * 2);
  expect(request).toHaveBeenCalledTimes(1);
  retry.dispose();
});

it("lets a fresh request take pending intent and cancels the old timer", () => {
  vi.useFakeTimers();
  const request = vi.fn();
  const retry = createWaitSnapshotRepairRetry(request, () => true);
  retry.schedule(coalescePendingStateResyncOptions(null, {
    allowSameServerEqualRevision: true, waitRepair: { observedServerInstanceId: "a" },
  }));
  expect(retry.take()).toMatchObject({ waitRepair: { observedServerInstanceId: "a" } });
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS * 2);
  expect(request).not.toHaveBeenCalled();
  retry.dispose();
});

it("clears pending work on successful repair or disposal", () => {
  vi.useFakeTimers();
  const request = vi.fn();
  const retry = createWaitSnapshotRepairRetry(request, () => true);
  const options = coalescePendingStateResyncOptions(null, {
    allowSameServerEqualRevision: true, waitRepair: { observedServerInstanceId: "a" },
  });
  retry.schedule(options);
  retry.complete();
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS * 2);
  expect(request).not.toHaveBeenCalled();
  retry.schedule(options);
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS);
  expect(request).toHaveBeenCalledTimes(1);
  retry.schedule(options);
  retry.dispose();
  vi.advanceTimersByTime(RECONNECT_STATE_RESYNC_DELAY_MS * 4);
  expect(request).toHaveBeenCalledTimes(1);
});
