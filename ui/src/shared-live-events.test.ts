import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import serverSseSource from "../../src/api_sse.rs?raw";
import { createLiveEventSource, SharedLiveEventSource } from "./live-event-source";
import { TestPort, TestSource } from "./shared-live-events.test-support";
import {
  createSharedLiveEventHub, LIVE_EVENT_TYPES, SHARED_LIVE_LEASE_MS,
  SHARED_LIVE_CONNECTING_STALE_MS,
} from "./shared-live-events";

const clients: SharedLiveEventSource[] = [];

function setup() {
  const sources: TestSource[] = [];
  const hub = createSharedLiveEventHub(() => {
    const source = new TestSource();
    sources.push(source);
    return source as unknown as EventSource;
  });
  const workers: { port: TestPort; onerror: null }[] = [];
  function makeWorker() {
    const clientPort = new TestPort();
    const hubPort = new TestPort();
    clientPort.peer = hubPort;
    hubPort.peer = clientPort;
    hub.connect(hubPort as unknown as MessagePort);
    const worker = { port: clientPort, onerror: null };
    workers.push(worker);
    return { worker, hubPort };
  }
  function connect(reconnect = false) {
    const { worker, hubPort } = makeWorker();
    const client = new SharedLiveEventSource(worker as unknown as SharedWorker, reconnect,
      () => makeWorker().worker as unknown as SharedWorker);
    clients.push(client);
    return { client, worker, hubPort };
  }
  return { sources, connect, workers };
}

async function flushMessages() {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => vi.useFakeTimers());

