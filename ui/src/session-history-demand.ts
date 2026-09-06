const SESSION_HISTORY_PAGE_DEMAND_EVENT = "termal:session-history-page-demand";

// Tiny cross-tree bridge for older-history demand. Callers can request a page
// before `useAppLiveState` has mounted its listener, so pending ids are replayed
// to late subscribers and deduped by session id.
const pendingHistoryPageDemands = new Map<string, SessionHistoryPageDemand>();
let nextHistoryDemandId = 1;
let nextHistoryDemandListenerId = 1;
const activeHistoryDemandListenerIds = new Set<number>();
const acceptedCompletionIdsByListener = new Map<number, Set<number>>();
// Explicit tail navigation may survive a rejected page, but only two fresh
// snapshot adoptions may restart it. No render or elapsed-time retry loop.
const MAX_TAIL_RECOVERY_RETRIES = 2;
const historyDemandCompletions = new Map<
  number,
  {
    resolve: (applied: boolean) => void;
    cleanup: () => void;
    recovery?: {
      demand: SessionHistoryPageDemand;
      retriesRemaining: number;
      waitingForState: boolean;
    };
  }
>();

export type SessionHistoryPageDemand = {
  sessionId: string;
  direction: "older" | "start" | "newer" | "tail" | "around";
  position?: number;
  requestId?: number;
  signal?: AbortSignal;
};

type TailHistoryDemandOptions = {
  signal?: AbortSignal;
  retryOnRecovery?: boolean;
};

export function resolveHasOlderSessionHistory({
  hasOlderHistory,
}: {
  hasOlderHistory?: boolean;
}) {
  // Only explicit window metadata authorizes pagination. An unhydrated or
  // malformed shape cannot prove that an older page exists.
  return hasOlderHistory === true;
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

export function requestSessionHistoryTailPage(
  sessionId: string,
  options?: TailHistoryDemandOptions,
) {
  return requestCompletableSessionHistoryPage(
    sessionId,
    "tail",
    undefined,
    options,
  );
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
  options?: TailHistoryDemandOptions,
) {
  // Completable navigation is an immediate request/response contract. Unlike
  // passive older-page prefetch demand, it must not create a promise that can
  // live forever when the app-state owner is not mounted.
  if (activeHistoryDemandListenerIds.size === 0 || options?.signal?.aborted) {
    return Promise.resolve(false);
  }
  const requestId = nextHistoryDemandId;
  nextHistoryDemandId += 1;
  return new Promise<boolean>((resolve) => {
    const demand: SessionHistoryPageDemand = {
      sessionId,
      direction,
      position,
      requestId,
      ...(options?.signal ? { signal: options.signal } : {}),
    };
    const abort = () => completeSessionHistoryPageDemand(requestId, false);
    historyDemandCompletions.set(requestId, {
      resolve,
      cleanup: () => options?.signal?.removeEventListener("abort", abort),
      recovery:
        direction === "tail" && options?.retryOnRecovery
          ? {
              demand,
              retriesRemaining: MAX_TAIL_RECOVERY_RETRIES,
              waitingForState: false,
            }
          : undefined,
    });
    options?.signal?.addEventListener("abort", abort, { once: true });
    dispatchSessionHistoryDemand(demand);
  });
}

// The loader calls this only for classified instance/metadata rejection, and
// BEFORE requesting recovery: a synchronous adoption must not miss the waiter.
export function deferSessionHistoryTailDemandUntilStateAdoption(
  demand: SessionHistoryPageDemand,
) {
  const recovery =
    demand.requestId === undefined
      ? undefined
      : historyDemandCompletions.get(demand.requestId)?.recovery;
  if (!recovery || demand.signal?.aborted) {
    return false;
  }
  if (recovery.waitingForState) {
    return true;
  }
  if (recovery.retriesRemaining === 0) {
    return false;
  }
  recovery.retriesRemaining -= 1;
  recovery.waitingForState = true;
  return true;
}

export function resumeSessionHistoryDemandsAfterStateAdoption() {
  // A successful full-state adoption publishes session/instance refs before
  // notifying us. Rejected snapshots and ordinary renders never reach here.
  const waiting = [...historyDemandCompletions.values()].flatMap(
    ({ recovery }) => recovery?.waitingForState ? [recovery] : [],
  );
  for (const recovery of waiting) {
    const { demand } = recovery;
    if (
      demand.signal?.aborted ||
      demand.requestId === undefined ||
      !historyDemandCompletions.has(demand.requestId)
    ) {
      continue;
    }
    recovery.waitingForState = false;
    dispatchSessionHistoryDemand(demand);
  }
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
  const completion = historyDemandCompletions.get(requestId);
  historyDemandCompletions.delete(requestId);
  completion?.cleanup();
  for (const [key, demand] of pendingHistoryPageDemands) {
    if (demand.requestId === requestId) {
      pendingHistoryPageDemands.delete(key);
    }
  }
  for (const acceptedIds of acceptedCompletionIdsByListener.values()) {
    acceptedIds.delete(requestId);
  }
  completion?.resolve(applied);
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
