// Owns live-state delegation-wait record equality and delta list updates.
// Does not own delegation session state, fan-in execution, or UI rendering.
// Split from app-live-state.ts to keep the hook's main state machine smaller.
import type { DelegationWaitRecord } from "./api";

export function areDelegationWaitRecordsEqual(
  current: readonly DelegationWaitRecord[],
  next: readonly DelegationWaitRecord[],
) {
  if (current.length !== next.length) {
    return false;
  }
  return current.every((record, index) => {
    const candidate = next[index];
    return (
      candidate !== undefined &&
      record.id === candidate.id &&
      record.parentSessionId === candidate.parentSessionId &&
      record.mode === candidate.mode &&
      record.createdAt === candidate.createdAt &&
      (record.title ?? null) === (candidate.title ?? null) &&
      record.delegationIds.length === candidate.delegationIds.length &&
      record.delegationIds.every((id, idIndex) => id === candidate.delegationIds[idIndex])
    );
  });
}

export function applyDelegationWaitCreated(
  waits: readonly DelegationWaitRecord[],
  wait: DelegationWaitRecord,
): DelegationWaitRecord[] {
  const index = waits.findIndex((record) => record.id === wait.id);
  if (index === -1) {
    return [...waits, wait];
  }

  const nextWaits = [...waits];
  nextWaits[index] = wait;
  return areDelegationWaitRecordsEqual(waits, nextWaits) ? [...waits] : nextWaits;
}

export function applyDelegationWaitConsumed(
  waits: readonly DelegationWaitRecord[],
  waitId: string,
): DelegationWaitRecord[] {
  if (!waits.some((wait) => wait.id === waitId)) {
    return [...waits];
  }

  return waits.filter((wait) => wait.id !== waitId);
}
