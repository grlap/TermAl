export type ResponseBoardSourceNavigation = {
  sessionId: string;
  messageId: string;
  messagePosition: number;
};

type NavigationListener = (request: ResponseBoardSourceNavigation) => void;

const pendingBySessionId = new Map<string, ResponseBoardSourceNavigation>();
const listenersBySessionId = new Map<string, Set<NavigationListener>>();

export function requestResponseBoardSourceNavigation(
  request: ResponseBoardSourceNavigation,
) {
  pendingBySessionId.set(request.sessionId, request);
  deliverPendingResponseBoardNavigation(request.sessionId);
}

export function subscribeResponseBoardSourceNavigation(
  sessionId: string,
  listener: NavigationListener,
) {
  const listeners = listenersBySessionId.get(sessionId) ?? new Set();
  listeners.add(listener);
  listenersBySessionId.set(sessionId, listeners);
  queueMicrotask(() => deliverPendingResponseBoardNavigation(sessionId));
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      listenersBySessionId.delete(sessionId);
    }
  };
}

function deliverPendingResponseBoardNavigation(sessionId: string) {
  const request = pendingBySessionId.get(sessionId);
  const listener = listenersBySessionId.get(sessionId)?.values().next().value;
  if (!request || !listener) {
    return;
  }
  pendingBySessionId.delete(sessionId);
  listener(request);
}
