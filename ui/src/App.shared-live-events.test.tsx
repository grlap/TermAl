// Owns real App/transport/facade/hub composition tests with ordered test ports.
// Does not claim browser lifecycle or deployed-asset validation. Complements
// backend-connection.test.tsx's native-stream tests without growing that file.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import App from "./App";
import * as api from "./api";
import { requestSessionHistoryAroundPage, requestSessionHistoryOlderPage } from "./session-history-demand";
import type { StateResponse } from "./api";
import { SharedLiveEventSource } from "./live-event-source";
import { createSharedLiveEventHub } from "./shared-live-events";
import { TestPort, TestSource } from "./shared-live-events.test-support";
import { makeSession, makeSessionHydrationResponse } from "./app-test-harness";
import { INITIAL_STATE_RESYNC_DELAY_MS } from "./app-shell-internals";

function state(name: string, revision: number): StateResponse {
  return {
    revision, serverInstanceId: "shared-integration", codex: {}, agentReadiness: [],
    preferences: {
      defaultCodexModel: "default", defaultClaudeModel: "default",
      defaultCursorModel: "default", defaultGeminiModel: "default",
      defaultCodexReasoningEffort: "medium", defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
    },
    projects: [], orchestrators: [], workspaces: [],
    sessions: [{ id: "session-1", name, emoji: "1f9ea", agent: "Codex",
      workdir: "/repo", projectId: null, model: "gpt-5", status: "idle",
      preview: name, messageCount: 0, queuePaused: false }],
  };
}

function setupSharedBrowser() {
  const sources: TestSource[] = [];
  const ports: TestPort[] = [];
  const hub = createSharedLiveEventHub(() => {
    const source = new TestSource();
    sources.push(source);
    return source as unknown as EventSource;
  });
  class Worker {
    port = new TestPort();
    onerror = null;
    constructor() {
      const hubPort = new TestPort();
      this.port.peer = hubPort;
      hubPort.peer = this.port;
      ports.push(this.port);
      hub.connect(hubPort as unknown as MessagePort);
    }
  }
  vi.stubGlobal("SharedWorker", Worker);
  // Any unintended native fallback must make this test fail.
  const native = vi.fn(function () { throw new Error("Unexpected native SSE"); });
  vi.stubGlobal("EventSource", native);
  return { sources, ports, native, Worker };
}

let peer: SharedLiveEventSource | undefined;
const originalScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");

it("retries a stopped session tail only when the user selects it again", async () => {
  const { sources } = setupSharedBrowser();
  const snapshot = state("Stopped tail", 1);
  snapshot.sessions[0] = { ...snapshot.sessions[0], messageCount: 1, sessionMutationStamp: 1 };
  snapshot.sessions.push({ ...snapshot.sessions[0], id: "session-2", name: "Other session" });
  let tailReads = 0;
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/sessions/session-1" || url.pathname === "/api/sessions/session-2") {
      const id = url.pathname.split("/").slice(-1)[0];
      if (id === "session-1" && ++tailReads === 1) {
        return Response.json({ error: "Tail output limit", code: "output_limit_exceeded",
          phase: "response_encoding", limitBytes: 64, mutationOutcome: "not_applicable" }, { status: 500 });
      }
      return Response.json(makeSessionHydrationResponse(makeSession(id, {
        name: id === "session-1" ? "Stopped tail" : "Other session",
        messageCount: 1, sessionMutationStamp: 1, messagesLoaded: true,
        messages: [{ id: `text-${id}`, type: "text", author: "assistant", timestamp: "12:00", text: "Recovered tail" }],
      }), 1, "shared-integration"));
    }
    if (url.pathname.startsWith("/api/workspaces/")) return new Response("", { status: 404 });
    return Response.json(snapshot);
  }));
  render(<App />);
  await act(async () => {});
  await act(async () => { sources[0].open(); sources[0].emit("state", JSON.stringify(snapshot)); });
  async function select(name: string) {
    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    await act(async () => {});
    const list = document.querySelector(".session-list")!;
    fireEvent.click(within(list as HTMLElement).getByText(name, {
      selector: ".session-title-line strong",
    }).closest("button.session-row")!);
    await act(async () => {});
  }
  await select("Stopped tail");
  expect(tailReads).toBe(1);
  await act(async () => { await vi.advanceTimersByTimeAsync(180_000); });
  expect(tailReads).toBe(1);
  await act(async () => {
    window.dispatchEvent(new Event("offline"));
    window.dispatchEvent(new Event("online"));
  });
  expect(tailReads).toBe(1);
  expect(screen.queryByLabelText("Control panel backend output limit")).toBeNull();
  await select("Other session");
  await select("Stopped tail");
  expect(tailReads).toBe(2);
  expect(screen.getAllByText("Recovered tail").length).toBeGreaterThan(0);
});

