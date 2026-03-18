import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
    this.listeners[type] = (this.listeners[type] ?? []).filter((entry) => entry !== listener);
  }

  close() {}

  dispatchError() {
    this.onerror?.(new Event("error"));
  }

  dispatchOpen() {
    this.onopen?.(new Event("open"));
  }

  dispatchState(state: { revision: number; projects: []; sessions: [] }) {
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

describe("Backend connection state", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    HTMLElement.prototype.scrollTo = vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
  });

  afterEach(() => {
    HTMLElement.prototype.scrollTo = originalScrollTo;
  });

  it("shows connecting, reconnecting, and offline states around the backend event stream", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const ownNavigatorOnlineDescriptor = Object.getOwnPropertyDescriptor(window.navigator, "onLine");
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      throw new Error(`Unexpected fetch: ${String(input)}`);
    });

    Object.defineProperty(window.navigator, "onLine", {
      configurable: true,
      value: true,
    });
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);

    try {
      const { container } = render(<App />);

      expect(screen.getByText("Connecting")).toBeInTheDocument();
      expect(container.querySelector(".workspace-status-strip")).toBeNull();
      expect(screen.getByText("Connecting").closest(".pane-bar-right")).not.toBeNull();

      const eventSource = EventSourceMock.instances[EventSourceMock.instances.length - 1];
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
      expect(fetchMock).not.toHaveBeenCalled();

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

      act(() => {
        eventSource?.dispatchOpen();
      });
      await waitFor(() => {
        expect(screen.getByText("Connected")).toBeInTheDocument();
      });
    } finally {
      if (ownNavigatorOnlineDescriptor) {
        Object.defineProperty(window.navigator, "onLine", ownNavigatorOnlineDescriptor);
      } else {
        Reflect.deleteProperty(window.navigator, "onLine");
      }
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
});

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