afterEach(async () => {
  for (const client of clients.splice(0)) client.close();
  await flushMessages();
  expect(vi.getTimerCount()).toBe(0);
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("shared live updates", () => {
  it("leaves HTTP capacity for a prompt when eight tabs subscribe", async () => {
    const { sources, connect } = setup();
    const tabs = Array.from({ length: 8 }, () => connect());
    await flushMessages();
    // A browser has six HTTP/1.1 slots. Previously each tab's SSE held one.
    const openConnections = sources.filter((source) => source.readyState !== 2).length;
    expect(openConnections).toBe(1);
    expect(openConnections + 1 /* prompt POST */).toBeLessThanOrEqual(6);
    const deltas = tabs.map(({ client }) => {
      const listener = vi.fn();
      client.addEventListener("delta", listener);
      return listener;
    });
    sources[0].open();
    sources[0].emit("delta", '{"type":"messageAppended"}');
    await flushMessages();
    for (const listener of deltas) {
      expect(listener).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
        data: '{"type":"messageAppended"}',
      }));
    }
    for (const { client } of tabs) client.close();
    await flushMessages();
    expect(sources[0].close).toHaveBeenCalledOnce();
  });

  it("resyncs a late tab without replaying a stale cached snapshot or reopening SSE", async () => {
    const { sources, connect } = setup();
    connect();
    await flushMessages();
    sources[0].open();
    sources[0].emit("state", '{"revision":1}');
    sources[0].emit("delta", '{"revision":2}');
    await flushMessages();
    const { client } = connect();
    const events: string[] = [];
    client.onopen = () => events.push("open");
    client.addEventListener("snapshotRequired", () => events.push("resync"));
    client.addEventListener("state", () => events.push("cached state"));
    await flushMessages();
    expect(events).toEqual(["open", "resync"]);
    expect(sources).toHaveLength(1);
    expect(client.readyState).toBe(1);
  });

  it.each(LIVE_EVENT_TYPES)("forwards %s data exactly", async (type) => {
    const { sources, connect } = setup();
    const { client } = connect();
    const listener = vi.fn();
    client.addEventListener(type, listener);
    await flushMessages();
    sources[0].emit(type, "payload\nwith lines");
    await flushMessages();
    expect(listener).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      data: "payload\nwith lines",
    }));
  });

  it("keeps other tabs connected when a subscriber closes", async () => {
    const { sources, connect } = setup();
    const first = connect().client;
    const second = connect().client;
    await flushMessages();
    const firstDelta = vi.fn();
    const secondDelta = vi.fn();
    first.addEventListener("delta", firstDelta);
    second.addEventListener("delta", secondDelta);
    first.close();
    first.close();
    await flushMessages();
    expect(sources[0].close).not.toHaveBeenCalled();
    sources[0].emit("delta", "next");
    await flushMessages();
    expect(firstDelta).not.toHaveBeenCalled();
    expect(secondDelta).toHaveBeenCalledOnce();
    expect(first.readyState).toBe(2);
  });

  it.each([0, 2])("propagates upstream failure state %s", async (readyState) => {
    const { sources, connect } = setup();
    const { client } = connect();
    const error = vi.fn();
    client.onerror = error;
    await flushMessages();
    sources[0].fail(readyState);
    await flushMessages();
    expect(client.readyState).toBe(readyState);
    expect(error).toHaveBeenCalledOnce();
  });

  it("restarts a stuck stream for all tabs and fences old stream callbacks", async () => {
    const { sources, connect } = setup();
    const first = connect().client;
    await flushMessages();
    const error = vi.fn();
    const data = vi.fn();
    first.onerror = error;
    first.addEventListener("state", data);
    vi.setSystemTime(Date.now() + SHARED_LIVE_CONNECTING_STALE_MS);
    connect(true);
    await flushMessages();
    expect(sources).toHaveLength(2);
    expect(sources[0].close).toHaveBeenCalledOnce();
    expect(error).toHaveBeenCalledOnce();
    sources[0].emit("state", "old");
    sources[0].fail(2);
    sources[1].open();
    sources[1].emit("state", "new");
    await flushMessages();
    expect(data).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: "new" }));
    expect(first.readyState).toBe(1);
    expect(error).toHaveBeenCalledOnce();
  });

  it("reopens a permanently closed source when a tab recovers", async () => {
    const { sources, connect } = setup();
    connect();
    await flushMessages();
    sources[0].fail(2);
    connect();
    await flushMessages();
    expect(sources).toHaveLength(2);
  });

  it("reports a worker failure to the existing reconnect watchdog", async () => {
    const { connect } = setup();
    const { client, worker } = connect();
    const error = vi.fn();
    client.onerror = error;
    (worker as unknown as SharedWorker).onerror?.(new ErrorEvent("error"));
    expect(client.readyState).toBe(2);
    expect(error).toHaveBeenCalledOnce();
  });

  it("releases a cached page's port and restores its live events on pageshow", async () => {
    const { sources, connect } = setup();
    const { client } = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    window.dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true }));
    await flushMessages();
    expect(client.readyState).toBe(0);
    expect(sources[0].close).toHaveBeenCalledOnce();
    window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true }));
    await flushMessages();
    expect(sources).toHaveLength(2);
    const delta = vi.fn();
    client.addEventListener("delta", delta);
    sources[1].open();
    sources[1].emit("delta", "after restore");
    await flushMessages();
    expect(client.readyState).toBe(1);
    expect(delta).toHaveBeenCalledOnce();
    window.dispatchEvent(new PageTransitionEvent("pagehide", { persisted: false }));
    await flushMessages();
    expect(client.readyState).toBe(2);
    expect(sources[0].close).toHaveBeenCalledOnce();
    expect(sources[1].close).toHaveBeenCalledOnce();
  });

  it("uses native EventSource when SharedWorker is unavailable", () => {
    vi.stubGlobal("SharedWorker", undefined);
    const native = new TestSource();
    const factory = vi.fn(function () { return native; });
    vi.stubGlobal("EventSource", factory);
    expect(createLiveEventSource()).toBe(native);
    expect(factory).toHaveBeenCalledWith("/api/events");
  });

  it("uses native EventSource when the browser denies SharedWorker construction", () => {
    vi.stubGlobal("SharedWorker", vi.fn(function () { throw new Error("denied"); }));
    const native = new TestSource();
    vi.stubGlobal("EventSource", vi.fn(function () { return native; }));
    expect(createLiveEventSource()).toBe(native);
  });
});

