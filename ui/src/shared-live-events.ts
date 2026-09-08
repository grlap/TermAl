// Worker-owned SSE: one upstream per origin, ordered fan-out, no cached state.
// Late subscribers request a fresh HTTP snapshot through the app's revision
// guards. Only this hub decides whether an upstream is stale enough to replace.
// Keep this allowlist in parity with src/api_sse.rs (covered by a wire test).
export const LIVE_EVENT_TYPES = [
  "state",
  "delta",
  "lagged",
  "workspaceFilesChanged",
] as const;

export type SharedLiveEvent =
  | { type: "open" | "error" | "status"; readyState: number }
  | { type: "ping" }
  | { type: "probeAck"; probeId: number }
  | { type: "snapshotRequired" }
  | { type: "event"; eventType: string; data: string };

export type ClientLiveEvent =
  | { type: "start"; reconnect: boolean }
  | { type: "pong" | "close" }
  | { type: "probe"; probeId: number };

// MessagePort.postMessage accepts any; type the producer as well as the receiver.
export function sendClientLiveEvent(port: MessagePort, message: ClientLiveEvent) {
  port.postMessage(message);
}

export const SHARED_LIVE_HEARTBEAT_MS = 15_000;
export const SHARED_LIVE_LEASE_MS = 90_000;
export const SHARED_LIVE_CONNECTING_STALE_MS = 10_000;

export function createSharedLiveEventHub(
  createSource: () => EventSource = () => new EventSource("/api/events"),
) {
  const ports = new Map<MessagePort, { active: boolean; lastSeen: number }>();
  let source: EventSource | null = null;
  let connectingSince: number | null = null;
  let heartbeat: ReturnType<typeof setInterval> | null = null;

  function disconnect(port: MessagePort) {
    ports.delete(port);
    port.onmessage = null;
    port.onmessageerror = null;
    port.close();
    if (ports.size === 0) {
      const previous = source;
      source = null;
      previous?.close();
      if (heartbeat !== null) clearInterval(heartbeat);
      heartbeat = null;
    }
  }

  function send(port: MessagePort, message: SharedLiveEvent) {
    try {
      port.postMessage(message);
    } catch {
      disconnect(port);
    }
  }

  function broadcast(message: SharedLiveEvent) {
    for (const [port, peer] of ports) {
      if (peer.active) send(port, message);
    }
  }

  function startSource() {
    const previous = source;
    source = null;
    previous?.close();
    // Other subscribers must observe a reconnect before the replacement's
    // initial snapshot, so their existing revision recovery rules apply.
    if (previous) broadcast({ type: "error", readyState: 0 });
    connectingSince = Date.now();
    let next: EventSource;
    try {
      next = createSource();
    } catch {
      broadcast({ type: "error", readyState: 2 });
      return;
    }
    source = next;
    next.onopen = () => {
      if (source !== next) return;
      connectingSince = null;
      broadcast({ type: "open", readyState: 1 });
    };
    next.onerror = () => {
      if (source === next) {
        connectingSince ??= Date.now();
        broadcast({ type: "error", readyState: next.readyState });
      }
    };
    for (const eventType of LIVE_EVENT_TYPES) {
      next.addEventListener(eventType, (event) => {
        if (source !== next) return;
        broadcast({
          type: "event",
          eventType,
          data: (event as MessageEvent<string>).data,
        });
      });
    }
  }

  return {
    connect(port: MessagePort) {
      ports.set(port, { active: false, lastSeen: Date.now() });
      // postMessage to a dead peer can silently succeed. Require acknowledgments
      // and bound abandoned subscriptions even when pagehide never ran.
      heartbeat ??= setInterval(() => {
        for (const [peerPort, peer] of ports) {
          if (Date.now() - peer.lastSeen >= SHARED_LIVE_LEASE_MS) {
            disconnect(peerPort);
          } else {
            send(peerPort, { type: "ping" });
          }
        }
      }, SHARED_LIVE_HEARTBEAT_MS);
      port.onmessage = (event: MessageEvent<ClientLiveEvent>) => {
        const peer = ports.get(port);
        if (!peer) return;
        if (event.data?.type === "close") {
          disconnect(port);
          return;
        }
        peer.lastSeen = Date.now();
        if (event.data?.type === "probe" && peer.active && Number.isSafeInteger(event.data.probeId)) {
          // Reply on the event channel itself so already posted events precede
          // the acknowledgment. An expired/disconnected port cannot answer.
          send(port, { type: "probeAck", probeId: event.data.probeId });
          return;
        }
        if (event.data?.type !== "start" || peer.active) return;
        peer.active = true;
        if (!source || source.readyState === 2 || (
          event.data.reconnect === true && source.readyState === 0 &&
          connectingSince !== null &&
          Date.now() - connectingSince >= SHARED_LIVE_CONNECTING_STALE_MS
        )) {
          startSource();
        }
        if (source?.readyState === 1) {
          send(port, { type: "open", readyState: 1 });
          // The existing SSE stream already sent its initial snapshot. The new
          // tab obtains a fresh snapshot through its normal HTTP recovery path;
          // replaying a cached summary would lose intervening transcript deltas.
          send(port, { type: "snapshotRequired" });
        } else {
          // Joining a fresh CONNECTING attempt never restarts it. Report its
          // state immediately so the client can distinguish it from worker failure.
          send(port, { type: "status", readyState: source?.readyState ?? 2 });
        }
      };
      port.onmessageerror = () => {
        send(port, { type: "error", readyState: 2 });
        disconnect(port);
      };
      port.start();
    },
  };
}
