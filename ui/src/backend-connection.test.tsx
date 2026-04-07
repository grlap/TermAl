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
import { BackendConnectionStatus } from "./workspace-shell-controls";

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

  dispatchState(state: unknown) {
    const event = new MessageEvent<string>("state", {
      data: JSON.stringify(state),
    });
    for (const listener of this.listeners.state ?? []) {
      listener(event);
    }
  }

  dispatchDelta(delta: unknown) {
    const event = new MessageEvent<string>("delta", {
      data: JSON.stringify(delta),
    });
    for (const listener of this.listeners.delta ?? []) {
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

function latestEventSource(): EventSourceMock {
  const eventSource = EventSourceMock.instances[EventSourceMock.instances.length - 1];
  if (!eventSource) {
    throw new Error("Event source not created");
  }
  return eventSource;
}

describe("Backend connection state", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
  });

  afterEach(() => {
    if (vi.isFakeTimers()) {
      vi.useRealTimers();
    }
    vi.unstubAllGlobals();
    HTMLElement.prototype.scrollTo = originalScrollTo;
  });

  it("updates workspace switcher summaries from live SSE state", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target === "/api/workspaces") {
        return jsonResponse({
          workspaces: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
        return new Response("", {
          status: 404,
        });
      }

      throw new Error("Unexpected fetch: " + target);
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
      const eventSource = latestEventSource();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          sessions: [],
          workspaces: [],
        });
      });

      fireEvent.click(screen.getByRole("button", { name: /workspace /i }));
      const switcherDialog = await screen.findByRole("dialog", {
        name: "Workspace switcher",
      });
      expect(switcherDialog).toBeInTheDocument();

      act(() => {
        eventSource.dispatchState({
          revision: 2,
          projects: [],
          orchestrators: [],
          sessions: [],
          workspaces: [
            {
              id: "monitor-live",
              revision: 3,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
            },
          ],
        });
      });

      await waitFor(() => {
        expect(screen.getAllByText("monitor-live").length).toBeGreaterThan(0);
      });

      act(() => {
        eventSource.dispatchState({
          revision: 3,
          projects: [],
          orchestrators: [],
          sessions: [],
          workspaces: [],
        });
      });

      await waitFor(() => {
        expect(screen.queryAllByText("monitor-live")).toHaveLength(0);
      });
    } finally {
      vi.stubGlobal("fetch", originalFetch);
      vi.stubGlobal("EventSource", originalEventSource);
      vi.stubGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("shows backend issue details in the connection tooltip and marks the chip as an issue", () => {
    const { container } = render(
      <BackendConnectionStatus
        state="reconnecting"
        issueDetail="Request failed with status 500."
      />,
    );

    fireEvent.mouseEnter(screen.getByLabelText("Reconnecting"));

    expect(screen.getByRole("tooltip")).toHaveTextContent("Reconnecting");
    expect(screen.getByRole("tooltip")).toHaveTextContent(
      "Request failed with status 500.",
    );
    expect(container.querySelector(".workspace-connection-status.has-issue")).not.toBeNull();
  });

  it("keeps the backend issue tooltip visible while moving from the chip toward the tooltip", () => {
    const { container } = render(
      <BackendConnectionStatus
        state="reconnecting"
        issueDetail="Request failed with status 500."
      />,
    );
    const status = container.querySelector(".workspace-connection-status");
    if (!(status instanceof HTMLDivElement)) {
      throw new Error("Connection status not found");
    }

    fireEvent.mouseEnter(status);
    const chip = screen.getByLabelText("Reconnecting");
    const tooltip = screen.getByRole("tooltip");

    fireEvent.mouseLeave(chip, { relatedTarget: tooltip });
    expect(screen.getByRole("tooltip")).toBe(tooltip);

    fireEvent.mouseLeave(status);
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("shows connecting, reconnecting, and offline states around the backend event stream", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const ownNavigatorOnlineDescriptor = Object.getOwnPropertyDescriptor(
      window.navigator,
      "onLine",
    );
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      expect(screen.getByLabelText("Connecting")).toBeInTheDocument();
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      act(() => {
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });

      act(() => {
        eventSource.dispatchError();
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();
      });
      fireEvent.mouseEnter(screen.getByLabelText("Reconnecting"));
      const reconnectTooltip = await screen.findByRole("tooltip");
      expect(reconnectTooltip).toHaveTextContent("Reconnecting");
      expect(reconnectTooltip).toHaveTextContent(
        "Live updates are disconnected. Trying to reconnect.",
      );
      const reconnectFetchCount = fetchMock.mock.calls.length;
      expect(reconnectFetchCount).toBeGreaterThanOrEqual(1);

      Object.defineProperty(window.navigator, "onLine", {
        configurable: true,
        value: false,
      });
      fireEvent(window, new Event("offline"));
      expect(screen.getByLabelText("Offline")).toBeInTheDocument();

      Object.defineProperty(window.navigator, "onLine", {
        configurable: true,
        value: true,
      });
      fireEvent(window, new Event("online"));
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();
      expect(fetchMock).toHaveBeenCalledTimes(reconnectFetchCount);

      act(() => {
        eventSource.dispatchOpen();
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
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

  it("keeps the reconnect fallback fetch armed when the stream reopens without usable data", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      act(() => {
        eventSource.dispatchOpen();
      });
      expect(screen.getByLabelText("Connected")).toBeInTheDocument();

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

  it("runs the reconnect fallback fetch after a second stream error when the reopened stream still delivers no data", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        eventSource.dispatchOpen();
      });
      expect(screen.getByLabelText("Connected")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

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

  it("runs the reconnect fallback fetch only after the full 400ms delay", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

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

  it("cancels the reconnect fallback fetch when an adopted reconnect state event arrives first", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      expect(screen.getByLabelText("Connected")).toBeInTheDocument();

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

  it("cancels the reconnect fallback fetch when an orchestrator delta proves the stream recovered", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchDelta({
          type: "orchestratorsUpdated",
          revision: 2,
          orchestrators: [],
        });
      });
      expect(screen.getByLabelText("Connected")).toBeInTheDocument();

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

  it("keeps the reconnect fallback fetch armed when an orchestrator delta arrives before a confirmed reopen", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        // Intentionally omit dispatchOpen(): a buffered pre-error delta must not
        // suppress the reconnect fallback until the stream proves it reopened.
        eventSource.dispatchDelta({
          type: "orchestratorsUpdated",
          revision: 2,
          orchestrators: [],
        });
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(299);
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

  it("keeps the reconnect fallback fetch armed when an applied session delta arrives before a confirmed reopen", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse(
          makeBackendStateResponse({
            revision: 2,
            sessionName: "Recovered Session",
            preview: "Recovered preview",
          }),
        );
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState(
          makeBackendStateResponse({
            revision: 1,
            sessionName: "Original Session",
            preview: "Original preview",
          }),
        );
      });
      await screen.findByText("Original preview");

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        // Intentionally omit dispatchOpen(): even an applied session delta may be
        // buffered from the pre-error stream and must not cancel the fallback yet.
        eventSource.dispatchDelta({
          type: "messageCreated",
          revision: 2,
          sessionId: "session-1",
          messageId: "message-1",
          messageIndex: 0,
          message: {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "",
          },
          preview: "Streaming preview",
          status: "active",
        });
      });
      expect(screen.getByText("Streaming preview")).toBeInTheDocument();
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(299);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);
      await act(async () => {
        await Promise.resolve();
      });
      expect(screen.getByText("Recovered preview")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("cancels the reconnect fallback fetch when an ignored delta arrives after a confirmed reopen", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchDelta({
          type: "messageCreated",
          revision: 1,
          sessionId: "missing-session",
          messageId: "message-1",
          messageIndex: 0,
          message: {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "",
          },
          preview: "Ignored preview",
          status: "active",
        });
      });
      expect(screen.getByLabelText("Connected")).toBeInTheDocument();

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

  it("keeps the reconnect fallback fetch armed when a pre-reopen gap resync only gets a stale state snapshot", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return jsonResponse({
            revision: 1,
            projects: [],
            sessions: [],
          });
        }
        return jsonResponse(
          makeBackendStateResponse({
            revision: 2,
            sessionName: "Recovered Session",
            preview: "Recovered preview",
          }),
        );
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        // Intentionally omit dispatchOpen(): the immediate resync gets a stale snapshot,
        // so the fast reconnect fallback must stay armed until a usable state arrives.
        eventSource.dispatchDelta({
          type: "messageCreated",
          revision: 3,
          sessionId: "session-1",
          messageId: "message-1",
          messageIndex: 0,
          message: {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "",
          },
          preview: "Buffered preview",
          status: "active",
        });
      });
      await act(async () => {
        await Promise.resolve();
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(299);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 2);
      // Flush the React state update from the adopted fetch response.
      await act(async () => {
        await Promise.resolve();
      });
      expect(screen.getByText("Recovered preview")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("keeps the reconnect fallback fetch armed when a gapped session delta arrives before a confirmed reopen", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          throw new Error("temporary outage");
        }
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        });
      });
      await waitFor(() => {
        expect(screen.getByLabelText("Connected")).toBeInTheDocument();
      });

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        // Intentionally omit dispatchOpen(): a buffered gap delta must not cancel
        // the reconnect fallback before the stream proves it reopened.
        eventSource.dispatchDelta({
          type: "messageCreated",
          revision: 3,
          sessionId: "session-1",
          messageId: "message-1",
          messageIndex: 0,
          message: {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "",
          },
          preview: "Buffered preview",
          status: "active",
        });
      });
      await act(async () => {
        await Promise.resolve();
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(299);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 2);
    } finally {
      vi.useRealTimers();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("keeps the reconnect fallback fetch armed when a reducer-rejected session delta arrives before a confirmed reopen", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          throw new Error("temporary outage");
        }
        return jsonResponse(
          makeBackendStateResponse({
            revision: 2,
            sessionName: "Recovered Session",
            preview: "Recovered preview",
          }),
        );
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState(
          makeBackendStateResponse({
            revision: 1,
            sessionName: "Original Session",
            preview: "Original preview",
          }),
        );
      });
      await screen.findByText("Original preview");

      const hydratedStateFetchCount = countStateFetches();
      vi.useFakeTimers();

      act(() => {
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      act(() => {
        // Intentionally omit dispatchOpen(): a reducer-rejected buffered delta must
        // not disarm the reconnect fallback until recovery is confirmed.
        eventSource.dispatchDelta({
          type: "messageCreated",
          revision: 2,
          sessionId: "missing-session",
          messageId: "message-1",
          messageIndex: 0,
          message: {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "",
          },
          preview: "Buffered preview",
          status: "active",
        });
      });
      await act(async () => {
        await Promise.resolve();
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(299);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(countStateFetches()).toBe(hydratedStateFetchCount + 2);
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
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse(fallbackState);
      }
      if (target.startsWith("/api/workspaces/")) {
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState(
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
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

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
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
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
        if (init?.method === "PUT") {
          return jsonResponse({
            layout: {
              id: "workspace-live",
              revision: 1,
              updatedAt: "2026-04-04 21:15:00",
              controlPanelSide: "left",
              workspace: { panes: [] },
            },
          });
        }
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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState(
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
        eventSource.dispatchError();
      });
      expect(screen.getByLabelText("Reconnecting")).toBeInTheDocument();

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

      const eventSource = latestEventSource();
      expect(eventSource).toBeDefined();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState(
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
        eventSource.dispatchError();
      });

      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(resolveStateFetch).not.toBeNull();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchState(
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
    codex: {},
    agentReadiness: [],
    preferences: {
      defaultCodexReasoningEffort: "medium",
      defaultClaudeEffort: "default",
    },
    projects: [],
    orchestrators: [],
    workspaces: [],
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
