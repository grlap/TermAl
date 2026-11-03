// Owns the isolated preview's fetch/EventSource boundary and finite wire replay.
// Does not replace App adoption, session publication, or workspace navigation.
import type { GitStatusResponse, SessionOverviewResponse, StateResponse } from "../api";
import type { Session } from "../types";
import { FIXTURE_SERVER_INSTANCE, FIXTURE_WORKSPACE_ID,
  type StateEventPacket, type StateEventWorkload } from "./state-event-test-fixtures";

export function installStateEventFixtureTransport(workload: StateEventWorkload) {
  const sources = new Set<FixtureEventSource>();
  const requests: { method: string; path: string }[] = [];
  const refusedRequests: string[] = [];
  const deliveries: { revision: number; bytes: number; dispatchMilliseconds: number;
    receiverCount: number; scheduledDelayMilliseconds: number }[] = [];
  let state: StateResponse = workload.stateAt(1);
  let timer: number | null = null;
  let nextPacket = 0;
  let prepared: StateEventPacket[] = [];
  let selectedRate: number | null = null;

  class FixtureEventSource extends EventTarget {
    static readonly CONNECTING = 0;
    static readonly OPEN = 1;
    static readonly CLOSED = 2;
    readonly CONNECTING = 0;
    readonly OPEN = 1;
    readonly CLOSED = 2;
    readonly url: string;
    readonly withCredentials = false;
    readyState = 0;
    onopen: ((event: Event) => void) | null = null;
    onerror: ((event: Event) => void) | null = null;
    onmessage: ((event: MessageEvent<string>) => void) | null = null;
    constructor(url: string | URL) {
      super();
      this.url = String(url);
      if (new URL(this.url, location.href).pathname !== "/api/events") {
        throw new Error(`Fixture refused EventSource ${this.url}`);
      }
      sources.add(this);
      queueMicrotask(() => {
        if (this.readyState === 2) return;
        this.readyState = 1;
        const event = new Event("open");
        this.dispatchEvent(event);
        this.onopen?.(event);
      });
    }
    close() { this.readyState = 2; sources.delete(this); }
  }

  Object.defineProperty(globalThis, "SharedWorker", { configurable: true, value: undefined });
  globalThis.EventSource = FixtureEventSource as unknown as typeof EventSource;
  globalThis.fetch = async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : String(input), location.href);
    const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
    requests.push({ method, path: url.pathname + url.search });
    if (method === "GET" && url.pathname === "/api/state") return Response.json(state);
    if (method === "GET" && url.pathname === "/api/workspaces") return Response.json({ workspaces: [] });
    if (url.pathname === `/api/workspaces/${FIXTURE_WORKSPACE_ID}` && ["GET", "PUT"].includes(method)) {
      const saved = method === "PUT" && typeof init?.body === "string"
        ? JSON.parse(init.body) : { controlPanelSide: "left", workspace: workload.workspace };
      return Response.json({ layout: { id: FIXTURE_WORKSPACE_ID, revision: state.revision,
        updatedAt: "2026-01-01 00:00:00", ...saved } });
    }
    if (method === "POST" && url.pathname === "/api/events/full-state") return new Response(null, { status: 204 });
    if (method === "GET" && url.pathname === "/api/git/status") {
      const owner = workload.sessions.get(url.searchParams.get("sessionId") ?? "");
      if (owner && url.searchParams.get("path") === owner.workdir) {
        const status: GitStatusResponse = { ahead: 0, behind: 0, branch: "fixture",
          files: [], isClean: true, repoRoot: owner.workdir, workdir: owner.workdir };
        return Response.json(status);
      }
    }
    const sessionRoute = /^\/api\/sessions\/([^/]+)(?:\/(agent-commands|history|overview))?$/.exec(url.pathname);
    const session = sessionRoute ? workload.sessions.get(decodeURIComponent(sessionRoute[1])) : undefined;
    if (method === "GET" && session) {
      if (sessionRoute![2] === "agent-commands") return Response.json({ commands: [] });
      if (sessionRoute![2] === "overview") {
        const requested = Number(url.searchParams.get("buckets") ?? 200);
        if (!Number.isInteger(requested) || requested < 1 || requested > 512) {
          throw new Error("Fixture overview requires between 1 and 512 buckets.");
        }
        // This fixture contains only alternating user/assistant text, without markers.
        // Project the complete fixture transcript, independently of the loaded tail.
        const count = session.messages.length;
        const bucketCount = Math.min(requested, Math.max(1, count));
        const overview: SessionOverviewResponse = {
          sessionId: session.id, messageCount: count,
          sessionMutationStamp: session.sessionMutationStamp ?? 0,
          buckets: Array.from({ length: bucketCount }, (_, index) => {
            const messages = session.messages.slice(Math.ceil(index * count / bucketCount),
              Math.ceil((index + 1) * count / bucketCount));
            return { c: messages.length, k: "text", m: false,
              u: messages.filter(message => message.type === "text" && message.author === "you").length };
          }),
          markers: [], latestPosition: Math.max(0, count - 1),
        };
        return Response.json(overview);
      }
      if (sessionRoute![2] === "history") {
        // This timing fixture supports tail and explicit-index pages only.
        // Refuse cursor/overview navigation rather than report an unrelated tail.
        if ([...url.searchParams.keys()].some(key => key !== "limit" && key !== "start")) {
          const failure = `${method} ${url.pathname}${url.search}`;
          refusedRequests.push(failure);
          throw new Error(`Isolated App fixture refused history navigation: ${failure}`);
        }
        const limit = Number(url.searchParams.get("limit") ?? 20);
        const start = Number(url.searchParams.get("start") ?? Math.max(0, session.messages.length - limit));
        if (!Number.isSafeInteger(limit) || limit < 1 || !Number.isSafeInteger(start) || start < 0 || start > session.messages.length) {
          const failure = `${method} ${url.pathname}${url.search}`;
          refusedRequests.push(failure);
          throw new Error(`Isolated App fixture refused history range: ${failure}`);
        }
        const messages = session.messages.slice(start, start + limit);
        return Response.json({ messages, messageCount: session.messages.length, messageStartIndex: start,
          hasMore: start > 0, hasNewer: start + messages.length < session.messages.length,
          revision: state.revision, sessionMutationStamp: session.sessionMutationStamp,
          serverInstanceId: FIXTURE_SERVER_INSTANCE });
      }
      const tail = url.searchParams.get("tail");
      const start = tail ? Math.max(0, session.messages.length - Number(tail)) : 0;
      const body: Session = { ...session, messages: session.messages.slice(start),
        messageStartIndex: start, messagesLoaded: start === 0, hasOlderHistory: start > 0 };
      return Response.json({ session: body, revision: state.revision, serverInstanceId: FIXTURE_SERVER_INSTANCE });
    }
    const failure = `${method} ${url.pathname}${url.search}`;
    refusedRequests.push(failure);
    throw new Error(`Isolated App fixture refused network request: ${failure}`);
  };

  function stop() { if (timer !== null) window.clearInterval(timer); timer = null; }
  function prepare(count = 24) {
    stop(); prepared = workload.preparePackets(count); nextPacket = 0;
    return { count, firstBytes: prepared[0].bytes };
  }
  function replayOne(scheduledAt = performance.now()) {
    const packet = prepared[nextPacket++];
    if (!packet) { stop(); return false; }
    state = packet.state;
    const startedAt = performance.now();
    let receiverCount = 0;
    for (const source of sources) {
      if (source.readyState !== 1) continue;
      receiverCount += 1;
      source.dispatchEvent(new MessageEvent("state", { data: packet.wire }));
    }
    deliveries.push({ revision: state.revision, bytes: packet.bytes, receiverCount,
      dispatchMilliseconds: performance.now() - startedAt,
      scheduledDelayMilliseconds: startedAt - scheduledAt });
    return true;
  }
  function start(rate: 1 | 5) {
    if (!prepared.length) throw new Error("Prepare finite wire before starting the measurement.");
    stop(); selectedRate = rate;
    const interval = 1000 / rate;
    let scheduledAt = performance.now() + interval;
    timer = window.setInterval(() => { replayOne(scheduledAt); scheduledAt += interval; }, interval);
  }
  return { prepare, replayOne, start, stop, requests, refusedRequests, deliveries,
    snapshot: () => ({ revision: state.revision, sessionCount: workload.sessions.size,
      readySources: sources.size, preparedEvents: prepared.length, deliveredEvents: deliveries.length,
      selectedRate, rateIsMeasuredLive: false }) };
}
