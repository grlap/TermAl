import { describe, expect, it } from "vitest";

import { ReconnectStateMachine } from "./app-live-state-reconnect-state";

describe("ReconnectStateMachine", () => {
  it("requires explicit open proof before ordinary delta recovery", () => {
    const state = new ReconnectStateMachine();

    state.onSseError();

    expect(state.confirmDeltaEvent()).toBe(false);
    expect(state.recoveryConfirmedSinceLastError).toBe(false);
    expect(state.confirmDeltaEvent({ eventSourceReadyStateIsOpen: true })).toBe(
      true,
    );
    expect(state.recoveryConfirmedSinceLastError).toBe(true);
  });

  it("allows state events to prove recovery after an error without onopen", () => {
    const state = new ReconnectStateMachine();

    state.onSseError();

    expect(state.confirmStateEvent()).toBe(true);
    expect(state.recoveryConfirmedSinceLastError).toBe(true);
  });

  it("carries manual retry same-instance progress until a later live event", () => {
    const state = new ReconnectStateMachine();

    state.onManualRetry();
    expect(state.confirmDeltaEvent()).toBe(false);

    state.onManualRetrySameInstanceProgress();

    expect(state.confirmDeltaEvent()).toBe(true);
  });

  it("clears bad-event and delegation repair proof after confirmed recovery", () => {
    const state = new ReconnectStateMachine();

    state.onSseOpen();
    state.onBadLiveEvent();
    state.setLastDelegationRepairRequestedRevision(3);
    expect(state.markDelegationRepairAdoptedIfCoversRevision(3, 3)).toBe(true);

    expect(state.pendingBadLiveEventRecovery).toBe(true);
    expect(state.delegationRepairAdoptedSinceLastReconnectError).toBe(true);

    expect(state.confirmDeltaEvent({ eventSourceReadyStateIsOpen: true })).toBe(
      true,
    );

    expect(state.pendingBadLiveEventRecovery).toBe(false);
    expect(state.delegationRepairAdoptedSinceLastReconnectError).toBe(false);
  });

  it("treats authoritative snapshot adoption as confirmed recovery", () => {
    const state = new ReconnectStateMachine();

    state.onBadLiveEvent();
    state.setLastDelegationRepairRequestedRevision(7);
    expect(state.markDelegationRepairAdoptedIfCoversRevision(7, 7)).toBe(true);

    state.confirmAuthoritativeSnapshot();

    expect(state.recoveryConfirmedSinceLastError).toBe(true);
    expect(state.pendingBadLiveEventRecovery).toBe(false);
    expect(state.delegationRepairAdoptedSinceLastReconnectError).toBe(false);
  });

  it("reports whether an error starts a fresh failure cycle", () => {
    const state = new ReconnectStateMachine();

    expect(state.onSseError().hadReconnectOpenSinceLastError).toBe(false);

    state.onSseOpen();

    expect(state.onSseError().hadReconnectOpenSinceLastError).toBe(true);
    expect(state.sawReconnectOpenSinceLastError).toBe(false);
    expect(state.recoveryConfirmedSinceLastError).toBe(false);
  });
});
