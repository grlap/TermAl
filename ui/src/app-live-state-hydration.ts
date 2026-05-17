// Owns: small pure helpers/constants for live-state session hydration.
// Does not own: hydration fetch effects, adoption side effects, or retry timers.
// Split from: ui/src/app-live-state.ts.

import type {
  AdoptSessionsOptions,
  AdoptStateOptions,
} from "./app-live-state-types";
import type { AdoptFetchedSessionOutcome } from "./session-hydration-adoption";

type FullFetchAdoptFetchedSessionOutcome = Exclude<
  AdoptFetchedSessionOutcome,
  "partial"
>;

export function fullFetchAdoptFetchedSessionOutcome(
  outcome: AdoptFetchedSessionOutcome,
): FullFetchAdoptFetchedSessionOutcome {
  if (outcome === "partial") {
    console.warn(
      "session hydration> full fetch unexpectedly produced a partial transcript adoption; retrying full hydration",
    );
    return "stale";
  }
  return outcome;
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
    forceMessagesUnloaded: serverInstanceChanged,
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