it.each(["retry", "recreation"])("ignores an old state-read output limit after transport %s", async (recovery) => {
  const { sources } = setupSharedBrowser();
  let finishOldRead!: (response: Response) => void;
  let stateReads = 0;
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/state" && ++stateReads === 1) {
      return new Promise<Response>(resolve => { finishOldRead = resolve; });
    }
    if (url.pathname.startsWith("/api/workspaces/")) return new Response("", { status: 404 });
    return Response.json(state("Replacement state", 2));
  }));
  render(<App />);
  await act(async () => {});
  await act(async () => {
    sources[0].open();
    await vi.advanceTimersByTimeAsync(INITIAL_STATE_RESYNC_DELAY_MS);
  });
  expect(stateReads).toBe(1);
  expect(finishOldRead).toBeTypeOf("function");
  await act(async () => sources[0].emit("state", JSON.stringify(state("Original state", 1))));
  if (recovery === "retry") {
    await act(async () => sources[0].emit("outputLimit", '{"code":"output_limit_exceeded"}'));
    fireEvent.click(screen.getByLabelText("Control panel backend output limit"));
    await act(async () => {});
  } else {
    await act(async () => {
      sources[0].fail(2);
      await vi.advanceTimersByTimeAsync(1000);
    });
  }
  expect(sources).toHaveLength(2);
  await act(async () => {
    sources[1].open();
    sources[1].emit("state", JSON.stringify(state("Replacement state", 2)));
  });
  expect(screen.getAllByText("Replacement state").length).toBeGreaterThan(0);
  await act(async () => finishOldRead(Response.json({
    error: "Old state output limit", code: "output_limit_exceeded",
    phase: "response_encoding", limitBytes: 64, mutationOutcome: "not_applicable",
  }, { status: 500 })));
  expect(screen.queryByLabelText("Control panel backend output limit")).toBeNull();
  expect(sources[1].close).not.toHaveBeenCalled();
  await act(async () => sources[1].emit("state", JSON.stringify(state("Still receiving updates", 3))));
  expect(screen.getAllByText("Still receiving updates").length).toBeGreaterThan(0);
  // The replacement still owns its own terminal errors.
  await act(async () => sources[1].emit("outputLimit", '{"code":"output_limit_exceeded"}'));
  expect(screen.getByLabelText("Control panel backend output limit")).toBeInTheDocument();
});

it.each([false, true])("fences bounded history across outputLimit (response after retry: %s)", async (afterRetry) => {
  const { sources } = setupSharedBrowser();
  const snapshot = state("History fence", 1);
  snapshot.sessions[0] = { ...snapshot.sessions[0], messageCount: 100, sessionMutationStamp: 1 };
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) =>
    String(input).startsWith("/api/workspaces/") ? new Response("", { status: 404 }) : Response.json(snapshot)));
  const page = (index: number): Awaited<ReturnType<typeof api.fetchSessionHistory>> => ({
    revision: 1, serverInstanceId: "shared-integration",
    sessionMutationStamp: 1, messageCount: 100, messageStartIndex: index,
    messages: [{ id: `message-${index}`, type: "text", timestamp: "12:00", author: "assistant", text: `History ${index}` }],
    hasMore: true, hasNewer: true, nextBefore: `message-${index}`, nextAfter: `message-${index}`,
  });
  let resolveAround!: (value: ReturnType<typeof page>) => void;
  let resolveOlder!: (value: ReturnType<typeof page>) => void;
  const history = vi.spyOn(api, "fetchSessionHistory").mockResolvedValueOnce(page(40));
  render(<App />);
  await act(async () => {});
  await act(async () => {
    sources[0].open();
    sources[0].emit("state", JSON.stringify(snapshot));
  });
  let admitted!: Promise<boolean>;
  await act(async () => { admitted = requestSessionHistoryAroundPage("session-1", 40); });
  await expect(admitted).resolves.toBe(true);
  history.mockImplementationOnce(() => new Promise(resolve => { resolveAround = resolve; }))
    .mockImplementationOnce(() => new Promise(resolve => { resolveOlder = resolve; }));
  let pendingAround!: Promise<boolean>;
  let pendingOlder!: Promise<boolean>;
  await act(async () => {
    pendingAround = requestSessionHistoryAroundPage("session-1", 30);
    pendingOlder = requestSessionHistoryOlderPage("session-1");
  });
  expect(history).toHaveBeenCalledTimes(3);
  await act(async () => sources[0].emit("outputLimit", '{"code":"output_limit_exceeded"}'));
  await expect(pendingAround).resolves.toBe(false);
  await expect(pendingOlder).resolves.toBe(false);
  await expect(requestSessionHistoryAroundPage("session-1", 20)).resolves.toBe(false);
  expect(history).toHaveBeenCalledTimes(3);
  if (afterRetry) {
    fireEvent.click(screen.getByLabelText("Control panel backend output limit"));
    await act(async () => {});
    await act(async () => {
      sources[1].open();
      sources[1].emit("state", JSON.stringify(snapshot));
    });
  }
  await act(async () => { resolveAround(page(30)); resolveOlder(page(39)); });
  if (!afterRetry) {
    fireEvent.click(screen.getByLabelText("Control panel backend output limit"));
    await act(async () => {});
    await act(async () => {
      sources[1].open();
      sources[1].emit("state", JSON.stringify(snapshot));
    });
  }
  // Neither stale replacement nor stale prepend may change the retained window.
  history.mockResolvedValueOnce(page(39));
  let next!: Promise<boolean>;
  await act(async () => { next = requestSessionHistoryOlderPage("session-1"); });
  expect(history.mock.calls[history.mock.calls.length - 1]?.[1]).toEqual(expect.objectContaining({ before: "message-40" }));
  await expect(next).resolves.toBe(true);
});

