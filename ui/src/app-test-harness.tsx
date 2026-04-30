// app-test-harness.tsx
//
// Owns: shared frontend-test scaffolding that every App.* test
// file in this directory depends on. That includes the
// EventSource/ResizeObserver stubs, a JSON `Response` shim, the
// act-wrapped UI-settling helpers (`flushUiWork`, `settleAsyncUi`,
// `advanceTimers`, `clickAndSettle`, `submitButtonAndSettle`), the
// `renderApp` / `renderAppWithProjectAndSession` harnesses, the
// fallback-state harness used by the SSE-recovery tests, fixture
// builders for `Session` / `OrchestratorInstance` / `AgentReadiness`
// plus the workspace-layout response, the DnD DataTransfer stand-ins,
// and the scroll/geometry stubs used by the scroll-regression tests.
//
// Does not own: the Monaco editor `vi.mock` calls (those stay in
// the test file(s) that need the mock to apply), test-file-local
// one-off helpers such as single-describe fixtures, and anything
// that is not referenced from more than one test file today or in
// the planned App.test.tsx split.
//
// Split out of: ui/src/App.test.tsx (Slice 1 of the App-split plan,
// see docs/app-split-plan.md).

import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { vi } from "vitest";

import * as api from "./api";
import App from "./App";
import type { AgentReadiness, OrchestratorInstance, Session } from "./types";
import type { WorkspaceState } from "./workspace";

export class EventSourceMock {
  static instances: EventSourceMock[] = [];

  readonly url: string | undefined;

  onerror: ((event: Event) => void) | null = null;

  onopen: ((event: Event) => void) | null = null;

  /**
   * Mirrors the real `EventSource.readyState` (CONNECTING=0, OPEN=1,
   * CLOSED=2). Most tests don't need to set this. Tests that exercise
   * the CLOSED-error recreation path set `2`; tests that exercise the
   * non-OPEN health watchdog set `0`.
   */
  readyState?: number;

  private listeners = new Map<
    string,
    Set<(event: MessageEvent<string>) => void>
  >();

  constructor(url?: string) {
    this.url = url;
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

  dispatchError() {
    this.onerror?.(new Event("error"));
  }

  dispatchOpen() {
    this.onopen?.(new Event("open"));
  }

  dispatchNamedEvent(type: string, data: unknown) {
    const payload = typeof data === "string" ? data : JSON.stringify(data);
    const event = { data: payload } as MessageEvent<string>;
    this.listeners.get(type)?.forEach((listener) => {
      listener(event);
    });
  }
}

export class ResizeObserverMock {
  disconnect() {}

  observe() {}

  unobserve() {}
}

export function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: {
      "Content-Type": "application/json",
    },
    status: 200,
  });
}

export type RestorableGlobalKey = "fetch" | "EventSource" | "ResizeObserver";

export function restoreGlobal<Key extends RestorableGlobalKey>(
  key: Key,
  originalValue: (typeof globalThis)[Key] | undefined,
) {
  if (originalValue === undefined) {
    delete (globalThis as Partial<typeof globalThis>)[key];
    return;
  }

  globalThis[key] = originalValue;
}

export function createActWrappedAnimationFrameMocks() {
  let nextFrameId = 1;
  const callbacks = new Map<number, FrameRequestCallback>();

  function requestAnimationFrameMock(callback: FrameRequestCallback) {
    const frameId = nextFrameId;
    nextFrameId += 1;
    callbacks.set(frameId, callback);
    queueMicrotask(() => {
      const pending = callbacks.get(frameId);
      if (!pending) {
        return;
      }

      callbacks.delete(frameId);
      act(() => {
        pending(Date.now());
      });
    });
    return frameId;
  }

  function cancelAnimationFrameMock(frameId: number) {
    callbacks.delete(frameId);
  }

  return {
    cancelAnimationFrameMock,
    requestAnimationFrameMock,
  };
}
function normalizeMessageEventListener(
  listener: EventListenerOrEventListenerObject,
) {
  if (typeof listener === "function") {
    return listener as (event: MessageEvent<string>) => void;
  }

  return (event: MessageEvent<string>) => listener.handleEvent(event);
}

export function createDeferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

export function makeWorkspaceLayoutResponse(
  overrides: Partial<{
    id: string;
    revision: number;
    updatedAt: string;
    controlPanelSide: "left" | "right";
    workspace: WorkspaceState;
  }> = {},
) {
  return {
    layout: {
      id: "workspace-test",
      revision: 1,
      updatedAt: "2026-03-30 09:00:00",
      controlPanelSide: "left" as const,
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
      ...overrides,
    },
  };
}