describe("shared stream recovery requirements", () => {
  it("falls back to native SSE after an asynchronous worker failure", async () => {
    const { connect } = setup();
    const { client, worker } = connect();
    const native = new TestSource();
    const factory = vi.fn(function () { return native; });
    vi.stubGlobal("EventSource", factory);
    const delta = vi.fn();
    client.addEventListener("delta", delta);
    await flushMessages();

    (worker as unknown as SharedWorker).onerror?.(new ErrorEvent("error"));
    await flushMessages();

    expect(factory).toHaveBeenCalledExactlyOnceWith("/api/events");
    native.open();
    native.emit("delta", "recovered");
    expect(client.readyState).toBe(1);
    expect(delta).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      data: "recovered",
    }));
  });

  it("dispatches worker errors to EventTarget listeners as well as onerror", async () => {
    const { connect } = setup();
    const { client, worker } = connect();
    const propertyError = vi.fn();
    const listenerError = vi.fn();
    client.onerror = propertyError;
    client.addEventListener("error", listenerError);
    await flushMessages();

    (worker as unknown as SharedWorker).onerror?.(new ErrorEvent("error"));

    expect(propertyError).toHaveBeenCalledOnce();
    expect(listenerError).toHaveBeenCalledOnce();
  });

  it("recovers a failed client message channel instead of staying falsely OPEN", async () => {
    const { sources, connect } = setup();
    const { client, worker } = connect();
    const factory = vi.fn(function () { return new TestSource(); });
    vi.stubGlobal("EventSource", factory);
    await flushMessages();
    sources[0].open();
    await flushMessages();
    expect(client.readyState).toBe(1);

    worker.port.onmessageerror?.();
    await flushMessages();

    expect(factory).toHaveBeenCalledExactlyOnceWith("/api/events");
    expect(client.readyState).not.toBe(1);
  });

  it("does not interrupt a healthy shared stream for a stale tab reconnect", async () => {
    const { sources, connect } = setup();
    const healthy = connect().client;
    await flushMessages();
    sources[0].open();
    await flushMessages();
    const error = vi.fn();
    healthy.onerror = error;

    const recovering = connect(true).client;
    const snapshot = vi.fn();
    recovering.addEventListener("snapshotRequired", snapshot);
    await flushMessages();

    expect(sources).toHaveLength(1);
    expect(sources[0].close).not.toHaveBeenCalled();
    expect(error).not.toHaveBeenCalled();
    expect(snapshot).toHaveBeenCalledOnce();
  });

  it("coalesces concurrent tab recovery instead of replacing each fresh attempt", async () => {
    const { sources, connect } = setup();
    connect();
    await flushMessages();
    // The original CONNECTING source is stuck; two tabs independently reach
    // their recovery epoch before either has observed a replacement opening.
    vi.setSystemTime(Date.now() + SHARED_LIVE_CONNECTING_STALE_MS);
    connect(true);
    connect(true);
    await flushMessages();

    expect(sources).toHaveLength(2);
    expect(sources[0].close).toHaveBeenCalledOnce();
    expect(sources[1].close).not.toHaveBeenCalled();
  });
});

