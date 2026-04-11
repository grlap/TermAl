import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import { WORKSPACE_LAYOUT_STORAGE_KEY } from "./workspace-storage";

type ObservedWorkspaceFilesChangedEvent = {
  revision: number;
  changes: Array<{
    path: string;
    kind: string;
    rootPath?: string | null;
    sessionId?: string | null;
  }>;
};

const observedFileEvents = vi.hoisted(
  () => [] as ObservedWorkspaceFilesChangedEvent[],
);

vi.mock("./panels/FileSystemPanel", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  return {
    FileSystemPanel: ({
      workspaceFilesChangedEvent,
    }: {
      workspaceFilesChangedEvent: ObservedWorkspaceFilesChangedEvent | null;
    }) => {
      React.useEffect(() => {
        if (workspaceFilesChangedEvent) {
          observedFileEvents.push(workspaceFilesChangedEvent);
        }
      }, [workspaceFilesChangedEvent]);

      return React.createElement("div", { "data-testid": "filesystem-panel" });
    },
  };
});

class EventSourceMock {
  static instances: EventSourceMock[] = [];

  onerror: ((event: Event) => void) | null = null;
  onopen: ((event: Event) => void) | null = null;

  private listeners = new Map<
    string,
    Set<(event: MessageEvent<string>) => void>
  >();

  constructor() {
    EventSourceMock.instances.push(this);
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    const listeners =
      this.listeners.get(type) ??
      new Set<(event: MessageEvent<string>) => void>();
    listeners.add(normalizeMessageEventListener(listener));
    this.listeners.set(type, listeners);
  }

  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
  ) {
    this.listeners.get(type)?.delete(normalizeMessageEventListener(listener));
  }

  close() {}

  dispatchOpen() {
    this.onopen?.(new Event("open"));
  }

  dispatchNamedEvent(type: string, data: unknown) {
    const event = { data: JSON.stringify(data) } as MessageEvent<string>;
    this.listeners.get(type)?.forEach((listener) => listener(event));
  }
}

class ResizeObserverMock {
  disconnect() {}

  observe() {}

  unobserve() {}
}

function normalizeMessageEventListener(
  listener: EventListenerOrEventListenerObject,
) {
  if (typeof listener === "function") {
    return listener as (event: MessageEvent<string>) => void;
  }

  return (event: MessageEvent<string>) => listener.handleEvent(event);
}

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "Content-Type": "application/json" },
    status,
  });
}

function latestEventSource() {
  const eventSource =
    EventSourceMock.instances[EventSourceMock.instances.length - 1];
  if (!eventSource) {
    throw new Error("EventSource was not created.");
  }
  return eventSource;
}

function stateResponse() {
  return {
    revision: 1,
    codex: {},
    agentReadiness: [],
    preferences: {
      defaultCodexReasoningEffort: "medium",
      defaultClaudeEffort: "default",
    },
    projects: [
      {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      },
    ],
    orchestrators: [],
    workspaces: [],
    sessions: [
      {
        id: "session-1",
        name: "Session 1",
        emoji: "x",
        agent: "Codex",
        workdir: "/projects/termal",
        projectId: "project-termal",
        model: "gpt-5.4",
        approvalPolicy: "never",
        reasoningEffort: "medium",
        sandboxMode: "workspace-write",
        status: "idle",
        preview: "Ready.",
        messages: [],
        pendingPrompts: [],
      },
    ],
  };
}

function storeFilesystemWorkspace(workspaceId: string) {
  window.localStorage.setItem(
    `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceId}`,
    JSON.stringify({
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-files",
        },
        panes: [
          {
            id: "pane-files",
            tabs: [
              {
                id: "tab-files",
                kind: "filesystem",
                rootPath: "/projects/termal",
                originSessionId: "session-1",
                originProjectId: "project-termal",
              },
            ],
            activeTabId: "tab-files",
            activeSessionId: "session-1",
            viewMode: "filesystem",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-files",
      },
    }),
  );
}

