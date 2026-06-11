const SESSION_FULL_HYDRATION_DEMAND_EVENT =
  "termal:session-full-hydration-demand";

// Tiny cross-tree bridge for transcript demand. Callers can request hydration
// before `useAppLiveState` has mounted its listener, so pending ids are replayed
// to late subscribers and deduped by session id.
const pendingFullHydrationSessionIds = new Set<string>();

export type SessionFullHydrationDemand = {
  sessionId: string;
};

export function requestSessionFullHydration(sessionId: string) {
  pendingFullHydrationSessionIds.add(sessionId);
  window.dispatchEvent(
    new CustomEvent<SessionFullHydrationDemand>(
      SESSION_FULL_HYDRATION_DEMAND_EVENT,
      {
        detail: { sessionId },
      },
    ),
  );
}

export function addSessionFullHydrationDemandListener(
  listener: (demand: SessionFullHydrationDemand) => void,
) {
  function emitToListener(sessionId: string) {
    pendingFullHydrationSessionIds.delete(sessionId);
    listener({ sessionId });
  }
  const handleDemand = (event: Event) => {
    const detail = (event as CustomEvent<SessionFullHydrationDemand>).detail;
    if (!detail?.sessionId) {
      return;
    }
    emitToListener(detail.sessionId);
  };
  window.addEventListener(SESSION_FULL_HYDRATION_DEMAND_EVENT, handleDemand);
  for (const sessionId of pendingFullHydrationSessionIds) {
    emitToListener(sessionId);
  }
  return () => {
    window.removeEventListener(
      SESSION_FULL_HYDRATION_DEMAND_EVENT,
      handleDemand,
    );
  };
}
