const SESSION_HISTORY_PAGE_DEMAND_EVENT = "termal:session-history-page-demand";

// Tiny cross-tree bridge for older-history demand. Callers can request a page
// before `useAppLiveState` has mounted its listener, so pending ids are replayed
// to late subscribers and deduped by session id.
const pendingHistoryPageDemands = new Map<string, SessionHistoryPageDemand>();
let nextHistoryDemandId = 1;
let nextHistoryDemandListenerId = 1;
const activeHistoryDemandListenerIds = new Set<number>();
const acceptedCompletionIdsByListener = new Map<number, Set<number>>();
const historyDemandCompletions = new Map<
  number,
  (applied: boolean) => void
>();

export type SessionHistoryPageDemand = {
  sessionId: string;
  direction: "older" | "start" | "newer" | "tail" | "around";
  position?: number;
  requestId?: number;
};

export function resolveHasOlderSessionHistory({
  hasOlderHistory,
  messageCount,
  messagesLoaded,
  residentMessageCount,
}: {
  hasOlderHistory?: boolean;
  messageCount?: number | null;
  messagesLoaded?: boolean | null;
  residentMessageCount: number;
}) {
  // The summary count remains authoritative while the resident transcript is a
  // bounded suffix; messagesLoaded=false is the legacy explicit window signal.
  return (
    hasOlderHistory ??
    (messagesLoaded === false ||
      (messagesLoaded !== true &&
        (messageCount ?? 0) > residentMessageCount))
  );
}

export function requestSessionHistoryPage(sessionId: string) {
  dispatchSessionHistoryDemand({ sessionId, direction: "older" });
}

export function requestSessionHistoryOlderPage(sessionId: string) {
  return requestCompletableSessionHistoryPage(sessionId, "older");
}

export function requestSessionHistoryStartPage(sessionId: string) {
  return requestCompletableSessionHistoryPage(sessionId, "start");
}

export function requestSessionHistoryNewerPage(sessionId: string) {
  return requestCompletableSessionHistoryPage(sessionId, "newer");
}

export function requestSessionHistoryTailPage(sessionId: string) {
  return requestCompletableSessionHistoryPage(sessionId, "tail");
}

export function requestSessionHistoryAroundPage(
  sessionId: string,
  position: number,
) {
  return requestCompletableSessionHistoryPage(
    sessionId,
    "around",
    Math.max(0, Math.floor(position)),
  );
}

function requestCompletableSessionHistoryPage(
  sessionId: string,
  direction: "older" | "start" | "newer" | "tail" | "around",
  position?: number,
) {
  // Completable navigation is an immediate request/response contract. Unlike
  // passive older-page prefetch demand, it must not create a promise that can
  // live forever when the app-state owner is not mounted.
  if (activeHistoryDemandListenerIds.size === 0) {
    return Promise.resolve(false);
  }
  const requestId = nextHistoryDemandId;
  nextHistoryDemandId += 1;
  return new Promise<boolean>((resolve) => {
    historyDemandCompletions.set(requestId, resolve);
    dispatchSessionHistoryDemand({
      sessionId,
      direction,
      position,
      requestId,
    });
  });
}

function demandKey(demand: SessionHistoryPageDemand) {
  return `${demand.sessionId}:${demand.direction}:${demand.position ?? ""}`;
}

function dispatchSessionHistoryDemand(demand: SessionHistoryPageDemand) {
  const key = demandKey(demand);
  const superseded = pendingHistoryPageDemands.get(key);
  if (
    superseded?.requestId !== undefined &&
    superseded.requestId !== demand.requestId
  ) {
    completeSessionHistoryPageDemand(superseded.requestId, false);
  }
  pendingHistoryPageDemands.set(key, demand);
  window.dispatchEvent(
    new CustomEvent<SessionHistoryPageDemand>(
      SESSION_HISTORY_PAGE_DEMAND_EVENT,
      {
        detail: demand,
      },
    ),
  );
}

export function completeSessionHistoryPageDemand(
  requestId: number | undefined,
  applied: boolean,
) {
  if (requestId === undefined) {
    return;
  }
  const complete = historyDemandCompletions.get(requestId);
  historyDemandCompletions.delete(requestId);
  for (const acceptedIds of acceptedCompletionIdsByListener.values()) {
    acceptedIds.delete(requestId);
  }
  complete?.(applied);
}

export function addSessionHistoryPageDemandListener(
  listener: (demand: SessionHistoryPageDemand) => void,
) {
  const listenerId = nextHistoryDemandListenerId;
  nextHistoryDemandListenerId += 1;
  activeHistoryDemandListenerIds.add(listenerId);
  const acceptedCompletionIds = new Set<number>();
  acceptedCompletionIdsByListener.set(listenerId, acceptedCompletionIds);
  function emitToListener(demand: SessionHistoryPageDemand) {
    pendingHistoryPageDemands.delete(demandKey(demand));
    if (demand.requestId !== undefined) {
      acceptedCompletionIds.add(demand.requestId);
    }
    listener(demand);
  }
  const handleDemand = (event: Event) => {
    const detail = (event as CustomEvent<SessionHistoryPageDemand>).detail;
    if (!detail?.sessionId || !detail.direction) {
      return;
    }
    emitToListener(detail);
  };
  window.addEventListener(SESSION_HISTORY_PAGE_DEMAND_EVENT, handleDemand);
  // Copy before emitting because emitToListener removes each entry.
  for (const demand of [...pendingHistoryPageDemands.values()]) {
    emitToListener(demand);
  }
  return () => {
    window.removeEventListener(SESSION_HISTORY_PAGE_DEMAND_EVENT, handleDemand);
    activeHistoryDemandListenerIds.delete(listenerId);
    acceptedCompletionIdsByListener.delete(listenerId);
    for (const requestId of acceptedCompletionIds) {
      const acceptedElsewhere = [...acceptedCompletionIdsByListener.values()].some(
        (acceptedIds) => acceptedIds.has(requestId),
      );
      if (!acceptedElsewhere) {
        completeSessionHistoryPageDemand(requestId, false);
      }
    }
  };
}