describe("App workspaceFilesChanged buffering", () => {
  const originalFetch = globalThis.fetch;
  const originalEventSource = globalThis.EventSource;
  const originalResizeObserver = globalThis.ResizeObserver;
  const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;

  beforeEach(() => {
    observedFileEvents.length = 0;
    EventSourceMock.instances = [];
    window.localStorage.clear();
    window.history.replaceState(
      window.history.state,
      "",
      "/?workspace=file-buffer-test",
    );
    storeFilesystemWorkspace("file-buffer-test");
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(stateResponse());
        }
        if (requestUrl.pathname === "/api/workspaces") {
          return jsonResponse({ workspaces: [] });
        }
        if (requestUrl.pathname.startsWith("/api/workspaces/")) {
          if ((init?.method ?? "GET").toUpperCase() === "PUT") {
            return jsonResponse({
              layout: {
                id: "file-buffer-test",
                revision: 1,
                updatedAt: "2026-04-11 09:00:00",
                controlPanelSide: "left",
                workspace: {
                  root: null,
                  panes: [],
                  activePaneId: null,
                },
              },
            });
          }

          return jsonResponse({}, 404);
        }

        throw new Error(
          `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
        );
      }),
    );
  });

  afterEach(() => {
    cleanup();
    window.localStorage.clear();
    window.history.replaceState(window.history.state, "", originalUrl);
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
    if (originalEventSource === undefined) {
      delete (globalThis as Partial<typeof globalThis>).EventSource;
    } else {
      globalThis.EventSource = originalEventSource;
    }
    if (originalResizeObserver === undefined) {
      delete (globalThis as Partial<typeof globalThis>).ResizeObserver;
    } else {
      globalThis.ResizeObserver = originalResizeObserver;
    }
    vi.unstubAllGlobals();
  });

  it("coalesces same-tick file events and resets stale revision gates on reopen", async () => {
    render(<App />);

    const eventSource = latestEventSource();
    await act(async () => {
      eventSource.dispatchNamedEvent("state", stateResponse());
      await Promise.resolve();
    });

    expect(await screen.findByTestId("filesystem-panel")).toBeInTheDocument();

    await act(async () => {
      eventSource.dispatchNamedEvent("workspaceFilesChanged", {
        revision: 1,
        changes: [
          {
            path: "/projects/termal/src/a.ts",
            kind: "modified",
            rootPath: "/projects/termal",
            sessionId: "session-1",
          },
        ],
      });
      eventSource.dispatchNamedEvent("workspaceFilesChanged", {
        revision: 1,
        changes: [
          {
            path: "/projects/termal/src/b.ts",
            kind: "created",
            rootPath: "/projects/termal",
            sessionId: "session-1",
          },
        ],
      });
      await new Promise((resolve) => window.setTimeout(resolve, 0));
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(observedFileEvents).toHaveLength(1);
    });
    expect(observedFileEvents[0]).toEqual({
      revision: 1,
      changes: [
        {
          path: "/projects/termal/src/a.ts",
          kind: "modified",
          rootPath: "/projects/termal",
          sessionId: "session-1",
        },
        {
          path: "/projects/termal/src/b.ts",
          kind: "created",
          rootPath: "/projects/termal",
          sessionId: "session-1",
        },
      ],
    });

    await act(async () => {
      eventSource.dispatchNamedEvent("workspaceFilesChanged", {
        revision: 0,
        changes: [
          {
            path: "/projects/termal/src/stale.ts",
            kind: "deleted",
            rootPath: "/projects/termal",
            sessionId: "session-1",
          },
        ],
      });
      await new Promise((resolve) => window.setTimeout(resolve, 0));
      await Promise.resolve();
    });

    expect(observedFileEvents).toHaveLength(1);

    await act(async () => {
      eventSource.dispatchOpen();
      eventSource.dispatchNamedEvent("workspaceFilesChanged", {
        revision: 1,
        changes: [
          {
            path: "/projects/termal/src/restarted.ts",
            kind: "modified",
            rootPath: "/projects/termal",
            sessionId: "session-1",
          },
        ],
      });
      await new Promise((resolve) => window.setTimeout(resolve, 0));
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(observedFileEvents).toHaveLength(2);
    });
    expect(observedFileEvents[1]).toEqual({
      revision: 1,
      changes: [
        {
          path: "/projects/termal/src/restarted.ts",
          kind: "modified",
          rootPath: "/projects/termal",
          sessionId: "session-1",
        },
      ],
    });
  });
});
