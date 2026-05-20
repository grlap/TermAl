// app-live-state-reconnect-state.ts
//
// Owns the reconnect proof flags used by app-live-state-transport.
// Timer scheduling, EventSource construction, and UI state setters stay in
// app-live-state-transport.ts so this module remains pure and testable.

type ConfirmLiveEventOptions = {
  allowWithoutConfirmedOpen?: boolean;
};

type ConfirmSseEventOptions = {
  eventSourceReadyStateIsOpen?: boolean;
};

export type ReconnectRecoveryStateSnapshot = {
  readonly delegationRepairAdoptedSinceLastReconnectError: boolean;
  readonly pendingBadLiveEventRecovery: boolean;
  readonly sawReconnectOpenSinceLastError: boolean;
};

export class ReconnectStateMachine
  implements ReconnectRecoveryStateSnapshot
{
  private allowReconnectRecoveryWithoutExplicitOpen = false;
  private lastDelegationRepairRequestedRevision: number | null = null;
  private reconnectErrorPendingLiveProof = false;
  private reconnectRecoveryConfirmedSinceLastError = false;

  delegationRepairAdoptedSinceLastReconnectError = false;
  pendingBadLiveEventRecovery = false;
  sawReconnectOpenSinceLastError = false;

  get recoveryConfirmedSinceLastError() {
    return this.reconnectRecoveryConfirmedSinceLastError;
  }

  onSseOpen() {
    this.sawReconnectOpenSinceLastError = true;
  }

  onSseError() {
    const hadReconnectOpenSinceLastError =
      this.sawReconnectOpenSinceLastError;
    this.sawReconnectOpenSinceLastError = false;
    this.reconnectRecoveryConfirmedSinceLastError = false;
    this.pendingBadLiveEventRecovery = false;
    this.allowReconnectRecoveryWithoutExplicitOpen = false;
    // State events carry complete snapshots, so a state event delivered while
    // EventSource reports OPEN can prove recovery even when the browser missed
    // `onopen`. Delta events need either explicit OPEN state or a narrower
    // manual-retry contract because buffered deltas can predate the error.
    this.reconnectErrorPendingLiveProof = true;
    this.clearDelegationRepairProof();
    return { hadReconnectOpenSinceLastError };
  }

  onManualRetry() {
    this.sawReconnectOpenSinceLastError = false;
    this.reconnectRecoveryConfirmedSinceLastError = false;
    this.pendingBadLiveEventRecovery = false;
    this.allowReconnectRecoveryWithoutExplicitOpen = false;
    this.reconnectErrorPendingLiveProof = false;
    this.clearDelegationRepairProof();
  }

  onManualRetrySameInstanceProgress() {
    this.allowReconnectRecoveryWithoutExplicitOpen = true;
  }

  onBadLiveEvent() {
    this.pendingBadLiveEventRecovery = true;
    this.reconnectRecoveryConfirmedSinceLastError = false;
    this.allowReconnectRecoveryWithoutExplicitOpen = false;
    this.clearDelegationRepairProof();
  }

  markRecoveryConfirmedAfterReopen({
    allowWithoutConfirmedOpen = false,
  }: ConfirmLiveEventOptions = {}) {
    if (
      !this.sawReconnectOpenSinceLastError &&
      !allowWithoutConfirmedOpen
    ) {
      return false;
    }

    this.reconnectRecoveryConfirmedSinceLastError = true;
    return true;
  }

  confirmLiveEvent(options?: ConfirmLiveEventOptions) {
    if (!this.markRecoveryConfirmedAfterReopen(options)) {
      return false;
    }

    this.pendingBadLiveEventRecovery = false;
    this.allowReconnectRecoveryWithoutExplicitOpen = false;
    this.reconnectErrorPendingLiveProof = false;
    this.clearDelegationRepairProof();
    return true;
  }

  confirmDeltaEvent({
    eventSourceReadyStateIsOpen = false,
  }: ConfirmSseEventOptions = {}) {
    return this.confirmLiveEvent({
      allowWithoutConfirmedOpen:
        this.allowReconnectRecoveryWithoutExplicitOpen ||
        eventSourceReadyStateIsOpen,
    });
  }

  confirmStateEvent({
    eventSourceReadyStateIsOpen = false,
  }: ConfirmSseEventOptions = {}) {
    return this.confirmLiveEvent({
      allowWithoutConfirmedOpen:
        this.allowReconnectRecoveryWithoutExplicitOpen ||
        this.reconnectErrorPendingLiveProof ||
        eventSourceReadyStateIsOpen,
    });
  }

  confirmAuthoritativeSnapshot() {
    this.reconnectRecoveryConfirmedSinceLastError = true;
    this.pendingBadLiveEventRecovery = false;
    this.allowReconnectRecoveryWithoutExplicitOpen = false;
    this.reconnectErrorPendingLiveProof = false;
    this.clearDelegationRepairProof();
  }

  setLastDelegationRepairRequestedRevision(revision: number) {
    this.lastDelegationRepairRequestedRevision = revision;
  }

  markDelegationRepairAdoptedIfCoversRevision(
    stateRevision: number,
    forceAdoptEqualOrNewerRevision: number | null,
  ) {
    if (
      this.lastDelegationRepairRequestedRevision === null ||
      forceAdoptEqualOrNewerRevision === null ||
      stateRevision < this.lastDelegationRepairRequestedRevision
    ) {
      return false;
    }

    this.delegationRepairAdoptedSinceLastReconnectError = true;
    return true;
  }

  clearDelegationRepairProof() {
    this.delegationRepairAdoptedSinceLastReconnectError = false;
    this.lastDelegationRepairRequestedRevision = null;
  }
}
