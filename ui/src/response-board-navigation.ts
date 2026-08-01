export type ResponseBoardSourceNavigation = {
  sessionId: string;
  messageId: string;
  messagePosition: number;
};

type NavigationListener = (request: ResponseBoardSourceNavigation) => void;

export const RESPONSE_BOARD_SOURCE_NAVIGATION_TTL_MS = 30_000;

type PendingNavigation = {
  request: ResponseBoardSourceNavigation;
  deliveredTo: Set<object>;
  expiresAt: number;
  expiryTimer: ReturnType<typeof setTimeout>;
};

const pendingBySessionId = new Map<string, PendingNavigation>();
const listenersBySessionId = new Map<
  string,
  Map<object, NavigationListener>
>();

export function requestResponseBoardSourceNavigation(
  request: ResponseBoardSourceNavigation,
) {
  const previous = pendingBySessionId.get(request.sessionId);
  if (previous) {
    clearTimeout(previous.expiryTimer);
  }
  const pending: PendingNavigation = {
    request,
    deliveredTo: new Set(),
    expiresAt: Date.now() + RESPONSE_BOARD_SOURCE_NAVIGATION_TTL_MS,
    expiryTimer: setTimeout(() => {
      if (pendingBySessionId.get(request.sessionId) === pending) {
        pendingBySessionId.delete(request.sessionId);
      }
    }, RESPONSE_BOARD_SOURCE_NAVIGATION_TTL_MS),
  };
  pendingBySessionId.set(request.sessionId, pending);
  deliverPendingResponseBoardNavigation(request.sessionId);
}

export function subscribeResponseBoardSourceNavigation(
  sessionId: string,
  listener: NavigationListener,
  subscriberKey: object = listener,
) {
  const listeners = listenersBySessionId.get(sessionId) ?? new Map();
  listeners.set(subscriberKey, listener);
  listenersBySessionId.set(sessionId, listeners);
  queueMicrotask(() => deliverPendingResponseBoardNavigation(sessionId));
  return () => {
    if (listeners.get(subscriberKey) === listener) {
      listeners.delete(subscriberKey);
    }
    if (listeners.size === 0) {
      listenersBySessionId.delete(sessionId);
    }
  };
}

function deliverPendingResponseBoardNavigation(sessionId: string) {
  const pending = pendingBySessionId.get(sessionId);
  if (!pending) {
    return;
  }
  if (pending.expiresAt <= Date.now()) {
    clearTimeout(pending.expiryTimer);
    pendingBySessionId.delete(sessionId);
    return;
  }
  for (const [subscriberKey, listener] of
    listenersBySessionId.get(sessionId) ?? []) {
    if (pending.deliveredTo.has(subscriberKey)) {
      continue;
    }
    pending.deliveredTo.add(subscriberKey);
    listener(pending.request);
  }
}
