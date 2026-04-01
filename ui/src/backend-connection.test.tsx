import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { StateResponse } from "./api";
import App from "./App";

class EventSourceMock {
  static instances: EventSourceMock[] = [];

  onerror: ((event: Event) => void) | null = null;

  onopen: ((event: Event) => void) | null = null;

  private listeners: Record<string, EventListener[]> = {};

  constructor(_url?: string) {
    EventSourceMock.instances.push(this);
  }

  addEventListener(type: string, listener: EventListener) {
    this.listeners[type] = [...(this.listeners[type] ?? []), listener];
  }

  removeEventListener(type: string, listener: EventListener) {
    this.listeners[type] = (this.listeners[type] ?? []).filter(
      (entry) => entry !== listener,
    );
  }

  close() {}

  dispatchError() {
    this.onerror?.(new Event("error"));
  }

  dispatchOpen() {
    this.onopen?.(new Event("open"));
  }

  dispatchState(state: StateResponse) {
    const event = new MessageEvent<string>("state", {
      data: JSON.stringify(state),
    });
    for (const listener of this.listeners.state ?? []) {
      listener(event);
    }
  }
}

class ResizeObserverMock {
  disconnect() {}

  observe() {}

  unobserve() {}
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: {
      "Content-Type": "application/json",
    },
    status: 200,
  });
}

