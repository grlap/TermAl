// Owns real App/transport/facade/hub composition tests with ordered test ports.
// Does not claim browser lifecycle or deployed-asset validation. Complements
// backend-connection.test.tsx's native-stream tests without growing that file.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import App from "./App";
import type { StateResponse } from "./api";
import { SharedLiveEventSource } from "./live-event-source";
import { createSharedLiveEventHub } from "./shared-live-events";
import { TestPort, TestSource } from "./shared-live-events.test-support";

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