export type AppTestStateResponse = Awaited<ReturnType<typeof api.fetchState>>;
export type AppTestStateResponseOverrides = Pick<
  AppTestStateResponse,
  "revision" | "projects" | "orchestrators" | "workspaces" | "sessions"
> &
  Partial<
    Pick<AppTestStateResponse, "codex" | "agentReadiness" | "serverInstanceId"> & {
      preferences: Partial<AppTestStateResponse["preferences"]>;
    }
  >;

export function makeStateResponse(overrides: AppTestStateResponseOverrides): AppTestStateResponse {
  return {
    revision: overrides.revision,
    serverInstanceId: overrides.serverInstanceId ?? "test-instance",
    codex: overrides.codex ?? {},
    agentReadiness: overrides.agentReadiness ?? [],
    preferences: {
      defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
      ...overrides.preferences,
    },
    projects: overrides.projects,
    orchestrators: overrides.orchestrators,
    workspaces: overrides.workspaces,
    sessions: overrides.sessions,
  };
}

export async function flushUiWork() {
  for (let iteration = 0; iteration < 3; iteration += 1) {
    await Promise.resolve();
    await Promise.resolve();
    if (vi.isFakeTimers()) {
      await vi.advanceTimersByTimeAsync(1);
      continue;
    }

    await new Promise((resolve) => window.setTimeout(resolve, 0));
  }
  await Promise.resolve();
  await Promise.resolve();
}

export async function settleAsyncUi() {
  await act(async () => {
    await flushUiWork();
  });
}

export async function advanceTimers(durationMs: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(durationMs);
  });
}

export async function renderApp() {
  await act(async () => {
    render(<App />);
  });
  await settleAsyncUi();
}

export function latestEventSource(): EventSourceMock {
  const eventSource =
    EventSourceMock.instances[EventSourceMock.instances.length - 1];
  if (!eventSource) {
    throw new Error("Event source not created");
  }
  return eventSource;
}

export function stubScrollIntoView() {
  return vi
    .spyOn(HTMLElement.prototype, "scrollIntoView")
    .mockImplementation(() => {});
}

export function mockScrollToAndApplyTop() {
  const scrollToMock =
    HTMLElement.prototype.scrollTo as unknown as ReturnType<typeof vi.fn>;
  scrollToMock.mockImplementation(function (
    this: HTMLElement,
    options?: ScrollToOptions | number,
    y?: number,
  ) {
    if (
      typeof options === "object" &&
      options !== null &&
      typeof options.top === "number"
    ) {
      this.scrollTop = options.top;
      return;
    }

    if (typeof options === "number" && typeof y === "number") {
      this.scrollTop = y;
    }
  });
  return scrollToMock;
}

export function filterScrollToCallsAt(
  scrollToMock: ReturnType<typeof vi.fn>,
  top: number,
  behavior: ScrollBehavior,
) {
  return scrollToMock.mock.calls.filter((call) => {
    const arg = call[0];
    return (
      typeof arg === "object" &&
      arg !== null &&
      (arg as ScrollToOptions).top === top &&
      (arg as ScrollToOptions).behavior === behavior
    );
  });
}

export function stubElementScrollGeometry({
  clientHeight,
  scrollHeight,
}: {
  clientHeight: number | (() => number);
  scrollHeight: number | (() => number);
}) {
  const originalScrollHeight = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    "scrollHeight",
  );
  const originalClientHeight = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    "clientHeight",
  );
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
    configurable: true,
    get() {
      return typeof scrollHeight === "function" ? scrollHeight() : scrollHeight;
    },
  });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get() {
      return typeof clientHeight === "function" ? clientHeight() : clientHeight;
    },
  });

  return () => {
    if (originalScrollHeight) {
      Object.defineProperty(
        HTMLElement.prototype,
        "scrollHeight",
        originalScrollHeight,
      );
    } else {
      delete (HTMLElement.prototype as unknown as Record<string, unknown>)
        .scrollHeight;
    }
    if (originalClientHeight) {
      Object.defineProperty(
        HTMLElement.prototype,
        "clientHeight",
        originalClientHeight,
      );
    } else {
      delete (HTMLElement.prototype as unknown as Record<string, unknown>)
        .clientHeight;
    }
  };
}

export function setDocumentVisibilityState(value: DocumentVisibilityState) {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value,
  });
}

export async function clickAndSettle(target: HTMLElement) {
  await act(async () => {
    fireEvent.click(target);
  });
  await settleAsyncUi();
}