it("retains last good state after outputLimit and does not poll or reopen until the user retries", async () => {
  const { sources, native } = setupSharedBrowser();
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    if (String(input) === "/api/state") return Response.json(state("Last good state", 1));
    if (String(input).startsWith("/api/workspaces/")) return new Response("", { status: 404 });
    return Response.json({});
  });
  vi.stubGlobal("fetch", fetchMock);
  render(<App />);
  await act(async () => {});
  await act(async () => {
    sources[0].open();
    sources[0].emit("heartbeat", "1");
    sources[0].emit("state", JSON.stringify(state("Last good state", 1)));
  });
  expect(screen.getAllByText("Last good state").length).toBeGreaterThan(0);
  await act(async () => sources[0].fail(0));
  await act(async () => sources[0].emit("outputLimit", '{"code":"output_limit_exceeded","limitBytes":67108864}'));
  expect(screen.getByLabelText("Control panel backend output limit")).toBeInTheDocument();
  const reads = () => fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state").length;
  const initialReads = reads();
  await act(async () => {
    window.dispatchEvent(new Event("offline"));
    window.dispatchEvent(new Event("online"));
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("pageshow"));
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(180_000);
  });
  expect(reads()).toBe(initialReads);
  expect(sources).toHaveLength(1);
  expect(native).not.toHaveBeenCalled();
  expect(screen.getAllByText("Last good state").length).toBeGreaterThan(0);
  fireEvent.click(screen.getByLabelText("Control panel backend output limit"));
  await act(async () => {});
  expect(sources).toHaveLength(2);
  await act(async () => {
    sources[1].open();
    sources[1].emit("state", JSON.stringify(state("Explicitly recovered", 2)));
  });
  expect(screen.queryByLabelText("Control panel backend output limit")).toBeNull();
  expect(screen.getAllByText("Explicitly recovered").length).toBeGreaterThan(0);
});
beforeEach(() => {
  vi.useFakeTimers();
  window.localStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Object.defineProperty(HTMLElement.prototype, "scrollTo", { configurable: true, value: vi.fn() });
});

afterEach(async () => {
  cleanup();
  peer?.close();
  peer = undefined;
  await act(async () => {});
  vi.restoreAllMocks();
  if (originalScrollTo) Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  else delete (HTMLElement.prototype as Partial<HTMLElement>).scrollTo;
  vi.unstubAllGlobals();
  vi.useRealTimers();
  window.localStorage.clear();
});

it("joins the real hub, hydrates its snapshot, recovers a closed stream through an epoch and releases ports", async () => {
  const { Worker, ports, sources, native } = setupSharedBrowser();
  peer = new SharedLiveEventSource(new Worker() as unknown as SharedWorker, false);
  await act(async () => {});
  sources[0].open();
  await act(async () => {});
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input) === "/api/state") return Response.json(state("Joined through hub", 5));
    if (init?.method === "PUT") return Response.json({});
    if (String(input).startsWith("/api/workspaces/")) return new Response("", { status: 404 });
    return Response.json({});
  });
  vi.stubGlobal("fetch", fetchMock);
  const view = render(<App />);
  await act(async () => {});
  expect(ports).toHaveLength(2);
  expect(ports[1].sent).toContainEqual({ type: "start", reconnect: false });
  expect(fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")).toHaveLength(1);
  expect(screen.getAllByText("Joined through hub").length).toBeGreaterThan(0);
  expect(screen.queryByLabelText("Control panel backend connecting")).toBeNull();

  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  expect(ports[1].sent).toContainEqual({ type: "probe", probeId: 1 });
  expect(ports).toHaveLength(2);
  expect(sources[0].close).not.toHaveBeenCalled();
  expect(screen.queryByLabelText("Control panel backend reconnecting")).toBeNull();

  await act(async () => sources[0].fail(2));
  expect(screen.getByLabelText("Control panel backend reconnecting")).toBeInTheDocument();
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(ports).toHaveLength(3);
  expect(ports[1].closed).toBe(true);
  expect(ports[1].sent).toContainEqual({ type: "close" });
  expect(ports[2].sent).toContainEqual({ type: "start", reconnect: true });
  expect(sources).toHaveLength(2);
  await act(async () => {
    sources[1].open();
    sources[1].emit("state", JSON.stringify(state("Recovered through hub", 6)));
  });
  expect(screen.getAllByText("Recovered through hub").length).toBeGreaterThan(0);
  expect(screen.queryByLabelText("Control panel backend reconnecting")).toBeNull();
  expect(native).not.toHaveBeenCalled();
  view.unmount();
  await act(async () => {});
  expect(ports[2].closed).toBe(true);
  expect(sources[1].close).not.toHaveBeenCalled();
  peer.close();
  await act(async () => {});
  expect(sources[1].close).toHaveBeenCalledOnce();
});
