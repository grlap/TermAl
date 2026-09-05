import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { StateResponse } from "./api";
import App from "./App";
import {
  clickAndSettle,
  flushUiWork,
  makeStateSessionSummary,
} from "./app-test-harness";
import {
  getSessionRecordSnapshotForTesting,
  resetSessionStoreForTesting,
} from "./session-store";
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
      defaultCodexModel: "default",
      defaultClaudeModel: "default",
      defaultCursorModel: "default",
      defaultGeminiModel: "default",
      defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
    },
    projects: [],
    orchestrators: [],
    workspaces: [],
    sessions: [makeStateSessionSummary(session)],
  } as StateResponse;
}

describe("SessionPaneView retry display state", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    resetSessionStoreForTesting();
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    vi.unstubAllGlobals();
    resetSessionStoreForTesting();
    HTMLElement.prototype.scrollTo = originalScrollTo;
  });

  it("passes retry display states through the session renderer as lifecycle changes", async () => {
    const activeSession = makeRetrySession("active");
    const activeState = makeState(activeSession, 1);
    let hydratedSession = activeSession;
    let hydrationRevision = 1;
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const target = String(input);
        if (target === "/api/state") {
          return jsonResponse(activeState);
        }
        if (target.startsWith("/api/sessions/session-1")) {
          return jsonResponse({
            revision: hydrationRevision,
            serverInstanceId: "test-instance",
            session: hydratedSession,
          });
        }
        if (target.startsWith("/api/workspaces/")) {
          if (init?.method === "PUT") {
            return jsonResponse({
              layout: {
                id: target.slice("/api/workspaces/".length),
                revision: 1,
                updatedAt: "2026-07-25 12:00:00",
                controlPanelSide: "left",
                workspace: { lastContentPaneId: null,
                lastViewerPaneId: null,
                activePaneId: null, panes: [], root: null },
              },
            });
          }
          return new Response("", { status: 404 });
        }
        throw new Error(`Unexpected fetch: ${target}`);
      },
    );
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

    await act(async () => {
      eventSource.dispatchOpen();
      eventSource.dispatchState(activeState);
      await flushUiWork();
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

    const resolvedSession = makeResolvedRetrySession();
    const recoveredMessage =
      resolvedSession.messages[resolvedSession.messages.length - 1];
    if (!recoveredMessage) {
      throw new Error("retry fixture missing recovered response");
    }
    act(() => {
      eventSource.dispatchDelta({
        type: "messageCreated",
        revision: 2,
        sessionId: "session-1",
        messageId: recoveredMessage.id,
        messageIndex: 2,
        messageCount: 3,
        message: recoveredMessage,
        preview: resolvedSession.preview,
        status: "idle",
        sessionMutationStamp: 2,
      });
    });

    expect(
      await screen.findByRole("heading", { name: "Connection recovered" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", {
        name: "Reconnecting to continue this turn",
      }),
    ).not.toBeInTheDocument();

    const thirdRetry = {
      ...activeSession.messages[1],
      id: "retry-3",
      timestamp: "10:03",
      text: "Connection dropped before the response finished. Retrying automatically (attempt 3 of 5).",
    };
    act(() => {
      eventSource.dispatchDelta({
        type: "messageCreated",
        revision: 3,
        sessionId: "session-1",
        messageId: thirdRetry.id,
        messageIndex: 3,
        messageCount: 4,
        message: thirdRetry,
        preview: thirdRetry.text,
        status: "active",
        sessionMutationStamp: 3,
      });
    });

    expect(
      await screen.findByRole("heading", {
        name: "Reconnecting to continue this turn",
      }),
    ).toBeInTheDocument();

    const sessionWithNewPrompt = makeRetrySessionWithNewPrompt();
    const newPrompt =
      sessionWithNewPrompt.messages[sessionWithNewPrompt.messages.length - 1];
    if (!newPrompt) {
      throw new Error("retry fixture missing superseding prompt");
    }
    hydratedSession = {
      ...sessionWithNewPrompt,
      messageCount: 5,
      sessionMutationStamp: 4,
      messages: [
        ...activeSession.messages,
        recoveredMessage,
        thirdRetry,
        newPrompt,
      ],
    };
    hydrationRevision = 4;
    await act(async () => {
      eventSource.dispatchState(makeState(hydratedSession, hydrationRevision));
      await flushUiWork();
    });

    await waitFor(() => {
      expect(getSessionRecordSnapshotForTesting("session-1")).toMatchObject({
        messageCount: 5,
        messages: expect.arrayContaining([
          expect.objectContaining({ id: "prompt-after-retry" }),
        ]),
      });
    });

    expect(
      await screen.findByRole("heading", { name: "Connection retry ended" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", {
        name: "Reconnecting to continue this turn",
      }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Connection recovered" }),
    ).toBeInTheDocument();
  });
});