export async function submitButtonAndSettle(target: HTMLElement) {
  const form = target.closest("form");
  if (!form) {
    throw new Error("Submit target is not inside a form.");
  }

  await act(async () => {
    fireEvent.submit(form);
  });
  await settleAsyncUi();
}

// React still warns for detached async handlers kicked off by these integration
// flows. Keep any suppression local to the specific tests that exercise them.
export async function withSuppressedActWarnings<T>(run: () => Promise<T>) {
  const originalConsoleError = console.error;
  const consoleErrorSpy = vi
    .spyOn(console, "error")
    .mockImplementation((message?: unknown, ...args: unknown[]) => {
      if (
        typeof message === "string" &&
        message.includes("not wrapped in act")
      ) {
        return;
      }

      originalConsoleError.call(console, message, ...args);
    });

  try {
    return await run();
  } finally {
    consoleErrorSpy.mockRestore();
  }
}


export type FallbackStateTestContext = {
  eventSource: EventSourceMock;
  sessionList: HTMLDivElement;
};

export async function dispatchStateEvent(eventSource: EventSourceMock, state: unknown) {
  await act(async () => {
    eventSource.dispatchNamedEvent("state", state);
    await flushUiWork();
  });
}

export async function dispatchOpenedStateEvent(
  eventSource: EventSourceMock,
  state: unknown,
) {
  await act(async () => {
    eventSource.dispatchOpen();
    eventSource.dispatchNamedEvent("state", state);
    await flushUiWork();
  });
}

export async function withFallbackStateHarness<T>(
  run: (context: FallbackStateTestContext) => Promise<T>,
) {
  const originalEventSource = globalThis.EventSource;
  const originalResizeObserver = globalThis.ResizeObserver;
  vi.stubGlobal(
    "EventSource",
    EventSourceMock as unknown as typeof EventSource,
  );
  vi.stubGlobal(
    "ResizeObserver",
    ResizeObserverMock as unknown as typeof ResizeObserver,
  );
  const scrollIntoViewSpy = stubScrollIntoView();

  try {
    await renderApp();
    await clickAndSettle(
      await screen.findByRole("button", { name: "Sessions" }),
    );
    const sessionList = document.querySelector(".session-list");
    if (!(sessionList instanceof HTMLDivElement)) {
      throw new Error("Session list not found");
    }

    return await run({
      eventSource: latestEventSource(),
      sessionList,
    });
  } finally {
    scrollIntoViewSpy.mockRestore();
    restoreGlobal("EventSource", originalEventSource);
    restoreGlobal("ResizeObserver", originalResizeObserver);
  }
}

export async function openCreateSessionDialog() {
  await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
  const [newButton] = await screen.findAllByRole("button", { name: "New" });
  if (!newButton) {
    throw new Error("New session button not found.");
  }
  await clickAndSettle(newButton);
  await screen.findByRole("heading", { level: 2, name: "New session" });
}

export async function selectComboboxOption(name: string, optionName: string | RegExp) {
  const combobox = await screen.findByRole("combobox", { name });
  await clickAndSettle(combobox);

  const listbox = await screen.findByRole("listbox");
  const option = within(listbox)
    .getAllByRole("option")
    .find((candidate) => {
      const label =
        candidate.querySelector(".combo-option-label")?.textContent?.trim() ??
        candidate.textContent?.trim() ??
        "";

      return typeof optionName === "string"
        ? label === optionName
        : optionName.test(label);
    });

  if (!option) {
    throw new Error(`Combobox option not found for ${String(optionName)}`);
  }

  await clickAndSettle(option);
}

export function createDragDataTransfer() {
  const data = new Map<string, string>();
  return {
    dropEffect: "move",
    effectAllowed: "all",
    getData: (format: string) => data.get(format) ?? "",
    setData: (format: string, value: string) => {
      data.set(format, value);
    },
    setDragImage: () => {},
    get types() {
      return Array.from(data.keys());
    },
  };
}

export function createReducedMimeDragDataTransfer(
  dataTransfer: ReturnType<typeof createDragDataTransfer>,
) {
  return {
    dropEffect: dataTransfer.dropEffect,
    effectAllowed: dataTransfer.effectAllowed,
    getData: dataTransfer.getData,
    setData: dataTransfer.setData,
    setDragImage: dataTransfer.setDragImage,
    get types() {
      return ["text/plain"];
    },
  };
}

export type RenderAppWithProjectAndSessionOptions = {
  includeGitStatus?: boolean;
  includeWorkspacePersistence?: boolean;
};

