// app-live-state-resync-options.ts
//
// Owns the pure pending-option coalescing helpers for
// `useAppLiveState`'s /api/state resync loop.
//
// Split out of: ui/src/app-live-state.ts. Keep this module free of React hook
// state; the hook owns when pending options are produced and consumed.

import type { MutableRefObject } from "react";

// Durable wait-repair evidence, independent of one-shot reconnect permissions.
export type WaitSnapshotRepairIntent = {
  observedServerInstanceId: string | null;
  replacementServerInstanceId?: string;
};

export type RequestStateResyncOptions = {
  allowAuthoritativeRollback?: boolean;
  /** Repair a partial projection at the current revision, without trusting a new server. */
  allowSameServerEqualRevision?: boolean;
  waitRepair?: WaitSnapshotRepairIntent;
  allowUnknownServerInstance?: boolean;
  preserveReconnectFallback?: boolean;
  preserveWatchdogCooldown?: boolean;
  rearmOnSuccess?: boolean;
  rearmUntilLiveEventOnSuccess?: boolean;
  rearmAfterSameInstanceProgressUntilLiveEvent?: boolean;
  confirmReconnectRecoveryOnAdoption?: boolean;
  forceAdoptEqualOrNewerRevision?: number;
  sseReconnectRequestId?: number;
  rearmOnFailure?: boolean;
  openSessionId?: string;
  paneId?: string | null;
};

export type PendingStateResyncOptions = {
  allowAuthoritativeRollback: boolean;
  allowSameServerEqualRevision: boolean;
  waitRepair: WaitSnapshotRepairIntent | null;
  allowUnknownServerInstance: boolean;
  preserveReconnectFallback: boolean;
  preserveWatchdogCooldown: boolean;
  rearmOnSuccess: boolean;
  rearmUntilLiveEventOnSuccess: boolean;
  rearmAfterSameInstanceProgressUntilLiveEvent: boolean;
  confirmReconnectRecoveryOnAdoption: boolean;
  forceAdoptEqualOrNewerRevision: number | null;
  sseReconnectRequestId: number | null;
  rearmOnFailure: boolean;
  openSessionId?: string;
  paneId?: string | null;
};

function createEmptyPendingStateResyncOptions(): PendingStateResyncOptions {
  return {
    allowAuthoritativeRollback: false,
    allowSameServerEqualRevision: false,
    waitRepair: null,
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
  };
}

export function coalescePendingStateResyncOptions(
  current: PendingStateResyncOptions | null,
  options: RequestStateResyncOptions | undefined,
): PendingStateResyncOptions {
  const next = current
    ? { ...current }
    : createEmptyPendingStateResyncOptions();
  next.allowAuthoritativeRollback =
    next.allowAuthoritativeRollback ||
    options?.allowAuthoritativeRollback === true;
  next.allowSameServerEqualRevision =
    next.allowSameServerEqualRevision ||
    options?.allowSameServerEqualRevision === true;
  if (options?.waitRepair) {
    const previous = next.waitRepair;
    const incoming = options.waitRepair;
    // A later adopted origin supersedes older evidence. Within one origin,
    // a current/untagged wait must not erase a pending replacement hint.
    next.waitRepair = previous?.observedServerInstanceId === incoming.observedServerInstanceId
      ? { ...incoming, replacementServerInstanceId: incoming.replacementServerInstanceId ?? previous.replacementServerInstanceId }
      : { ...incoming };
  }
  next.allowUnknownServerInstance =
    next.allowUnknownServerInstance ||
    options?.allowUnknownServerInstance === true;
  next.preserveReconnectFallback =
    next.preserveReconnectFallback ||
    options?.preserveReconnectFallback === true;
  next.preserveWatchdogCooldown =
    next.preserveWatchdogCooldown ||
    options?.preserveWatchdogCooldown === true;
  next.rearmOnSuccess =
    next.rearmOnSuccess || options?.rearmOnSuccess === true;
  next.rearmUntilLiveEventOnSuccess =
    next.rearmUntilLiveEventOnSuccess ||
    options?.rearmUntilLiveEventOnSuccess === true;
  next.rearmAfterSameInstanceProgressUntilLiveEvent =
    next.rearmAfterSameInstanceProgressUntilLiveEvent ||
    options?.rearmAfterSameInstanceProgressUntilLiveEvent === true;
  next.confirmReconnectRecoveryOnAdoption =
    next.confirmReconnectRecoveryOnAdoption ||
    options?.confirmReconnectRecoveryOnAdoption === true;
  if (
    typeof options?.forceAdoptEqualOrNewerRevision === "number" &&
    Number.isSafeInteger(options.forceAdoptEqualOrNewerRevision)
  ) {
    next.forceAdoptEqualOrNewerRevision =
      next.forceAdoptEqualOrNewerRevision === null
        ? options.forceAdoptEqualOrNewerRevision
        : Math.max(
            next.forceAdoptEqualOrNewerRevision,
            options.forceAdoptEqualOrNewerRevision,
          );
  }
  if (
    typeof options?.sseReconnectRequestId === "number" &&
    Number.isSafeInteger(options.sseReconnectRequestId)
  ) {
    next.sseReconnectRequestId =
      next.sseReconnectRequestId === null
        ? options.sseReconnectRequestId
        : Math.max(next.sseReconnectRequestId, options.sseReconnectRequestId);
  }
  next.rearmOnFailure =
    next.rearmOnFailure || options?.rearmOnFailure === true;
  if (options?.openSessionId !== undefined) {
    next.openSessionId = options.openSessionId;
    next.paneId = options.paneId ?? null;
  }
  return next;
}

export function consumePendingStateResyncOptions(
  pendingRef: MutableRefObject<PendingStateResyncOptions | null>,
): PendingStateResyncOptions {
  const pending =
    pendingRef.current ?? createEmptyPendingStateResyncOptions();
  pendingRef.current = null;
  return pending;
}
