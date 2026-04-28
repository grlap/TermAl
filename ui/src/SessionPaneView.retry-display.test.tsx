import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { StateResponse } from "./api";
import App from "./App";
import { clickAndSettle } from "./app-test-harness";
import type { Session } from "./types";

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
}

class ResizeObserverMock {
  disconnect() {}

  observe() {}

  unobserve() {}
}

function latestEventSource() {
  const eventSource = EventSourceMock.instances[EventSourceMock.instances.length - 1];
  if (!eventSource) {
    throw new Error("Event source not created");
  }
  return eventSource;
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: {
      "Content-Type": "application/json",
    },
    status: 200,
  });
}

function makeRetrySession(status: Session["status"]): Session {
  return {
    id: "session-1",
    name: "Retry Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "/repo",
    projectId: null,
    model: "gpt-5",
    status,
    preview: "Retrying automatically",
    messagesLoaded: true,
    pendingPrompts: [],
    messages: [
      {
        id: "retry-1",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "Connection dropped before the response finished. Retrying automatically (attempt 1 of 5).",
      },
      {
        id: "retry-2",
        type: "text",
        timestamp: "10:01",
        author: "assistant",
        text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
      },
    ],
  };
}

function makeResolvedRetrySession(): Session {
  const session = makeRetrySession("idle");
  const retryMessage = session.messages[0];
  if (!retryMessage) {
    throw new Error("retry fixture missing first message");
  }
  return {
    ...session,
    preview: "Recovered after retry",
    messages: [
      retryMessage,
      {
        id: "message-recovered",
        type: "text",
        timestamp: "10:02",
        author: "assistant",
        text: "Recovered response.",
      },
    ],
  };
}

function makeRetrySessionWithNewPrompt(): Session {
  const session = makeRetrySession("active");
  return {
    ...session,
    preview: "Try a different task",
    messages: [
      ...session.messages,
      {
        id: "prompt-after-retry",
        type: "text",
        timestamp: "10:02",
        author: "you",
        text: "Try a different task.",
      },
    ],
  };
}

function makeState(session: Session, revision: number): StateResponse {
  return {
    revision,
    serverInstanceId: "test-instance",
    codex: {},
    agentReadiness: [],
    preferences: {
      defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
    },
    projects: [],
    orchestrators: [],
    workspaces: [],
    sessions: [session],
  } as StateResponse;
}

describe("SessionPaneView retry display state", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    HTMLElement.prototype.scrollTo = originalScrollTo;
  });

  it("passes retry display states through the session renderer as lifecycle changes", async () => {
    const activeState = makeState(makeRetrySession("active"), 1);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const target = String(input);
      if (target === "/api/state") {
        return jsonResponse(activeState);
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

    render(<App />);
    const eventSource = latestEventSource();

    act(() => {
      eventSource.dispatchOpen();
      eventSource.dispatchState(activeState);
    });

    await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
    const sessionRowButton = (await screen.findByText("Retry Session")).closest("button");
    if (!sessionRowButton) {
      throw new Error("Retry session row button not found");
    }
    await clickAndSettle(sessionRowButton);

    expect(
      await screen.findByRole("heading", { name: "Retry superseded" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", {
        name: "Reconnecting to continue this turn",
      }),
    ).toBeInTheDocument();

    act(() => {
      eventSource.dispatchState(makeState(makeRetrySessionWithNewPrompt(), 2));
    });

    expect(
      await screen.findByRole("heading", { name: "Connection retry ended" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", {
        name: "Reconnecting to continue this turn",
      }),
    ).not.toBeInTheDocument();

    act(() => {
      eventSource.dispatchState(makeState(makeRetrySession("idle"), 3));
    });

    expect(
      await screen.findByRole("heading", { name: "Connection retry ended" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", {
        name: "Reconnecting to continue this turn",
      }),
    ).not.toBeInTheDocument();

    act(() => {
      eventSource.dispatchState(makeState(makeResolvedRetrySession(), 4));
    });

    expect(
      await screen.findByRole("heading", { name: "Connection recovered" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Connection retry ended" }),
    ).not.toBeInTheDocument();
  });
});
