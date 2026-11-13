// Worker-owned SSE: one upstream per origin, ordered fan-out, no cached state.
// Late subscribers request a fresh HTTP snapshot through the app's revision
// guards. Only this hub decides whether its upstream is stale enough to replace;
// each tab's transport separately judges its own client of the hub.
// Keep this allowlist in parity with src/api_sse.rs (covered by a wire test).
export const LIVE_EVENT_TYPES = [
  "state",
  "stateDiff",
  "delta",
  "lagged",
  "workspaceFilesChanged",
  // Sent every 15 s by the server; carries no state. It lets a client tell a
  // quiet stream from one that silently stopped delivering.
  "heartbeat",
] as const;

// A stream that has proved it sends heartbeats and then delivers nothing for
// this long (three missed beats) is replaced. A stream from a server that
// never sent a heartbeat (an older backend) is never judged silent.
export const LIVE_STREAM_SILENCE_RESTART_MS = 45_000;

// Every browser connection opts into `stateDiff`; other `/api/events`
// clients (the remote hub) keep the full-`state` stream.
export const LIVE_EVENTS_URL = "/api/events?stateDiff=1";

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
  createSource: () => EventSource = () => new EventSource(LIVE_EVENTS_URL),
) {
  const ports = new Map<MessagePort, { active: boolean; lastSeen: number }>();
  let source: EventSource | null = null;
  let connectingSince: number | null = null;
  let heartbeat: ReturnType<typeof setInterval> | null = null;
  // Upstream liveness for the current source. An OPEN EventSource behind a
  // proxy can stay OPEN with nothing arriving; only data tells them apart.
  let lastUpstreamEventAt = Date.now();
  let upstreamSendsHeartbeats = false;

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
    lastUpstreamEventAt = Date.now();
    upstreamSendsHeartbeats = false;
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
      lastUpstreamEventAt = Date.now();
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
        lastUpstreamEventAt = Date.now();
        if (eventType === "heartbeat") upstreamSendsHeartbeats = true;
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
        if (
          source?.readyState === 1 &&
          upstreamSendsHeartbeats &&
          Date.now() - lastUpstreamEventAt >= LIVE_STREAM_SILENCE_RESTART_MS
        ) {
          // OPEN but silent past three missed heartbeats: the upstream is
          // gone even though the socket says otherwise. Replace it; joined
          // tabs see the reconnect and resync through their normal rules.
          startSource();
        }
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