describe("Backend connection state", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
  });

  afterEach(() => {
    HTMLElement.prototype.scrollTo = originalScrollTo;
  });

  it("shows connecting, reconnecting, and offline states around the backend event stream", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const ownNavigatorOnlineDescriptor = Object.getOwnPropertyDescriptor(
      window.navigator,
      "onLine",
    );
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        return new Response("", {
          status: 404,
        });
      }

      throw new Error(`Unexpected fetch: ${target}`);
    });

    Object.defineProperty(window.navigator, "onLine", {
      configurable: true,
      value: true,
    });
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      const { container } = render(<App />);

      expect(screen.getByText("Connecting")).toBeInTheDocument();
      expect(container.querySelector(".workspace-status-strip")).toBeNull();
      const workspaceSwitcherTrigger = screen.getByRole("button", {
        name: /workspace /i,
      });
      expect(workspaceSwitcherTrigger).toBeInTheDocument();
      expect(
        workspaceSwitcherTrigger.closest(".pane-bar-right"),
      ).not.toBeNull();
      expect(
        container.querySelector(
          ".control-panel-header-actions .workspace-switcher",
        ),
      ).toBeNull();

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
      });
      await waitFor(() => {
        expect(screen.getByText("Connected")).toBeInTheDocument();
      });

      act(() => {
        eventSource?.dispatchState({
          revision: 1,
          projects: [],
          sessions: [],
        });
      });

      act(() => {
        eventSource?.dispatchError();
      });
      await waitFor(() => {
        expect(screen.getByText("Reconnecting")).toBeInTheDocument();
      });
      expect(screen.getByRole("status")).toHaveAttribute(
        "title",
        "Live updates are disconnected. Trying to reconnect.",
      );
      const reconnectFetchCount = fetchMock.mock.calls.length;
      expect(reconnectFetchCount).toBeGreaterThanOrEqual(1);

      Object.defineProperty(window.navigator, "onLine", {
        configurable: true,
        value: false,
      });
      fireEvent(window, new Event("offline"));
      expect(screen.getByText("Offline")).toBeInTheDocument();

      Object.defineProperty(window.navigator, "onLine", {
        configurable: true,
        value: true,
      });
      fireEvent(window, new Event("online"));
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();
      expect(fetchMock).toHaveBeenCalledTimes(reconnectFetchCount);

      act(() => {
        eventSource?.dispatchOpen();
      });
      await waitFor(() => {
        expect(screen.getByText("Connected")).toBeInTheDocument();
      });
    } finally {
      if (ownNavigatorOnlineDescriptor) {
        Object.defineProperty(
          window.navigator,
          "onLine",
          ownNavigatorOnlineDescriptor,
        );
      } else {
        Reflect.deleteProperty(window.navigator, "onLine");
      }
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("cancels the reconnect fallback fetch as soon as the stream reopens", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        return new Response("", {
          status: 404,
        });
      }

      throw new Error(`Unexpected fetch: ${target}`);
    });
    const countStateFetches = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      render(<App />);

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState({
          revision: 1,
          projects: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource?.dispatchError();
      });
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();

      act(() => {
        eventSource?.dispatchOpen();
      });
      expect(screen.getByText("Connected")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount);
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("runs the reconnect fallback fetch only after the full 400ms delay", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        return new Response("", {
          status: 404,
        });
      }

      throw new Error(`Unexpected fetch: ${target}`);
    });
    const countStateFetches = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      render(<App />);

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState({
          revision: 1,
          projects: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource?.dispatchError();
      });
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(399);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("cancels the reconnect fallback fetch when a reconnect state event arrives first", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        return new Response("", {
          status: 404,
        });
      }

      throw new Error(`Unexpected fetch: ${target}`);
    });
    const countStateFetches = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      render(<App />);

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState({
          revision: 1,
          projects: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource?.dispatchError();
      });
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        eventSource?.dispatchState({
          revision: 1,
          projects: [],
          sessions: [],
        });
      });
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount);
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("adopts a same-revision reconnect fallback snapshot after backend restart", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let fallbackState = makeBackendStateResponse({
      revision: 1,
      sessionName: "Recovered Session",
      preview: "Recovered preview",
    });
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse(fallbackState);
      }
      if (target.startsWith("/api/workspaces/")) {
        return new Response("", {
          status: 404,
        });
      }

      throw new Error(`Unexpected fetch: ${target}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      render(<App />);

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState(
          makeBackendStateResponse({
            revision: 1,
            sessionName: "Original Session",
            preview: "Original preview",
          }),
        );
      });
      expect(await screen.findByText("Original Session")).toBeInTheDocument();
      expect(screen.getByText("Original preview")).toBeInTheDocument();

      vi.useFakeTimers();
      act(() => {
        eventSource?.dispatchError();
      });
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });

      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
      ).toBe(true);
      expect(screen.getByText("Recovered Session")).toBeInTheDocument();
      expect(screen.getByText("Recovered preview")).toBeInTheDocument();
      expect(screen.queryByText("Original Session")).toBeNull();
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("adopts a lower-revision reconnect fallback snapshot after backend restart", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse(
          makeBackendStateResponse({
            revision: 3,
            sessionName: "Recovered Session",
            preview: "Recovered preview",
          }),
        );
      }
      if (target.startsWith("/api/workspaces/")) {
        return new Response("", {
          status: 404,
        });
      }

      throw new Error(`Unexpected fetch: ${target}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      render(<App />);

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState(
          makeBackendStateResponse({
            revision: 5,
            sessionName: "Original Session",
            preview: "Original preview",
          }),
        );
      });
      expect(await screen.findByText("Original Session")).toBeInTheDocument();
      expect(screen.getByText("Original preview")).toBeInTheDocument();

      vi.useFakeTimers();
      act(() => {
        eventSource?.dispatchError();
      });
      expect(screen.getByText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });

      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
      ).toBe(true);
      expect(screen.getByText("Recovered Session")).toBeInTheDocument();
      expect(screen.getByText("Recovered preview")).toBeInTheDocument();
      expect(screen.queryByText("Original Session")).toBeNull();
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("discards a lower-revision reconnect fallback fetch after a newer SSE state arrives", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let resolveStateFetch: ((response: Response) => void) | null = null;
    const stateFetchPromise = new Promise<Response>((resolve) => {
      resolveStateFetch = resolve;
    });
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return stateFetchPromise;
      }
      if (target.startsWith("/api/workspaces/")) {
        return Promise.resolve(
          new Response("", {
            status: 404,
          }),
        );
      }

      return Promise.reject(new Error(`Unexpected fetch: ${target}`));
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );

    try {
      render(<App />);

      const eventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState(
          makeBackendStateResponse({
            revision: 5,
            sessionName: "Original Session",
            preview: "Original preview",
          }),
        );
      });
      expect(await screen.findByText("Original Session")).toBeInTheDocument();

      vi.useFakeTimers();
      act(() => {
        eventSource?.dispatchError();
      });

      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(resolveStateFetch).not.toBeNull();

      act(() => {
        eventSource?.dispatchOpen();
        eventSource?.dispatchState(
          makeBackendStateResponse({
            revision: 6,
            sessionName: "Newer Session",
            preview: "Newer preview",
          }),
        );
      });
      expect(screen.getByText("Newer Session")).toBeInTheDocument();
      expect(screen.getByText("Newer preview")).toBeInTheDocument();

      await act(async () => {
        resolveStateFetch?.(
          jsonResponse(
            makeBackendStateResponse({
              revision: 3,
              sessionName: "Recovered Session",
              preview: "Recovered preview",
            }),
          ),
        );
        await Promise.resolve();
      });

      expect(screen.getByText("Newer Session")).toBeInTheDocument();
      expect(screen.getByText("Newer preview")).toBeInTheDocument();
      expect(screen.queryByText("Recovered Session")).toBeNull();
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
});

function makeBackendStateResponse({
  revision,
  sessionName,
  preview,
}: {
  revision: number;
  sessionName: string;
  preview: string;
}): StateResponse {
  return {
    revision,
    projects: [],
    sessions: [
      {
        id: "session-1",
        name: sessionName,
        emoji: "1f9ea",
        agent: "Codex" as const,
        workdir: "/repo",
        projectId: null,
        model: "gpt-5",
        status: "idle" as const,
        preview,
        messages: [],
        pendingPrompts: [],
      },
    ],
  };
}

function restoreGlobal<Key extends "fetch" | "EventSource" | "ResizeObserver">(
  key: Key,
  originalValue: (typeof globalThis)[Key] | undefined,
) {
  if (originalValue === undefined) {
    delete (globalThis as Partial<typeof globalThis>)[key];
    return;
  }

  globalThis[key] = originalValue;
}