describe("shared stream lifecycle", () => {
  it.each(["hide", "close", "pagehide"])("cancels a pending visibility probe on %s", async (action) => {
    const { sources, connect, workers } = setup();
    const { client, hubPort } = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    // Hold the reply, not the whole hub, so close can still release resources.
    vi.spyOn(hubPort, "postMessage").mockImplementation(() => {});
    document.dispatchEvent(new Event("visibilitychange"));
    await flushMessages();
    if (action === "hide") {
      vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
    } else if (action === "close") {
      client.close();
    } else {
      window.dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true }));
    }
    await vi.advanceTimersByTimeAsync(8_000);
    expect(workers).toHaveLength(1);
    if (action === "hide") {
      expect(client.readyState).toBe(1);
      expect(sources[0].close).not.toHaveBeenCalled();
    } else {
      expect(sources[0].close).toHaveBeenCalledOnce();
    }
  });

  it("recovers a hub-expired port despite a freshly delivered pre-freeze message", async () => {
    const { sources, connect, workers } = setup();
    const { client, worker, hubPort } = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    const queuedMessage = worker.port.onmessage;
    worker.port.close();
    vi.setSystemTime(Date.now() + SHARED_LIVE_LEASE_MS + 1);
    queuedMessage?.(new MessageEvent("message", {
      data: { type: "event", eventType: "delta", data: "old queued data" },
    }));
    // Execute the hub sweep while the client's activity clock still looks fresh.
    await vi.advanceTimersByTimeAsync(15_000);
    expect(hubPort.closed).toBe(true);
    expect(sources[0].close).toHaveBeenCalledOnce();
    expect(workers).toHaveLength(1);
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(8_000);
    expect(workers).toHaveLength(2);
    expect(sources).toHaveLength(2);
    const delta = vi.fn();
    client.addEventListener("delta", delta);
    sources[1].open();
    sources[1].emit("delta", "after expiry recovery");
    await flushMessages();
    expect(client.readyState).toBe(1);
    expect(delta).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: "after expiry recovery" }));
  });

  it.each([1, 3])("keeps %i healthy tabs attached on visibility return", async (count) => {
    const { sources, connect, workers } = setup();
    const tabs = Array.from({ length: count }, () => connect());
    await flushMessages();
    sources[0].open();
    await flushMessages();
    const errors = tabs.map(({ client }) => {
      const listener = vi.fn();
      client.onerror = listener;
      return listener;
    });
    const snapshots = tabs.map(({ client }) => {
      const listener = vi.fn();
      client.addEventListener("snapshotRequired", listener);
      return listener;
    });
    const deltas = tabs.map(({ client }) => {
      const listener = vi.fn();
      client.addEventListener("delta", listener);
      return listener;
    });
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    // The reply must follow queued data on the same port, not replace it.
    sources[0].emit("delta", "queued while hidden");
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    document.dispatchEvent(new Event("visibilitychange"));
    await flushMessages();
    await vi.advanceTimersByTimeAsync(8_000);
    expect(workers).toHaveLength(count);
    expect(sources).toHaveLength(1);
    expect(sources[0].close).not.toHaveBeenCalled();
    for (const error of errors) expect(error).not.toHaveBeenCalled();
    for (const snapshot of snapshots) expect(snapshot).not.toHaveBeenCalled();
    for (const delta of deltas) {
      expect(delta).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: "queued while hidden" }));
    }
    for (const { client } of tabs) expect(client.readyState).toBe(1);
  });

  it("does not accept an old probe reply as proof of current attachment", async () => {
    const { sources, connect, workers } = setup();
    const { client, worker, hubPort } = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    document.dispatchEvent(new Event("visibilitychange"));
    await flushMessages();
    const reply = hubPort.sent.find((message) => (message as { type: string }).type === "probeAck");
    expect(reply).toBeDefined();
    worker.port.close();
    document.dispatchEvent(new Event("visibilitychange"));
    worker.port.onmessage?.(new MessageEvent("message", { data: reply }));
    await vi.advanceTimersByTimeAsync(8_000);
    expect(workers).toHaveLength(2);
    sources[0].emit("delta", "recovered");
    const delta = vi.fn();
    client.addEventListener("delta", delta);
    await flushMessages();
    expect(delta).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: "recovered" }));
    // The old silently closed port is still leased until the hub sweeps it.
    await vi.advanceTimersByTimeAsync(SHARED_LIVE_LEASE_MS);
  });

  it("keeps the named event allowlist in sync with the server wire events", () => {
    const names = [...serverSseSource.matchAll(/\.event\("([^"]+)"\)/g)].map((match) => match[1]);
    expect([...new Set(names)].sort()).toEqual([...LIVE_EVENT_TYPES].sort());
  });

  it("acknowledges a joining tab during CONNECTING without replacing the fresh stream", async () => {
    const { sources, connect } = setup();
    connect();
    await flushMessages();
    const { client, hubPort } = connect(true);
    await flushMessages();
    expect(hubPort.sent).toContainEqual({ type: "status", readyState: 0 });
    expect(client.readyState).toBe(0);
    expect(sources).toHaveLength(1);
    const state = vi.fn();
    client.addEventListener("state", state);
    sources[0].open();
    sources[0].emit("state", "initial");
    await flushMessages();
    expect(client.readyState).toBe(1);
    expect(state).toHaveBeenCalledOnce();
  });

  it("expires a silently closed peer while keeping a responding tab streaming", async () => {
    const { sources, connect } = setup();
    const abandoned = connect();
    const healthy = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    // Chrome does not provide MessagePort.onclose and posting to a dead peer
    // silently succeeds. Closing locally must not magically inform the hub.
    abandoned.worker.port.close();
    abandoned.client.close();
    await vi.advanceTimersByTimeAsync(SHARED_LIVE_LEASE_MS);
    expect(abandoned.hubPort.closed).toBe(true);
    expect(healthy.hubPort.closed).toBe(false);
    const delta = vi.fn();
    healthy.client.addEventListener("delta", delta);
    sources[0].emit("delta", "still live");
    await flushMessages();
    expect(delta).toHaveBeenCalledOnce();
    healthy.client.close();
    await flushMessages();
    expect(sources[0].close).toHaveBeenCalledOnce();
  });

  it("returns a frozen tab through a fresh port even when old queued data looks recent", async () => {
    const { sources, connect, workers } = setup();
    const { client, worker } = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    // No close message reaches the hub. After a browser suspension a buffered
    // message can run before visibilitychange and refresh the activity clock.
    const queuedMessage = worker.port.onmessage;
    worker.port.close();
    vi.setSystemTime(Date.now() + SHARED_LIVE_LEASE_MS + 1);
    queuedMessage?.(new MessageEvent("message", {
      data: { type: "event", eventType: "delta", data: "buffered before freeze" },
    }));
    document.dispatchEvent(new Event("visibilitychange"));
    await flushMessages();
    // Only an unanswered fresh probe permits recovery, not queued activity.
    await vi.advanceTimersByTimeAsync(8_000);
    expect(workers).toHaveLength(2);
    expect(client.readyState).toBe(1);
    expect(sources).toHaveLength(1);
    const delta = vi.fn();
    client.addEventListener("delta", delta);
    sources[0].emit("delta", "resumed");
    await flushMessages();
    expect(delta).toHaveBeenCalledOnce();
    // Let the hub reclaim the old port whose local close could not notify it.
    await vi.advanceTimersByTimeAsync(15_000);
    expect(workers).toHaveLength(2);
  });

  it("falls back when a worker script never acknowledges its subscription", async () => {
    const native = new TestSource();
    const factory = vi.fn(function () { return native; });
    vi.stubGlobal("EventSource", factory);
    const port = { onmessage: null, onmessageerror: null, start() {}, postMessage() {}, close() {} };
    const client = new SharedLiveEventSource({ port, onerror: null } as unknown as SharedWorker, false);
    clients.push(client);
    const opened = vi.fn();
    client.addEventListener("open", opened);
    await vi.advanceTimersByTimeAsync(8_000);
    expect(factory).toHaveBeenCalledOnce();
    native.open();
    expect(opened).toHaveBeenCalledOnce();
    client.close();
    native.emit("state", "late");
    expect(native.close).toHaveBeenCalledOnce();
  });

  it("constructs the module worker through the public factory", async () => {
    const port = { onmessage: null, onmessageerror: null, start() {}, postMessage: vi.fn(), close() {} };
    const factory = vi.fn(function (_url: URL, _options: WorkerOptions) { return { port, onerror: null }; });
    vi.stubGlobal("SharedWorker", factory);
    const client = createLiveEventSource();
    clients.push(client as SharedLiveEventSource);
    expect(factory).toHaveBeenCalledWith(expect.any(URL), { type: "module", name: "termal-live-events-v2" });
    expect(String(factory.mock.calls[0][0])).toContain("live-events.worker.ts");
    expect(port.postMessage).toHaveBeenCalledWith({ type: "start", reconnect: false });
  });

  it("wires the worker connect event to an upstream and delivers live data", async () => {
    const upstream = new TestSource();
    const createSource = vi.fn(function (_url: string) { return upstream; });
    vi.stubGlobal("EventSource", createSource);
    const scope = { onconnect: null as ((event: MessageEvent) => void) | null };
    vi.stubGlobal("self", scope);
    await import("./live-events.worker");
    const hubPort = new TestPort();
    const clientPort = new TestPort();
    hubPort.peer = clientPort;
    clientPort.peer = hubPort;
    scope.onconnect?.({ ports: [hubPort] } as unknown as MessageEvent);
    const client = new SharedLiveEventSource({ port: clientPort, onerror: null } as unknown as SharedWorker, false);
    clients.push(client);
    const state = vi.fn();
    client.addEventListener("state", state);
    await flushMessages();
    expect(createSource).toHaveBeenCalledExactlyOnceWith("/api/events");
    upstream.open();
    upstream.emit("state", "worker entry snapshot");
    await flushMessages();
    expect(client.readyState).toBe(1);
    expect(state).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: "worker entry snapshot" }));
    client.close();
    await flushMessages();
    expect(upstream.close).toHaveBeenCalledOnce();
  });

  it("reports a hub-side channel failure instead of leaving the tab falsely OPEN", async () => {
    const { sources, connect } = setup();
    const { client, hubPort } = connect();
    await flushMessages();
    sources[0].open();
    await flushMessages();
    const error = vi.fn();
    client.addEventListener("error", error);
    hubPort.onmessageerror?.();
    await flushMessages();
    expect(client.readyState).toBe(2);
    expect(error).toHaveBeenCalledOnce();
    expect(sources[0].close).toHaveBeenCalledOnce();
  });
});
