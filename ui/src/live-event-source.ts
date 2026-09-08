// EventTarget facade for the app's four named SSE events, readyState and
// onopen/onerror. A failed worker/channel falls back on this same object so
// listeners survive. bfcache releases its port and resubscribes on pageshow.
import {
  LIVE_EVENT_TYPES,
  SHARED_LIVE_HEARTBEAT_MS,
  SHARED_LIVE_LEASE_MS,
  sendClientLiveEvent,
  type SharedLiveEvent,
} from "./shared-live-events";

const WORKER_START_TIMEOUT_MS = 8_000;

function createWorker() {
  return new SharedWorker(new URL("./live-events.worker.ts", import.meta.url), {
    type: "module",
    name: "termal-live-events-v2",
  });
}

export class SharedLiveEventSource extends EventTarget {
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private closed = false;
  private suspended = false;
  private worker: SharedWorker | null = null;
  private native: EventSource | null = null;
  private lastWorkerMessageAt = Date.now();
  private startupTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private probeTimer: ReturnType<typeof setTimeout> | null = null;
  private nextProbeId = 0;
  private pendingProbeId: number | null = null;

  constructor(worker: SharedWorker, reconnect: boolean, private readonly makeWorker = createWorker) {
    super();
    this.attachWorker(worker, reconnect);
    window.addEventListener("pagehide", this.handlePageHide);
    window.addEventListener("pageshow", this.handlePageShow);
    document.addEventListener("visibilitychange", this.handleVisibilityChange);
  }

  private emitStatus(type: "open" | "error", readyState: number) {
    this.readyState = readyState;
    const event = new Event(type);
    if (type === "open") this.onopen?.(event);
    else this.onerror?.(event);
    this.dispatchEvent(event);
  }

  private attachWorker(worker: SharedWorker, reconnect: boolean) {
    this.worker = worker;
    this.lastWorkerMessageAt = Date.now();
    this.readyState = 0;
    this.startupTimer = setTimeout(() => this.fallbackToNative(), WORKER_START_TIMEOUT_MS);
    this.heartbeatTimer = setInterval(() => {
      if (Date.now() - this.lastWorkerMessageAt >= SHARED_LIVE_LEASE_MS) {
        // A frozen tab may outlive its lease. Rejoin the shared stream before
        // considering native fallback, which would consume another HTTP slot.
        this.resumeWorker();
      }
    }, SHARED_LIVE_HEARTBEAT_MS);
    worker.port.onmessage = (event: MessageEvent<SharedLiveEvent>) => {
      if (this.closed || this.worker !== worker) return;
      this.lastWorkerMessageAt = Date.now();
      if (this.startupTimer !== null) clearTimeout(this.startupTimer);
      this.startupTimer = null;
      const message = event.data;
      if (message.type === "ping") {
        try { sendClientLiveEvent(worker.port, { type: "pong" }); }
        catch { this.fallbackToNative(); }
      } else if (message.type === "event") {
        this.dispatchEvent(new MessageEvent(message.eventType, { data: message.data }));
      } else if (message.type === "snapshotRequired") {
        this.dispatchEvent(new Event("snapshotRequired"));
      } else if (message.type === "status") {
        this.readyState = message.readyState;
        if (message.readyState === 2) this.emitStatus("error", 2);
      } else if (message.type === "probeAck") {
        if (message.probeId === this.pendingProbeId) this.clearProbe();
      } else {
        this.emitStatus(message.type, message.readyState);
      }
    };
    worker.onerror = () => { if (this.worker === worker) this.fallbackToNative(); };
    worker.port.onmessageerror = () => { if (this.worker === worker) this.fallbackToNative(); };
    try {
      worker.port.start();
      sendClientLiveEvent(worker.port, { type: "start", reconnect });
    } catch {
      this.fallbackToNative();
    }
  }

  private detach() {
    this.clearProbe();
    if (this.startupTimer !== null) clearTimeout(this.startupTimer);
    if (this.heartbeatTimer !== null) clearInterval(this.heartbeatTimer);
    this.startupTimer = null;
    this.heartbeatTimer = null;
    const worker = this.worker;
    this.worker = null;
    if (worker) {
      worker.onerror = null;
      worker.port.onmessage = null;
      worker.port.onmessageerror = null;
      try { sendClientLiveEvent(worker.port, { type: "close" }); }
      catch { /* The hub lease reclaims a port whose channel already failed. */ }
      worker.port.close();
    }
    const native = this.native;
    this.native = null;
    native?.close();
  }

  private fallbackToNative() {
    if (this.closed || this.suspended || this.native) return;
    this.detach();
    try {
      const native = new EventSource("/api/events");
      this.native = native;
      native.onopen = () => { if (this.native === native) this.emitStatus("open", native.readyState); };
      native.onerror = () => { if (this.native === native) this.emitStatus("error", native.readyState); };
      for (const type of LIVE_EVENT_TYPES) {
        native.addEventListener(type, (event) => {
          if (this.native === native) {
            this.dispatchEvent(new MessageEvent(type, { data: (event as MessageEvent).data }));
          }
        });
      }
      this.emitStatus("error", native.readyState);
    } catch {
      this.emitStatus("error", 2);
    }
  }

  private resumeWorker() {
    if (this.closed) return;
    this.suspended = false;
    this.detach();
    this.emitStatus("error", 0);
    try { this.attachWorker(this.makeWorker(), true); }
    catch { this.fallbackToNative(); }
  }

  private readonly handlePageHide = (event: PageTransitionEvent) => {
    if (!event.persisted) { this.close(); return; }
    this.suspended = true;
    this.detach();
    this.readyState = 0;
  };

  private readonly handlePageShow = () => {
    if (this.suspended) this.resumeWorker();
  };

  private readonly handleVisibilityChange = () => {
    if (document.visibilityState !== "visible") {
      this.clearProbe();
      return;
    }
    const worker = this.worker;
    if (!worker || this.pendingProbeId !== null) return;
    // A locally fresh timestamp can come from pre-freeze queued data. Ask the
    // hub on this port instead: its matching reply follows earlier events in
    // the same ordered MessagePort queue, without forcing a healthy resync.
    const probeId = ++this.nextProbeId;
    this.pendingProbeId = probeId;
    this.probeTimer = setTimeout(() => {
      if (this.worker === worker && this.pendingProbeId === probeId) this.resumeWorker();
    }, WORKER_START_TIMEOUT_MS);
    try {
      sendClientLiveEvent(worker.port, { type: "probe", probeId });
    } catch {
      this.resumeWorker();
    }
  };

  private clearProbe() {
    if (this.probeTimer !== null) clearTimeout(this.probeTimer);
    this.probeTimer = null;
    this.pendingProbeId = null;
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.detach();
    this.readyState = 2;
    window.removeEventListener("pagehide", this.handlePageHide);
    window.removeEventListener("pageshow", this.handlePageShow);
    document.removeEventListener("visibilitychange", this.handleVisibilityChange);
  }
}

export function createLiveEventSource(reconnect = false) {
  if (typeof SharedWorker === "undefined") return new EventSource("/api/events");
  try {
    return new SharedLiveEventSource(createWorker(), reconnect);
  } catch {
    // Embedded browsers can expose SharedWorker while denying its constructor.
    return new EventSource("/api/events");
  }
}