export async function renderAppWithProjectAndSession(
  options: RenderAppWithProjectAndSessionOptions = {},
) {
  const { includeGitStatus = false, includeWorkspacePersistence = false } =
    options;
  const originalFetch = globalThis.fetch;
  const originalEventSource = globalThis.EventSource;
  const originalResizeObserver = globalThis.ResizeObserver;
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const requestUrl = new URL(String(input), "http://localhost");
      if (requestUrl.pathname === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
            }),
          ],
        });
      }

      if (includeGitStatus && requestUrl.pathname === "/api/git/status") {
        return jsonResponse({
          ahead: 0,
          behind: 0,
          branch: "main",
          files: [],
          isClean: true,
          repoRoot: "/projects/termal",
          upstream: "origin/main",
          workdir: "/projects/termal",
        });
      }

      if (
        includeWorkspacePersistence &&
        requestUrl.pathname.startsWith("/api/workspaces/")
      ) {
        if ((init?.method ?? "GET").toUpperCase() === "PUT") {
          return jsonResponse({ ok: true });
        }

        return new Response("", { status: 404 });
      }

      throw new Error(
        `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
      );
    },
  );

  const priorEventSourceCount = EventSourceMock.instances.length;

  window.localStorage.clear();
  vi.stubGlobal("fetch", fetchMock);
  vi.stubGlobal(
    "EventSource",
    EventSourceMock as unknown as typeof EventSource,
  );
  vi.stubGlobal(
    "ResizeObserver",
    ResizeObserverMock as unknown as typeof ResizeObserver,
  );
  const scrollIntoViewSpy = vi
    .spyOn(HTMLElement.prototype, "scrollIntoView")
    .mockImplementation(() => {});

  function restoreSetup() {
    window.localStorage.clear();
    EventSourceMock.instances.splice(priorEventSourceCount);
    scrollIntoViewSpy.mockRestore();
    restoreGlobal("fetch", originalFetch);
    restoreGlobal("EventSource", originalEventSource);
    restoreGlobal("ResizeObserver", originalResizeObserver);
  }

  try {
    await renderApp();
    const eventSource = EventSourceMock.instances[priorEventSourceCount];
    if (!eventSource) {
      throw new Error("Event source not created");
    }
    await act(async () => {
      eventSource.dispatchError();
    });
    await settleAsyncUi();

    const sessionList = document.querySelector(".session-list");
    if (!(sessionList instanceof HTMLDivElement)) {
      throw new Error("Session list not found");
    }

    const sessionRowLabel = await within(sessionList).findByText("Session 1");
    const sessionRowButton = sessionRowLabel.closest("button");
    if (!sessionRowButton) {
      throw new Error("Session row button not found");
    }

    await clickAndSettle(sessionRowButton);

    return {
      fetchMock,
      cleanup: restoreSetup,
    };
  } catch (error) {
    restoreSetup();
    throw error;
  }
}
export function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.4",
    approvalPolicy: "never",
    reasoningEffort: "medium",
    sandboxMode: "workspace-write",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

export function makeOrchestrator(
  overrides: Partial<OrchestratorInstance> = {},
): OrchestratorInstance {
  return {
    id: "orchestrator-1",
    templateId: "template-1",
    projectId: "project-local",
    templateSnapshot: {
      id: "template-1",
      name: "Runtime Flow",
      description: "Handle orchestration work.",
      createdAt: "2026-03-30 09:00:00",
      updatedAt: "2026-03-30 09:05:00",
      projectId: "project-local",
      sessions: [
        {
          id: "builder",
          name: "Builder",
          agent: "Codex",
          model: null,
          instructions: "Implement the queued work.",
          autoApprove: true,
          inputMode: "queue",
          position: { x: 220, y: 420 },
        },
      ],
      transitions: [],
    },
    status: "running",
    sessionInstances: [
      {
        templateSessionId: "builder",
        sessionId: "session-1",
        lastCompletionRevision: null,
        lastDeliveredCompletionRevision: null,
      },
    ],
    pendingTransitions: [],
    createdAt: "2026-03-30 09:06:00",
    completedAt: null,
    errorMessage: null,
    ...overrides,
  };
}

export function makeReadiness(overrides?: Partial<AgentReadiness>): AgentReadiness {
  return {
    agent: "Gemini",
    status: "needsSetup",
    blocking: true,
    detail: "Gemini CLI needs auth before TermAl can create sessions.",
    commandPath: "/usr/local/bin/gemini",
    ...overrides,
  };
}
