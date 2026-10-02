// Owns: small helpers/constants and release of the existing hydration flight.
// Does not own: hydration fetch effects, adoption side effects, or retry timers.
// Split from: ui/src/app-live-state.ts.

import type {
  AdoptSessionsOptions,
  AdoptStateOptions,
} from "./app-live-state-types";
export type SessionHydrationOptions = {
  allowDivergentTextRepairAfterNewerRevision?: boolean;
  forceTailRepair?: boolean;
  ownerTailRead?: boolean;
  queueAfterCurrent?: boolean;
};

// Shared by the hook and its deterministic delivery model. No second flight
// registry or proof token is introduced; every terminal path frees this slot.
export function releaseSessionHydrationFlight(active: Set<string>, sessionId: string) {
  active.delete(sessionId);
}

// The task-end request decision is shared with the delivery model. Keep demand
// lazy so blocked requests do not evaluate it, just as in the hook's guard.
export function shouldRequestSessionTailRead(options: {
  mounted: boolean;
  inFlight: boolean;
  retryPending: boolean;
  needsTailRead: () => boolean;
}): boolean {
  return options.mounted && !options.inFlight && !options.retryPending && options.needsTailRead();
}

export function resolveAdoptStateSessionOptions(
  options: AdoptStateOptions | undefined,
  serverInstanceChanged: boolean,
): AdoptSessionsOptions {
  return {
    ...options,
    disableMutationStampFastPath:
      serverInstanceChanged || options?.disableMutationStampFastPath === true,
    // A server-instance change is the canonical "we just observed a
    // backend restart" signal. Persisted sessions arrive with a cleared
    // `sessionMutationStamp`, so the summary reconcile cannot otherwise
    // tell whether the local transcript matches the server's
    // authoritative content. Forcing `messagesLoaded: false` re-arms the
    // visible-session hydration effect so /api/sessions/{id} repaints
    // the active pane instead of leaving stale streaming content
    // visible until the user hard-refreshes.
    forceMessagesUnloaded:
      serverInstanceChanged || options?.forceMessagesUnloaded === true,
    // Delegated child sessions are durable for result inspection, but restored
    // workspace layouts should not reopen historical reviewer panes after a
    // browser or backend restart. They remain openable explicitly from the
    // delegation card in the current UI session.
    pruneDelegatedChildWorkspaceTabs: serverInstanceChanged,
  };
}

/** First retry after a metadata-only hydration response. Exported for tests. */
export const SESSION_HYDRATION_FIRST_RETRY_DELAY_MS = 50;
export const SESSION_HYDRATION_RETRY_DELAYS_MS = [
  SESSION_HYDRATION_FIRST_RETRY_DELAY_MS,
  250,
  1000,
  3000,
] as const;
export const SESSION_HYDRATION_MAX_RETRY_ATTEMPTS =
  SESSION_HYDRATION_RETRY_DELAYS_MS.length;
