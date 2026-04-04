import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import App, {
  MarkdownContent,
  ThemedCombobox,
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  getWorkspaceSplitResizeBounds,
  resolveControlPanelWorkspaceRoot,
  resolveStandaloneControlPanelDockWidthRatio,
  describeUnknownSessionModelWarning,
  resolveUnknownSessionModelSendAttempt,
} from "./App";
import {
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
} from "./live-updates";
import type { AgentReadiness, OrchestratorInstance, Session } from "./types";

class EventSourceMock {
  static instances: EventSourceMock[] = [];

  readonly url: string | undefined;

  onerror: ((event: Event) => void) | null = null;

  onopen: ((event: Event) => void) | null = null;

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

type RestorableGlobalKey = "fetch" | "EventSource" | "ResizeObserver";

function restoreGlobal<Key extends RestorableGlobalKey>(
  key: Key,
  originalValue: (typeof globalThis)[Key] | undefined,
) {
  if (originalValue === undefined) {
    delete (globalThis as Partial<typeof globalThis>)[key];
    return;
  }

  globalThis[key] = originalValue;
}

function createActWrappedAnimationFrameMocks() {
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

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function makeWorkspaceLayoutResponse(
  overrides: Partial<{
    id: string;
    revision: number;
    updatedAt: string;
    controlPanelSide: "left" | "right";
    workspace: {
      root: null;
      panes: never[];
      activePaneId: null;
    };
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

async function flushUiWork() {
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

async function settleAsyncUi() {
  await act(async () => {
    await flushUiWork();
  });
}

async function advanceTimers(durationMs: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(durationMs);
  });
}

async function renderApp() {
  await act(async () => {
    render(<App />);
  });
  await settleAsyncUi();
}

function latestEventSource(): EventSourceMock {
  const eventSource =
    EventSourceMock.instances[EventSourceMock.instances.length - 1];
  if (!eventSource) {
    throw new Error("Event source not created");
  }
  return eventSource;
}

function setDocumentVisibilityState(value: DocumentVisibilityState) {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value,
  });
}

async function clickAndSettle(target: HTMLElement) {
  await act(async () => {
    fireEvent.click(target);
  });
  await settleAsyncUi();
}

async function submitButtonAndSettle(target: HTMLElement) {
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
async function withSuppressedActWarnings<T>(run: () => Promise<T>) {
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


type FallbackStateTestContext = {
  eventSource: EventSourceMock;
  sessionList: HTMLDivElement;
};

async function dispatchStateEvent(eventSource: EventSourceMock, state: unknown) {
  await act(async () => {
    eventSource.dispatchNamedEvent("state", state);
    await flushUiWork();
  });
}

async function dispatchOpenedStateEvent(
  eventSource: EventSourceMock,
  state: unknown,
) {
  await act(async () => {
    eventSource.dispatchOpen();
    eventSource.dispatchNamedEvent("state", state);
    await flushUiWork();
  });
}

async function withFallbackStateHarness<T>(
  run: (context: FallbackStateTestContext) => Promise<T>,
) {
  const originalEventSource = globalThis.EventSource;
  const originalResizeObserver = globalThis.ResizeObserver;
  const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
  vi.stubGlobal(
    "EventSource",
    EventSourceMock as unknown as typeof EventSource,
  );
  vi.stubGlobal(
    "ResizeObserver",
    ResizeObserverMock as unknown as typeof ResizeObserver,
  );
  HTMLElement.prototype.scrollIntoView = vi.fn();

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
    HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    restoreGlobal("EventSource", originalEventSource);
    restoreGlobal("ResizeObserver", originalResizeObserver);
  }
}

async function openCreateSessionDialog() {
  await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
  const [newButton] = await screen.findAllByRole("button", { name: "New" });
  if (!newButton) {
    throw new Error("New session button not found.");
  }
  await clickAndSettle(newButton);
  await screen.findByRole("heading", { level: 2, name: "New session" });
}

async function selectComboboxOption(name: string, optionName: string | RegExp) {
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

function createDragDataTransfer() {
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

function createReducedMimeDragDataTransfer(
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

type RenderAppWithProjectAndSessionOptions = {
  includeGitStatus?: boolean;
  includeWorkspacePersistence?: boolean;
};

async function renderAppWithProjectAndSession(
  options: RenderAppWithProjectAndSessionOptions = {},
) {
  const { includeGitStatus = false, includeWorkspacePersistence = false } =
    options;
  const originalFetch = globalThis.fetch;
  const originalEventSource = globalThis.EventSource;
  const originalResizeObserver = globalThis.ResizeObserver;
  const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
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

  function restoreSetup() {
    window.localStorage.clear();
    EventSourceMock.instances.splice(priorEventSourceCount);
    HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    restoreGlobal("fetch", originalFetch);
    restoreGlobal("EventSource", originalEventSource);
    restoreGlobal("ResizeObserver", originalResizeObserver);
  }

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
  HTMLElement.prototype.scrollIntoView = vi.fn();

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
function makeSession(id: string, overrides?: Partial<Session>): Session {
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

function makeOrchestrator(
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

function makeReadiness(overrides?: Partial<AgentReadiness>): AgentReadiness {
  return {
    agent: "Gemini",
    status: "needsSetup",
    blocking: true,
    detail: "Gemini CLI needs auth before TermAl can create sessions.",
    commandPath: "/usr/local/bin/gemini",
    ...overrides,
  };
}

describe("MarkdownContent", () => {
  it("wraps markdown tables in a scroll container", () => {
    const markdown = [
      "| Finding | Resolution |",
      "| --- | --- |",
      "| `skip_list.rs` | Fixed |",
    ].join("\n");

    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    try {
      const { container } = render(<MarkdownContent markdown={markdown} />);

      const tableScroll = container.querySelector(".markdown-table-scroll");
      expect(tableScroll).not.toBeNull();
      expect(tableScroll?.querySelector("table")).not.toBeNull();
      expect(consoleError).not.toHaveBeenCalled();
    } finally {
      consoleError.mockRestore();
    }
  });

  it("opens local file links through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[experience.tex#L63](experience.tex#L63)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("opens absolute Windows file links through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[route_post_processing_service.dart:469](C:/github/Personal/fit_friends/lib/services/route_post_processing_service.dart#L469)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/TermAl"
      />,
    );

    fireEvent.click(
      screen.getByRole("link", {
        name: "route_post_processing_service.dart:469",
      }),
    );

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/fit_friends/lib/services/route_post_processing_service.dart",
      line: 469,
      openInNewTab: false,
    });
  });

  it("opens absolute Linux file links through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[route_post_processing_service.dart:469](/home/grzeg/projects/fit_friends/lib/services/route_post_processing_service.dart#L469)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(
      screen.getByRole("link", {
        name: "route_post_processing_service.dart:469",
      }),
    );

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/home/grzeg/projects/fit_friends/lib/services/route_post_processing_service.dart",
      line: 469,
      openInNewTab: false,
    });
  });

  it("opens localhost app file URLs through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[20260322000004_child_provisioning_rpcs.sql](http://127.0.0.1:4173/C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql#L15C1)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/questly"
      />,
    );

    const link = screen.getByRole("link", {
      name: "20260322000004_child_provisioning_rpcs.sql",
    });
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql",
      line: 15,
      column: 1,
      openInNewTab: false,
    });
  });

  it("renders bare localhost app file URLs with workspace-relative labels", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="http://127.0.0.1:4173/C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql#L15C1"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/questly"
      />,
    );

    const link = screen.getByRole("link", {
      name: "supabase/migrations/20260322000004_child_provisioning_rpcs.sql#L15C1",
    });
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql",
      line: 15,
      column: 1,
      openInNewTab: false,
    });
  });

  it("opens localhost Unix file URLs through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[service.rs](http://127.0.0.1:4173/home/grzeg/projects/fit_friends/src/service.rs#L12)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/home/grzeg/projects/fit_friends"
      />,
    );

    const link = screen.getByRole("link", { name: "service.rs" });
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/home/grzeg/projects/fit_friends/src/service.rs",
      line: 12,
      openInNewTab: false,
    });
  });
  it("keeps same-origin docs URLs as normal external links", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="http://localhost/docs/architecture.md"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const link = screen.getByRole("link", {
      name: "http://localhost/docs/architecture.md",
    });
    expect(link).toHaveAttribute("target", "_blank");

    fireEvent.click(link);

    expect(onOpenSourceLink).not.toHaveBeenCalled();
  });

  it("autolinks bare file references with line targets", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="The Microsoft scope bullet needs more evidence in experience.tex#L63."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("autolinks bare file references with dotted line targets", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="The Microsoft scope bullet needs more evidence in experience.tex.#L63."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex.#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("opens inline code file references through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay clickable."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex.#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });
});

describe("App", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;
  const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
  const originalCancelAnimationFrame = globalThis.cancelAnimationFrame;

  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createActWrappedAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({
      workspaces: [],
    });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(
      makeWorkspaceLayoutResponse(),
    );
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    if (originalRequestAnimationFrame === undefined) {
      delete (globalThis as Partial<typeof globalThis>).requestAnimationFrame;
    } else {
      globalThis.requestAnimationFrame = originalRequestAnimationFrame;
    }
    if (originalCancelAnimationFrame === undefined) {
      delete (globalThis as Partial<typeof globalThis>).cancelAnimationFrame;
    } else {
      globalThis.cancelAnimationFrame = originalCancelAnimationFrame;
    }
    window.localStorage.clear();
    if (vi.isFakeTimers()) {
      vi.useRealTimers();
    }
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("restores helper setup globals when renderAppWithProjectAndSession fails", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const originalQuerySelector = Document.prototype.querySelector;
      const querySelectorSpy = vi
        .spyOn(Document.prototype, "querySelector")
        .mockImplementation(function (this: Document, selectors: string) {
          if (selectors === ".session-list") {
            return null;
          }

          return originalQuerySelector.call(this, selectors);
        });

      const originalScrollIntoViewBeforeFailure =
        HTMLElement.prototype.scrollIntoView;

      try {
        await expect(renderAppWithProjectAndSession()).rejects.toThrow(
          "Session list not found",
        );
        expect(globalThis.fetch).toBe(originalFetch);
        expect(globalThis.EventSource).toBe(originalEventSource);
        expect(globalThis.ResizeObserver).toBe(originalResizeObserver);
        expect(HTMLElement.prototype.scrollIntoView).toBe(
          originalScrollIntoViewBeforeFailure,
        );
      } finally {
        querySelectorSpy.mockRestore();
      }
    });
  });

  it("uses the freshly rendered EventSource when prior mock instances exist", async () => {
    await withSuppressedActWarnings(async () => {
      const staleDispatchError = vi.fn(() => {
        throw new Error("stale EventSource should not be reused");
      });
      const priorEventSourceCount = EventSourceMock.instances.length;
      const seededEventSourceCount = (EventSourceMock.instances = [
        ...EventSourceMock.instances,
        {
          dispatchError: staleDispatchError,
        } as unknown as EventSourceMock,
      ]).length;

      const context = await renderAppWithProjectAndSession();
      try {
        const freshEventSources = EventSourceMock.instances.slice(
          seededEventSourceCount,
        );
        expect(EventSourceMock.instances.length).toBeGreaterThan(
          seededEventSourceCount,
        );
        expect(
          freshEventSources.some(
            (eventSource) => eventSource.url?.includes("/api/events") ?? false,
          ),
        ).toBe(true);
        expect(staleDispatchError).not.toHaveBeenCalled();
      } finally {
        context.cleanup();
        EventSourceMock.instances.splice(priorEventSourceCount);
      }
    });
  });

  it("applies the active combobox option on space without closing the menu", () => {
    const onChange = vi.fn();
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(() => {});

    try {
      render(
        <ThemedCombobox
          id="test-combobox"
          value="gpt-5"
          options={[
            { label: "GPT-5", value: "gpt-5" },
            { label: "GPT-5 mini", value: "gpt-5-mini" },
          ]}
          onChange={onChange}
        />,
      );

      fireEvent.click(screen.getByRole("combobox"));
      fireEvent.keyDown(window, { key: "ArrowDown" });
      fireEvent.keyDown(window, { key: " " });

      expect(onChange).toHaveBeenCalledTimes(1);
      expect(onChange).toHaveBeenCalledWith("gpt-5-mini");
      expect(screen.getByRole("listbox")).toBeInTheDocument();

      fireEvent.keyDown(window, { key: "Enter" });

      expect(onChange).toHaveBeenCalledTimes(2);
      expect(onChange).toHaveBeenLastCalledWith("gpt-5-mini");
      expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    } finally {
      scrollIntoViewSpy.mockRestore();
    }
  });

  it("scrolls an off-screen combobox selection into view when the menu opens", async () => {
    const originalGetBoundingClientRect =
      HTMLElement.prototype.getBoundingClientRect;
    HTMLElement.prototype.getBoundingClientRect = function () {
      if (this.classList.contains("combo-menu")) {
        return {
          bottom: 90,
          height: 90,
          left: 0,
          right: 240,
          top: 0,
          width: 240,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }

      const optionIndex = this.getAttribute("data-option-index");
      if (optionIndex !== null) {
        const index = Number(optionIndex);
        const listbox = this.parentElement as HTMLElement | null;
        const top = index * 30 - (listbox?.scrollTop ?? 0);

        return {
          bottom: top + 30,
          height: 30,
          left: 0,
          right: 240,
          top,
          width: 240,
          x: 0,
          y: top,
          toJSON: () => ({}),
        } as DOMRect;
      }

      return originalGetBoundingClientRect.call(this);
    };

    try {
      render(
        <ThemedCombobox
          id="overflow-combobox"
          value="model-7"
          options={Array.from({ length: 8 }, (_, index) => ({
            label: `Model ${index}`,
            value: `model-${index}`,
          }))}
          onChange={vi.fn()}
        />,
      );

      fireEvent.click(screen.getByRole("combobox"));

      const listbox = await screen.findByRole("listbox");
      await waitFor(() => {
        expect(listbox.scrollTop).toBe(150);
      });
    } finally {
      HTMLElement.prototype.getBoundingClientRect =
        originalGetBoundingClientRect;
    }
  });

  it("describes when a Codex model switch resets reasoning effort", () => {
    expect(
      describeCodexModelAdjustmentNotice(
        makeSession("before", {
          model: "gpt-5",
          reasoningEffort: "minimal",
          modelOptions: [
            {
              label: "GPT-5",
              value: "gpt-5",
              supportedReasoningEfforts: ["minimal", "low", "medium", "high"],
            },
          ],
        }),
        makeSession("after", {
          model: "gpt-5-codex-mini",
          reasoningEffort: "medium",
          modelOptions: [
            {
              label: "GPT-5 Codex Mini",
              value: "gpt-5-codex-mini",
              supportedReasoningEfforts: ["medium", "high"],
            },
          ],
        }),
      ),
    ).toBe(
      "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
    );
  });

  it("derives control panel workspace roots only from the active workspace or a local project", () => {
    expect(resolveControlPanelWorkspaceRoot(null, null)).toBeNull();
    expect(resolveControlPanelWorkspaceRoot(null, "")).toBeNull();
    expect(resolveControlPanelWorkspaceRoot(null, "   ")).toBeNull();
    expect(
      resolveControlPanelWorkspaceRoot(null, "  /workspace/current  "),
    ).toBe("/workspace/current");
    expect(
      resolveControlPanelWorkspaceRoot(
        {
          id: "project-api",
          name: "API",
          rootPath: "/projects/api",
        },
        null,
      ),
    ).toBe("/projects/api");
    expect(
      resolveControlPanelWorkspaceRoot(
        {
          id: "project-remote",
          name: "Remote",
          rootPath: "/remote/repo",
          remoteId: "ssh-lab",
        },
        "/workspace/current",
      ),
    ).toBeNull();
  });

  it("rewrites model refresh failures into agent-specific guidance", () => {
    expect(
      describeSessionModelRefreshError(
        "Gemini",
        "failed to refresh Gemini model options: auth missing",
        makeReadiness(),
      ),
    ).toBe("Gemini CLI needs auth before TermAl can create sessions.");
    expect(
      describeSessionModelRefreshError(
        "Claude",
        "timed out refreshing Claude model options",
      ),
    ).toBe(
      "Claude did not return its live model list in time. Try Refresh models again. If this keeps happening, start a new Claude session.",
    );
  });

  it("warns before sending a prompt with an unknown session model", () => {
    expect(
      describeUnknownSessionModelWarning(
        makeSession("unknown-model", {
          agent: "Codex",
          model: "gpt-5.5-preview",
          modelOptions: [{ label: "GPT-5.4", value: "gpt-5.4" }],
        }),
      ),
    ).toBe(
      "Codex is set to gpt-5.5-preview, but that model is not in the current live list. Refresh models to verify it, or send the prompt again to continue anyway.",
    );
  });

  it("keeps newer SSE state when a reconnect resync returns an older snapshot", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const stateFetch = createDeferred<Response>();
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return stateFetch.promise;
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      const eventSource = latestEventSource();
      expect(eventSource).toBeTruthy();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Newer Session",
              preview: "Fresh preview",
              status: "active",
            }),
          ],
        });
      });

      await screen.findByText("Newer Session");

      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "messageCreated",
          revision: 4,
          sessionId: "session-1",
          messageId: "message-2",
          messageIndex: 1,
          message: {
            id: "message-2",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "",
          },
          preview: "Should trigger resync",
          status: "active",
        });
      });

      await waitFor(() => {
        expect(
          fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
        ).toBe(true);
      });

      await act(async () => {
        stateFetch.resolve(
          jsonResponse({
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Older Session",
                preview: "Stale preview",
              }),
            ],
          }),
        );
        await flushUiWork();
      });

      await waitFor(() => {
        expect(screen.getByText("Newer Session")).toBeInTheDocument();
      });
      await settleAsyncUi();
      expect(screen.queryByText("Older Session")).not.toBeInTheDocument();
      expect(screen.queryByText("Stale preview")).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("does not roll back to an older snapshot when a delta-gap resync queues behind an in-flight reconnect resync", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const firstStateFetch = createDeferred<Response>();
    vi.useFakeTimers();
    let stateRequestCount = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return firstStateFetch.promise;
        }
        if (stateRequestCount === 2) {
          return Promise.resolve(
            jsonResponse({
              revision: 2,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Codex Session",
                  status: "idle",
                  preview: "Older snapshot.",
                  messages: [
                    {
                      id: "message-user-1",
                      type: "text",
                      timestamp: "10:00",
                      author: "you",
                      text: "test",
                    },
                    {
                      id: "message-assistant-1",
                      type: "text",
                      timestamp: "10:01",
                      author: "assistant",
                      text: "Older snapshot.",
                    },
                  ],
                }),
              ],
            }),
          );
        }
        throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;

    try {
      await renderApp();

      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      act(() => {
        eventSource.dispatchError();
      });
      await advanceTimers(400);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "messageCreated",
          revision: 4,
        });
      });
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      await act(async () => {
        firstStateFetch.resolve(
          jsonResponse({
            revision: 3,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "idle",
                preview: "Recovered current.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                  {
                    id: "message-assistant-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Recovered current.",
                  },
                ],
              }),
            ],
          }),
        );
        await flushUiWork();
      });

      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(2);

      expect(screen.getAllByText("Recovered current.")).toHaveLength(2);
      expect(screen.queryByText("Older snapshot.")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("restarts the resync loop from finally when a reconnect fallback queues behind a failing pre-reopen resync", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let rejectFirstStateFetch!: (reason?: unknown) => void;
    const firstStateFetch = new Promise<Response>((_resolve, reject) => {
      rejectFirstStateFetch = reject;
    });
    vi.useFakeTimers();
    let stateRequestCount = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return firstStateFetch;
        }
        if (stateRequestCount === 2) {
          return Promise.resolve(
            jsonResponse({
              // Intentionally below SSE revision 2: allowAuthoritativeRollback
              // permits rollback when no newer SSE state arrived mid-fetch.
              revision: 1,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered queued reconnect session",
                  status: "idle",
                  preview: "Recovered after queued reconnect fallback.",
                  messages: [
                    {
                      id: "message-user-1",
                      type: "text",
                      timestamp: "10:00",
                      author: "you",
                      text: "test",
                    },
                    {
                      id: "message-assistant-1",
                      type: "text",
                      timestamp: "10:01",
                      author: "assistant",
                      text: "Recovered after queued reconnect fallback.",
                    },
                  ],
                }),
              ],
            }),
          );
        }

        throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      expect(stateRequestCount).toBe(0);

      act(() => {
        eventSource.dispatchError();
      });
      await advanceTimers(100);
      await settleAsyncUi();

      act(() => {
        // Intentionally omit dispatchOpen(): the pre-reopen gap delta should trigger
        // a resync while the 400 ms reconnect fallback remains armed behind it.
        eventSource.dispatchNamedEvent("delta", {
          type: "messageCreated",
          revision: 4,
          sessionId: "session-1",
          messageId: "message-assistant-1",
          messageIndex: 1,
          message: {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "",
          },
          preview: "Buffered preview",
          status: "active",
        });
      });
      await settleAsyncUi();

      expect(stateRequestCount).toBe(1);

      await advanceTimers(299);
      await settleAsyncUi();
      expect(stateRequestCount).toBe(1);

      await advanceTimers(1);
      await settleAsyncUi();
      // The 400 ms timer fires here but defers because the first fetch is still
      // in flight; the queued follow-up stays pending without starting fetch #2 yet.
      expect(stateRequestCount).toBe(1);

      await act(async () => {
        rejectFirstStateFetch(new Error("temporary outage"));
        await flushUiWork();
      });

      await settleAsyncUi();
      expect(stateRequestCount).toBe(2);
      expect(
        screen.getAllByText("Recovered after queued reconnect fallback."),
      ).toHaveLength(2);
      expect(
        screen.getAllByText("Recovered queued reconnect session"),
      ).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("retains rollback permission when a plain resync queues after a stronger reconnect resync", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const firstStateFetch = createDeferred<Response>();
    vi.useFakeTimers();
    let stateRequestCount = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return firstStateFetch.promise;
        }
        if (stateRequestCount === 2) {
          return Promise.resolve(
            jsonResponse({
              revision: 3,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Codex Session",
                  status: "idle",
                  preview: "Recovered after coalesced reconnect.",
                  messages: [
                    {
                      id: "message-user-1",
                      type: "text",
                      timestamp: "10:00",
                      author: "you",
                      text: "test",
                    },
                    {
                      id: "message-assistant-1",
                      type: "text",
                      timestamp: "10:01",
                      author: "assistant",
                      text: "Recovered after coalesced reconnect.",
                    },
                  ],
                }),
              ],
            }),
          );
        }
        throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;

    try {
      await renderApp();

      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "messageCreated",
          revision: 4,
        });
      });
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      act(() => {
        eventSource.dispatchError();
      });
      await advanceTimers(400);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      // A later plain resync must not clear the queued reconnect rollback request.
      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "messageCreated",
          revision: 5,
        });
      });
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      await act(async () => {
        firstStateFetch.resolve(
          jsonResponse({
            revision: 3,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Still syncing.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                ],
              }),
            ],
          }),
        );
        await flushUiWork();
      });

      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(2);
      expect(
        screen.getAllByText("Recovered after coalesced reconnect."),
      ).toHaveLength(2);
      expect(screen.queryByText("Still syncing.")).not.toBeInTheDocument();
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
  it("resyncs after a post-hydration stream error so completed replies do not stay hidden", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      throw new Error(`Unexpected fetch: ${String(input)}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    try {
      await renderApp();
      const eventSource = latestEventSource();
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Sessions" }),
      );
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel =
        await within(sessionList).findByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      await screen.findByText("Waiting for the next chunk of output...");
      act(() => {
        eventSource.dispatchError();
      });
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here.",
                },
              ],
            }),
          ],
        });
      });
      await waitFor(() => {
        expect(screen.getAllByText("Here.")).toHaveLength(2);
      });
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
      ).toBe(false);
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resyncs marked fallback state events instead of adopting their empty snapshot", async () => {
    await withSuppressedActWarnings(async () => {
      const resyncStateDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            return resyncStateDeferred.promise;
          }

          throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
        });

      try {
        await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
          await dispatchOpenedStateEvent(eventSource, {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Stale Session",
                preview: "Original preview",
              }),
            ],
          });
          await within(sessionList).findByText("Stale Session");

          await dispatchStateEvent(eventSource, {
            _sseFallback: true,
            revision: 2,
            projects: [],
            sessions: [],
          });

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Stale Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Original preview"),
          ).toBeInTheDocument();

          await act(async () => {
            resyncStateDeferred.resolve({
              revision: 1,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered from /api/state",
                }),
              ],
            });
            await flushUiWork();
          });

          await waitFor(() => {
            expect(
              within(sessionList).getByText("Recovered Session"),
            ).toBeInTheDocument();
          });
          expect(
            within(sessionList).getByText("Recovered from /api/state"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Stale Session"),
          ).not.toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Original preview"),
          ).not.toBeInTheDocument();
        });
      } finally {
        fetchStateSpy.mockRestore();
      }
    });
  });

  it("ignores a stale same-revision reconnect state after fallback-driven resync", async () => {
    await withSuppressedActWarnings(async () => {
      const resyncStateDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            return resyncStateDeferred.promise;
          }

          throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
        });

      try {
        await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
          await dispatchOpenedStateEvent(eventSource, {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Original Session",
                preview: "Original preview",
              }),
            ],
          });
          await within(sessionList).findByText("Original Session");

          act(() => {
            eventSource.dispatchError();
          });

          await dispatchOpenedStateEvent(eventSource, {
            _sseFallback: true,
            revision: 1,
            projects: [],
            sessions: [],
          });

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);

          await act(async () => {
            resyncStateDeferred.resolve({
              revision: 1,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered from /api/state",
                }),
              ],
            });
            await flushUiWork();
          });

          await waitFor(() => {
            expect(
              within(sessionList).getByText("Recovered Session"),
            ).toBeInTheDocument();
          });
          expect(
            within(sessionList).getByText("Recovered from /api/state"),
          ).toBeInTheDocument();

          await dispatchStateEvent(eventSource, {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Stale Reconnect Session",
                preview: "Stale after fallback",
              }),
            ],
          });

          expect(
            within(sessionList).queryByText("Stale Reconnect Session"),
          ).not.toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Stale after fallback"),
          ).not.toBeInTheDocument();
          expect(
            within(sessionList).getByText("Recovered Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Recovered from /api/state"),
          ).toBeInTheDocument();
        });
      } finally {
        fetchStateSpy.mockRestore();
      }
    });
  });

  it("retries the armed reconnect fallback after a fallback-driven /api/state failure", async () => {
    await withSuppressedActWarnings(async () => {
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            throw new Error("Temporary /api/state failure");
          }

          if (fetchStateCallCount === 2) {
            return {
              revision: 1,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered after retry",
                }),
              ],
            };
          }

          throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
        });

      try {
        await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
          await dispatchOpenedStateEvent(eventSource, {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Original Session",
                preview: "Original preview",
              }),
            ],
          });
          await within(sessionList).findByText("Original Session");

          act(() => {
            eventSource.dispatchError();
          });

          await dispatchOpenedStateEvent(eventSource, {
            _sseFallback: true,
            revision: 1,
            projects: [],
            sessions: [],
          });

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Original Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Original preview"),
          ).toBeInTheDocument();

          await waitFor(
            () => {
              expect(fetchStateSpy).toHaveBeenCalledTimes(2);
            },
            { timeout: 2000 },
          );
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(2);
          expect(
            within(sessionList).getByText("Recovered Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Recovered after retry"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Original Session"),
          ).not.toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Original preview"),
          ).not.toBeInTheDocument();
        });
      } finally {
        fetchStateSpy.mockRestore();
      }
    });
  });

  it("retries fallback-driven resyncs on a live stream after a transient /api/state failure", async () => {
    await withSuppressedActWarnings(async () => {
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            throw new Error("Temporary /api/state failure");
          }

          if (fetchStateCallCount === 2) {
            return {
              revision: 2,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered after live fallback retry",
                }),
              ],
            };
          }

          throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
        });

      try {
        await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
          await dispatchOpenedStateEvent(eventSource, {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Original Session",
                preview: "Original preview",
              }),
            ],
          });
          await within(sessionList).findByText("Original Session");

          await dispatchStateEvent(eventSource, {
            _sseFallback: true,
            revision: 2,
            projects: [],
            sessions: [],
          });
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Original Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Original preview"),
          ).toBeInTheDocument();

          await waitFor(
            () => {
              expect(fetchStateSpy).toHaveBeenCalledTimes(2);
            },
            { timeout: 2000 },
          );
          await settleAsyncUi();

          expect(
            within(sessionList).getByText("Recovered Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Recovered after live fallback retry"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Original Session"),
          ).not.toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Original preview"),
          ).not.toBeInTheDocument();
        });
      } finally {
        fetchStateSpy.mockRestore();
      }
    });
  });

  it("retries initial-connect fallback resyncs after a transient /api/state failure", async () => {
    await withSuppressedActWarnings(async () => {
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            throw new Error("Temporary /api/state failure");
          }

          if (fetchStateCallCount === 2) {
            return {
              revision: 1,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered after initial fallback retry",
                }),
              ],
            };
          }

          throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
        });

      try {
        await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
          await dispatchOpenedStateEvent(eventSource, {
            _sseFallback: true,
            revision: 1,
            projects: [],
            sessions: [],
          });

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).queryByText("Recovered Session"),
          ).not.toBeInTheDocument();

          await waitFor(
            () => {
              expect(fetchStateSpy).toHaveBeenCalledTimes(2);
            },
            { timeout: 2000 },
          );
          await settleAsyncUi();

          expect(
            within(sessionList).getByText("Recovered Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText(
              "Recovered after initial fallback retry",
            ),
          ).toBeInTheDocument();
        });
      } finally {
        fetchStateSpy.mockRestore();
      }
    });
  });

  it("still adopts non-fallback empty state snapshots", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue({
        revision: 99,
        projects: [],
        sessions: [],
      });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }

        const eventSource = latestEventSource();
        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                preview: "Visible before empty snapshot",
              }),
            ],
          });
          await flushUiWork();
        });
        await within(sessionList).findByText("Codex Session");

        await act(async () => {
          eventSource.dispatchNamedEvent("state", {
            revision: 2,
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });

        expect(fetchStateSpy).not.toHaveBeenCalled();
        expect(
          within(sessionList).queryByText("Codex Session"),
        ).not.toBeInTheDocument();
        expect(
          within(sessionList).queryByText("Visible before empty snapshot"),
        ).not.toBeInTheDocument();
      } finally {
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("falls back to /api/state after a post-hydration stream error when reconnect data does not arrive", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    try {
      await renderApp();
      const eventSource = latestEventSource();
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Sessions" }),
      );
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel =
        await within(sessionList).findByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      await screen.findByText("Waiting for the next chunk of output...");
      act(() => {
        eventSource.dispatchError();
      });

      await waitFor(
        () => {
          expect(
            fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
          ).toBe(true);
        },
        { timeout: 2000 },
      );
      await waitFor(() => {
        expect(screen.getAllByText("Here.")).toHaveLength(2);
      });
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
  it("cancels the reconnect fallback after a reconnect error when the first reconnect delta is ignored", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after ignored reconnect delta.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after ignored reconnect delta.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      act(() => {
        eventSource.dispatchError();
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("delta", {
          type: "textReplace",
          revision: 1,
          sessionId: "session-1",
          messageId: "message-assistant-1",
          messageIndex: 1,
          text: "Partial output.",
          preview: "Partial output.",
        });
      });

      await advanceTimers(399);

      expect(stateFetchCallCount()).toBe(0);

      await advanceTimers(1);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);
      expect(screen.getByText("Connected")).toBeInTheDocument();
      expect(screen.getAllByText("Partial output.")).toHaveLength(2);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("falls back to /api/state after a reconnect error when the first reconnect state snapshot is stale", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after stale reconnect state.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after stale reconnect state.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      act(() => {
        // Intentionally omit dispatchOpen(): without a confirmed stream reopen,
        // a same-revision state snapshot is treated as stale and must not cancel
        // the fast reconnect fallback.
        eventSource.dispatchError();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });

      await advanceTimers(399);

      expect(stateFetchCallCount()).toBe(0);

      await advanceTimers(1);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getAllByText("Here after stale reconnect state."),
      ).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
  it("resyncs when the page becomes visible again after a live reply finishes while hidden", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here while hidden.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here while hidden.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Sessions" }),
      );
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel =
        await within(sessionList).findByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      await screen.findByText("Waiting for the next chunk of output...");
      fetchMock.mockClear();

      act(() => {
        setDocumentVisibilityState("hidden");
        document.dispatchEvent(new Event("visibilitychange"));
      });

      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
      ).toBe(false);

      act(() => {
        setDocumentVisibilityState("visible");
        document.dispatchEvent(new Event("visibilitychange"));
      });

      await waitFor(() => {
        expect(
          fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
        ).toBe(true);
      });
      await waitFor(() => {
        expect(screen.getAllByText("Here while hidden.")).toHaveLength(2);
      });
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("ignores focus-triggered resync while the document remains hidden", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount > 1) {
          throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
        }

        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after hidden focus.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after hidden focus.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();
      fetchMock.mockClear();

      act(() => {
        window.dispatchEvent(new Event("blur"));
      });
      act(() => {
        setDocumentVisibilityState("hidden");
        window.dispatchEvent(new Event("focus"));
      });

      await settleAsyncUi();
      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
      ).toBe(false);

      act(() => {
        setDocumentVisibilityState("visible");
        window.dispatchEvent(new Event("focus"));
      });

      await waitFor(() => {
        expect(
          fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state"),
        ).toHaveLength(1);
      });
      await waitFor(() => {
        expect(
          screen.getByText("Here after hidden focus."),
        ).toBeInTheDocument();
      });
    } finally {
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resyncs after a focused wake without visibility events when the live transport stays stale during reconnects", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const reconnectBaseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(reconnectBaseline);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after wake.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after wake.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      // dispatchOpen alone does not mark transport activity; only SSE state/delta
      // events and successful resync snapshots refresh the watchdog baseline.
      for (let elapsed = 0; elapsed < 14800; elapsed += 200) {
        act(() => {
          eventSource.dispatchError();
          eventSource.dispatchOpen();
        });
        await advanceTimers(200);
      }

      act(() => {
        eventSource.dispatchError();
        eventSource.dispatchOpen();
      });
      await advanceTimers(199);
      expect(stateFetchCallCount()).toBe(0);

      let watchdogTriggered = false;
      for (let elapsed = 0; elapsed < 1000; elapsed += 200) {
        act(() => {
          eventSource.dispatchError();
          eventSource.dispatchOpen();
        });
        await advanceTimers(200);
        if (stateFetchCallCount() > 0) {
          watchdogTriggered = true;
          break;
        }
      }
      await settleAsyncUi();
      expect(watchdogTriggered).toBe(true);
      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Here after wake.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("does not watchdog-resync stale live sessions while the document stays hidden", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after wake.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after wake.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;
      setDocumentVisibilityState("hidden");

      // Hidden sessions should suppress the watchdog even after transport silence
      // crosses the normal stale threshold.
      for (let elapsed = 0; elapsed < 14800; elapsed += 200) {
        act(() => {
          eventSource.dispatchError();
          eventSource.dispatchOpen();
        });
        await advanceTimers(200);
      }

      act(() => {
        eventSource.dispatchError();
        eventSource.dispatchOpen();
      });
      await advanceTimers(199);
      expect(stateFetchCallCount()).toBe(0);

      for (let elapsed = 0; elapsed < 1000; elapsed += 200) {
        act(() => {
          eventSource.dispatchError();
          eventSource.dispatchOpen();
        });
        await advanceTimers(200);
      }
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("watchdog-resyncs when repeated ignored deltas arrive for an active session", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after ignored deltas.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after ignored deltas.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      let watchdogTriggered = false;
      for (
        let elapsed = 0;
        elapsed < LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 3000;
        elapsed += 1000
      ) {
        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: 1,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 1,
            text: "Partial output.",
            preview: "Partial output.",
          });
        });
        await advanceTimers(1000);
        if (stateFetchCallCount() > 0) {
          watchdogTriggered = true;
          break;
        }
      }

      await settleAsyncUi();

      expect(watchdogTriggered).toBe(true);
      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Here after ignored deltas.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
  it("watchdog-resyncs when only orchestrator deltas arrive during stale live transport", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after orchestrators.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after orchestrators.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      let orchestratorRevision = 1;
      let watchdogTriggered = false;
      for (
        let elapsed = 0;
        elapsed < LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 3000;
        elapsed += 1000
      ) {
        orchestratorRevision += 1;
        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "orchestratorsUpdated",
            revision: orchestratorRevision,
            orchestrators: [
              makeOrchestrator({ id: `orchestrator-${orchestratorRevision}` }),
            ],
          });
        });
        await advanceTimers(1000);
        if (stateFetchCallCount() > 0) {
          watchdogTriggered = true;
          break;
        }
      }

      await settleAsyncUi();

      expect(watchdogTriggered).toBe(true);
      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Here after orchestrators.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("watchdog-resyncs for active sessions first introduced by orchestrator deltas", async () => {
    const reviewerTemplateSession = {
      id: "reviewer",
      name: "Reviewer",
      agent: "Claude" as const,
      model: null,
      instructions: "Review the queued work.",
      autoApprove: false,
      inputMode: "queue" as const,
      position: { x: 520, y: 420 },
    };
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return jsonResponse({
            revision: 0,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Builder",
                status: "idle",
                preview: "Waiting.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "start work",
                  },
                ],
              }),
            ],
          });
        }

        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Builder",
              status: "idle",
              preview: "Waiting.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "start work",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Reviewer",
              status: "idle",
              preview: "Recovered after watchdog.",
              messages: [
                {
                  id: "message-user-reviewer-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "you",
                  text: "review the changes",
                },
                {
                  id: "message-assistant-reviewer-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Recovered after watchdog.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Builder",
              status: "idle",
              preview: "Waiting.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "start work",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();
      fetchMock.mockClear();
      stateRequestCount = 0;

      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "orchestratorsUpdated",
          revision: 2,
          orchestrators: [
            makeOrchestrator({
              status: "running",
              templateSnapshot: {
                ...makeOrchestrator().templateSnapshot,
                sessions: [
                  ...makeOrchestrator().templateSnapshot.sessions,
                  reviewerTemplateSession,
                ],
              },
              sessionInstances: [
                ...makeOrchestrator().sessionInstances,
                {
                  templateSessionId: "reviewer",
                  sessionId: "session-2",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
              ],
            }),
          ],
          sessions: [
            makeSession("session-2", {
              name: "Reviewer",
              status: "active",
              preview: "Draft review ready.",
              messages: [
                {
                  id: "message-user-reviewer-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "you",
                  text: "review the changes",
                },
                {
                  id: "message-assistant-reviewer-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Draft review ready.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);

      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("watchdog-resyncs a stalled active session even while another active session keeps receiving deltas", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 40,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Quiet Session",
              status: "idle",
              preview: "Recovered quiet session.",
              messages: [
                {
                  id: "message-user-quiet-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "quiet prompt",
                },
                {
                  id: "message-assistant-quiet-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Recovered quiet session.",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Noisy Session",
              status: "idle",
              preview: "Busy output settled.",
              messages: [
                {
                  id: "message-user-noisy-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "noisy prompt",
                },
                {
                  id: "message-assistant-noisy-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Busy output settled.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Quiet Session",
              status: "active",
              preview: "Quiet partial.",
              messages: [
                {
                  id: "message-user-quiet-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "quiet prompt",
                },
                {
                  id: "message-assistant-quiet-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Quiet partial.",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Noisy Session",
              status: "active",
              preview: "Busy output 1",
              messages: [
                {
                  id: "message-user-noisy-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "noisy prompt",
                },
                {
                  id: "message-assistant-noisy-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Busy output 1",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const quietSessionRowLabel =
        within(sessionList).getByText("Quiet Session");
      const quietSessionRowButton = quietSessionRowLabel.closest("button");
      if (!quietSessionRowButton) {
        throw new Error("Quiet session row button not found");
      }

      await clickAndSettle(quietSessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      let noisyRevision = 1;
      let watchdogTriggered = false;
      for (
        let elapsed = 0;
        elapsed < LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 3000;
        elapsed += 1000
      ) {
        noisyRevision += 1;
        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: noisyRevision,
            sessionId: "session-2",
            messageId: "message-assistant-noisy-1",
            messageIndex: 1,
            text: `Busy output ${noisyRevision}`,
            preview: `Busy output ${noisyRevision}`,
          });
        });
        await advanceTimers(1000);
        if (stateFetchCallCount() > 0) {
          watchdogTriggered = true;
          break;
        }
      }

      await settleAsyncUi();

      expect(watchdogTriggered).toBe(true);
      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Recovered quiet session.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("waits for a session-specific stale window even while another session stays noisy", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        return jsonResponse({
          revision: 30,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Quiet Session",
              status: "idle",
              preview: "Recovered quiet session.",
              messages: [
                {
                  id: "message-user-quiet-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "quiet prompt",
                },
                {
                  id: "message-assistant-quiet-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Recovered quiet session.",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Noisy Session",
              status: "idle",
              preview: "Busy output settled.",
              messages: [
                {
                  id: "message-user-noisy-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "noisy prompt",
                },
                {
                  id: "message-assistant-noisy-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Busy output settled.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Quiet Session",
              status: "active",
              preview: "Quiet partial.",
              messages: [
                {
                  id: "message-user-quiet-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "quiet prompt",
                },
                {
                  id: "message-assistant-quiet-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Quiet partial.",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Noisy Session",
              status: "active",
              preview: "Busy output 1",
              messages: [
                {
                  id: "message-user-noisy-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "noisy prompt",
                },
                {
                  id: "message-assistant-noisy-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Busy output 1",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const quietSessionRowLabel =
        within(sessionList).getByText("Quiet Session");
      const quietSessionRowButton = quietSessionRowLabel.closest("button");
      if (!quietSessionRowButton) {
        throw new Error("Quiet session row button not found");
      }

      await clickAndSettle(quietSessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      await advanceTimers(1000);
      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "textReplace",
          revision: 2,
          sessionId: "session-1",
          messageId: "message-assistant-quiet-1",
          messageIndex: 1,
          text: "Quiet partial refreshed.",
          preview: "Quiet partial refreshed.",
        });
      });
      await settleAsyncUi();

      let noisyRevision = 2;
      for (
        let elapsed = 0;
        elapsed < LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS - 1000;
        elapsed += 1000
      ) {
        noisyRevision += 1;
        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: noisyRevision,
            sessionId: "session-2",
            messageId: "message-assistant-noisy-1",
            messageIndex: 1,
            text: `Busy output ${noisyRevision}`,
            preview: `Busy output ${noisyRevision}`,
          });
        });
        await advanceTimers(1000);
      }

      await settleAsyncUi();
      expect(stateFetchCallCount()).toBe(0);

      let watchdogTriggered = false;
      for (let elapsed = 0; elapsed < 2000; elapsed += 1000) {
        noisyRevision += 1;
        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: noisyRevision,
            sessionId: "session-2",
            messageId: "message-assistant-noisy-1",
            messageIndex: 1,
            text: `Busy output ${noisyRevision}`,
            preview: `Busy output ${noisyRevision}`,
          });
        });
        await advanceTimers(1000);
        if (stateFetchCallCount() > 0) {
          watchdogTriggered = true;
          break;
        }
      }

      await settleAsyncUi();

      expect(watchdogTriggered).toBe(true);
      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Recovered quiet session.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("does not watchdog-resync a quiet active session before any assistant output arrives", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Quiet turn finished.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:30",
                  author: "assistant",
                  text: "Quiet turn finished.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      // 2 full stale windows: the watchdog should still stay quiet without current-turn output.
      await advanceTimers(
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS * 2 + 2000,
      );
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("does not watchdog-resync immediately after approval resumes an active turn without new assistant output", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const approvalEndpoint =
      "/api/sessions/session-1/approvals/message-approval-1";
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === approvalEndpoint) {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Approval granted. Codex is continuing...",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "run the command",
                },
                {
                  id: "message-approval-1",
                  type: "approval",
                  timestamp: "10:01",
                  author: "assistant",
                  title: "Codex needs approval",
                  command: "npm test",
                  detail: "Need permission to run the test suite.",
                  decision: "accepted",
                },
              ],
            }),
          ],
        });
      }
      if (url === "/api/state") {
        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after approval.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "run the command",
                },
                {
                  id: "message-approval-1",
                  type: "approval",
                  timestamp: "10:01",
                  author: "assistant",
                  title: "Codex needs approval",
                  command: "npm test",
                  detail: "Need permission to run the test suite.",
                  decision: "accepted",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Here after approval.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "approval",
              preview: "Approval pending.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "run the command",
                },
                {
                  id: "message-approval-1",
                  type: "approval",
                  timestamp: "10:01",
                  author: "assistant",
                  title: "Codex needs approval",
                  command: "npm test",
                  detail: "Need permission to run the test suite.",
                  decision: "pending",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByRole("button", { name: "Approve" }),
      ).toBeInTheDocument();

      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);

      await clickAndSettle(screen.getByRole("button", { name: "Approve" }));
      expect(screen.getByText("Codex needs approval")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled();
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resyncs on the first wake-gap tick after a local state adoption resumes an active turn", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const approvalEndpoint =
      "/api/sessions/session-1/approvals/message-approval-1";
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === approvalEndpoint) {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Approval granted. Codex is continuing...",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "run the command",
                },
                {
                  id: "message-approval-1",
                  type: "approval",
                  timestamp: "10:01",
                  author: "assistant",
                  title: "Codex needs approval",
                  command: "npm test",
                  detail: "Need permission to run the test suite.",
                  decision: "accepted",
                },
              ],
            }),
          ],
        });
      }
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount > 1) {
          throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
        }

        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after approval wake.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "run the command",
                },
                {
                  id: "message-approval-1",
                  type: "approval",
                  timestamp: "10:01",
                  author: "assistant",
                  title: "Codex needs approval",
                  command: "npm test",
                  detail: "Need permission to run the test suite.",
                  decision: "accepted",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Here after approval wake.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "approval",
              preview: "Approval pending.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "run the command",
                },
                {
                  id: "message-approval-1",
                  type: "approval",
                  timestamp: "10:01",
                  author: "assistant",
                  title: "Codex needs approval",
                  command: "npm test",
                  detail: "Need permission to run the test suite.",
                  decision: "pending",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByRole("button", { name: "Approve" }),
      ).toBeInTheDocument();

      await clickAndSettle(screen.getByRole("button", { name: "Approve" }));
      expect(screen.getByText("Codex needs approval")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled();
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 2000,
        ),
      );
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Here after approval wake.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
  it("does not watchdog-resync when queued follow-ups exist but the current turn has no assistant output yet", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      throw new Error(`Unexpected fetch: ${String(input)}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Current prompt",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Earlier prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Earlier answer.",
                },
                {
                  id: "message-user-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "you",
                  text: "Current prompt",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:03",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Cancel queued prompt" }),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      // 2 full stale windows: the watchdog should still stay quiet without current-turn output.
      await advanceTimers(
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS * 2 + 2000,
      );
      await settleAsyncUi();

      expect(fetchMock).not.toHaveBeenCalled();
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Cancel queued prompt" }),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("watchdog-resyncs when queued follow-ups exist after the current turn already streamed output", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Current turn finished.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Earlier prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Earlier answer.",
                },
                {
                  id: "message-user-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Current turn finished.",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:04",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Earlier prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Earlier answer.",
                },
                {
                  id: "message-user-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:04",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Cancel queued prompt" }),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Current turn finished.")).toHaveLength(2);
      expect(
        screen.getByRole("button", { name: "Cancel queued prompt" }),
      ).toBeInTheDocument();
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resyncs on the watchdog drift-gap path when queued follow-ups exist behind a quiet current turn", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Current turn finished.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Earlier prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Earlier answer.",
                },
                {
                  id: "message-user-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Current turn finished.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Current prompt",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Earlier prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Earlier answer.",
                },
                {
                  id: "message-user-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "you",
                  text: "Current prompt",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:03",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Cancel queued prompt" }),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 2000,
        ),
      );
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Current turn finished.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "Cancel queued prompt" }),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resyncs on the watchdog drift-gap path before transport becomes stale", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after clock jump.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after clock jump.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        // Intentionally no assistant message: the drift-gap path must recover any
        // active session after wake, even before the current turn has output.
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 2000,
        ),
      );
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Here after clock jump.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("keeps wake-gap recovery armed for a stalled session despite unrelated post-wake SSE traffic", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount > 1) {
          throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
        }

        return jsonResponse({
          revision: 4,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Quiet Session",
              status: "idle",
              preview: "Recovered quiet session after wake.",
              messages: [
                {
                  id: "message-user-quiet-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "quiet prompt",
                },
                {
                  id: "message-assistant-quiet-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Recovered quiet session after wake.",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Noisy Session",
              status: "active",
              preview: "Still streaming from session 2.",
              messages: [
                {
                  id: "message-user-noisy-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "noisy prompt",
                },
                {
                  id: "message-assistant-noisy-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Still streaming from session 2.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        // Intentionally no assistant message for session-1: the wake-gap path must
        // stay armed even if unrelated sessions keep producing live traffic first.
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Quiet Session",
              status: "active",
              preview: "quiet prompt",
              messages: [
                {
                  id: "message-user-quiet-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "quiet prompt",
                },
              ],
            }),
            makeSession("session-2", {
              name: "Noisy Session",
              status: "active",
              preview: "Busy output 1",
              messages: [
                {
                  id: "message-user-noisy-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "noisy prompt",
                },
                {
                  id: "message-assistant-noisy-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Busy output 1",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const quietSessionRowLabel =
        within(sessionList).getByText("Quiet Session");
      const quietSessionRowButton = quietSessionRowLabel.closest("button");
      if (!quietSessionRowButton) {
        throw new Error("Quiet session row button not found");
      }

      await clickAndSettle(quietSessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 2000,
        ),
      );
      act(() => {
        eventSource.dispatchNamedEvent("delta", {
          type: "textReplace",
          revision: 2,
          sessionId: "session-2",
          messageId: "message-assistant-noisy-1",
          messageIndex: 1,
          text: "Still streaming from session 2.",
          preview: "Still streaming from session 2.",
        });
        eventSource.dispatchNamedEvent("delta", {
          type: "orchestratorsUpdated",
          revision: 3,
          orchestrators: [makeOrchestrator({ id: "orchestrator-1" })],
        });
      });
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(0);

      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getAllByText("Recovered quiet session after wake."),
      ).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resets the watchdog drift baseline after a long reconnect resync completes", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    const firstStateFetch = createDeferred<Response>();
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return firstStateFetch.promise;
        }

        throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      act(() => {
        eventSource.dispatchError();
      });
      await advanceTimers(400);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      // 3.4 s past the drift threshold gives the next watchdog tick time to run
      // before the separate 15 s stale-transport window could become the trigger.
      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 3400,
        ),
      );
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      await act(async () => {
        firstStateFetch.resolve(
          jsonResponse({
            revision: 2,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Recovered after wake.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                  {
                    id: "message-assistant-final-1",
                    type: "text",
                    timestamp: "10:02",
                    author: "assistant",
                    text: "Recovered after wake.",
                  },
                ],
              }),
            ],
          }),
        );
        await flushUiWork();
      });

      await advanceTimers(LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 1000);
      // 6000 ms < LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS (15000 ms), so
      // the stale-transport path cannot independently trigger the resync here.
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Recovered after wake.")).toHaveLength(2);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resets the watchdog drift baseline when live state recovers before a slow reconnect resync settles", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    const firstStateFetch = createDeferred<Response>();
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return firstStateFetch.promise;
        }

        throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      act(() => {
        eventSource.dispatchError();
      });
      await advanceTimers(400);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 3400,
        ),
      );
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Recovered from live state.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-live-state-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Recovered from live state.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();
      expect(screen.getAllByText("Recovered from live state.")).toHaveLength(2);

      await act(async () => {
        firstStateFetch.resolve(
          jsonResponse({
            // Stale: intentionally below SSE revision 2 - rejected by adoptState,
            // proving only the live SSE baseline reset matters.
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Partial output.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                  {
                    id: "message-assistant-partial-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Partial output.",
                  },
                ],
              }),
            ],
          }),
        );
        await flushUiWork();
      });

      await advanceTimers(LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Recovered from live state.")).toHaveLength(2);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("resets the watchdog drift baseline when a live session delta recovers before a slow reconnect resync settles", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    const firstStateFetch = createDeferred<Response>();
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return firstStateFetch.promise;
        }

        throw new Error(`Unexpected /api/state call #${stateRequestCount}`);
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      act(() => {
        eventSource.dispatchError();
      });
      await advanceTimers(400);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      vi.setSystemTime(
        new Date(
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 3400,
        ),
      );
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("delta", {
          type: "textReplace",
          revision: 2,
          sessionId: "session-1",
          messageId: "message-assistant-partial-1",
          messageIndex: 1,
          text: "Recovered from live delta.",
          preview: "Recovered from live delta.",
        });
      });
      await settleAsyncUi();
      expect(screen.getAllByText("Recovered from live delta.")).toHaveLength(2);

      await act(async () => {
        firstStateFetch.resolve(
          jsonResponse({
            // Stale: intentionally below SSE revision 2 - rejected by adoptState,
            // proving only the live SSE baseline reset matters.
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Partial output.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                  {
                    id: "message-assistant-partial-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Partial output.",
                  },
                ],
              }),
            ],
          }),
        );
        await flushUiWork();
      });

      await advanceTimers(LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(screen.getAllByText("Recovered from live delta.")).toHaveLength(2);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("retries stale-transport watchdog resyncs after the cooldown when a fetch fails", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
    let shouldFailFirstStateFetch = true;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        if (shouldFailFirstStateFetch) {
          shouldFailFirstStateFetch = false;
          throw new Error("backend unavailable");
        }

        return jsonResponse({
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after retry.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here after retry.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();

      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();

      await advanceTimers(
        LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS - 1000,
      );
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(2);
      expect(screen.getAllByText("Here after retry.")).toHaveLength(2);
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("preserves the watchdog cooldown after a successful watchdog snapshot until live transport resumes", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return jsonResponse({
            revision: 2,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Partial output.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                  {
                    id: "message-assistant-partial-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Partial output.",
                  },
                ],
              }),
            ],
          });
        }

        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after enforced cooldown.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-final-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Here after enforced cooldown.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () =>
      fetchMock.mock.calls.filter(([url]) => String(url) === "/api/state")
        .length;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();

      // 15s stale threshold + 1s watchdog tick margin -> first watchdog resync.
      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();

      // One tick short of the cooldown boundary: watchdog must stay quiet.
      await advanceTimers(
        LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS - 1000,
      );
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();

      // Crossing the next 1s tick moves the watchdog past the cooldown boundary.
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(2);
      expect(screen.getAllByText("Here after enforced cooldown.")).toHaveLength(
        2,
      );
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
  it("clears the watchdog cooldown when live SSE state resumes after a watchdog snapshot", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalVisibilityState = document.visibilityState;
    const baseline = new Date("2026-04-02T09:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(baseline);
    let stateRequestCount = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        stateRequestCount += 1;
        if (stateRequestCount === 1) {
          return jsonResponse({
            revision: 2,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Partial output.",
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                  {
                    id: "message-assistant-partial-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Partial output.",
                  },
                ],
              }),
            ],
          });
        }

        return jsonResponse({
          revision: 4,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here after cleared cooldown.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-final-1",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Here after cleared cooldown.",
                },
              ],
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const stateFetchCallCount = () => stateRequestCount;
    setDocumentVisibilityState("visible");
    try {
      await renderApp();
      const eventSource = latestEventSource();
      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Partial output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Partial output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = within(sessionList).getByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
      fetchMock.mockClear();
      stateRequestCount = 0;

      // First watchdog snapshot stays active, so the cooldown remains armed.
      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();

      // A fresh SSE state payload should clear the residual watchdog cooldown and
      // reset the stale-transport timer from this newer live activity.
      act(() => {
        eventSource.dispatchNamedEvent("state", {
          revision: 3,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Fresh live output.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-partial-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Fresh live output.",
                },
              ],
            }),
          ],
        });
      });
      await settleAsyncUi();

      expect(screen.getAllByText("Fresh live output.")).toHaveLength(2);
      expect(stateFetchCallCount()).toBe(1);

      await advanceTimers(LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(1);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();

      // The next 1s watchdog tick should fire immediately once transport is stale again.
      await advanceTimers(1000);
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBe(2);
      expect(screen.getAllByText("Here after cleared cooldown.")).toHaveLength(
        2,
      );
      expect(
        screen.queryByText("Waiting for the next chunk of output..."),
      ).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it.each(["approval", "error"] as const)(
    "does not watchdog-resync focused %s sessions without active streaming",
    async (status) => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalVisibilityState = document.visibilityState;
      const sessionPreview =
        status === "approval" ? "Awaiting approval." : "Last command failed.";
      vi.useFakeTimers();
      vi.setSystemTime(new Date("2026-04-02T09:00:00.000Z"));
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        throw new Error(`Unexpected fetch: ${String(input)}`);
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();
      setDocumentVisibilityState("visible");
      try {
        await renderApp();
        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 1,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status,
                preview: sessionPreview,
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "test",
                  },
                ],
              }),
            ],
          });
        });
        await settleAsyncUi();

        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }

        const sessionRowLabel = within(sessionList).getByText("Codex Session");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }

        await clickAndSettle(sessionRowButton);
        expect(screen.getByText(sessionPreview)).toBeInTheDocument();
        fetchMock.mockClear();

        await advanceTimers(
          LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000,
        );
        await settleAsyncUi();

        expect(fetchMock).not.toHaveBeenCalled();
      } finally {
        vi.useRealTimers();
        setDocumentVisibilityState(originalVisibilityState);
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    },
  );

  it("adopts the reconnect state snapshot even when the backend restarts at the same revision", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        throw new Error("backend restarting");
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      const eventSource = latestEventSource();
      expect(eventSource).toBeTruthy();

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "test",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
              ],
            }),
          ],
        });
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Sessions" }),
      );
      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel =
        await within(sessionList).findByText("Codex Session");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);
      await waitFor(() => {
        expect(screen.getAllByText("test").length).toBeGreaterThan(0);
      });
      expect(screen.queryByText("Here.")).not.toBeInTheDocument();

      act(() => {
        eventSource.dispatchError();
      });
      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/state"),
      ).toBe(false);

      act(() => {
        eventSource.dispatchOpen();
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Here.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "test",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Here.",
                },
              ],
            }),
          ],
        });
      });

      await waitFor(() => {
        expect(screen.getAllByText("Here.")).toHaveLength(2);
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("shows a workspace switcher with saved workspaces and can open a new workspace window", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const openSpy = vi.spyOn(window, "open").mockImplementation(() => null);
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );

        const switcherTrigger = await screen.findByRole("button", {
          name: /workspace /i,
        });
        await clickAndSettle(switcherTrigger);

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        expect(
          within(switcherDialog).getAllByText("monitor-left").length,
        ).toBeGreaterThan(0);
        expect(
          within(switcherDialog).getAllByText("monitor-right").length,
        ).toBeGreaterThan(0);

        await clickAndSettle(
          await screen.findByRole("button", { name: "New window" }),
        );

        expect(openSpy).toHaveBeenCalledTimes(1);
        expect(String(openSpy.mock.calls[0]?.[0] ?? "")).toContain(
          "workspace=",
        );
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        openSpy.mockRestore();
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("flushes a pending workspace layout save with keepalive on pagehide", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue({
        revision: 1,
        projects: [],
        sessions: [],
      });
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "workspace-next",
              revision: 2,
              updatedAt: "2026-03-30 09:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-current",
            updatedAt: "2026-03-30 09:31:00",
          }),
        );
      window.localStorage.clear();
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );

      try {
        await renderApp();
        saveWorkspaceLayoutSpy.mockClear();
        act(() => {
          window.dispatchEvent(new Event("pagehide"));
        });

        await waitFor(() => {
          expect(saveWorkspaceLayoutSpy).toHaveBeenCalledWith(
            expect.any(String),
            expect.objectContaining({
              workspace: expect.any(Object),
            }),
            { keepalive: true },
          );
        });
      } finally {
        window.localStorage.clear();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchWorkspaceLayoutsSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("refreshes model options after creating a new Codex session", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const createSessionDeferred = createDeferred<{
        sessionId: string;
        state: Awaited<ReturnType<typeof api.fetchState>>;
      }>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => fetchStateDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
      const refreshSessionModelOptionsSpy = vi
        .spyOn(api, "refreshSessionModelOptions")
        .mockImplementation(() => refreshSessionModelOptionsDeferred.promise);

      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalled();
        });
        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-1",
            state: {
              revision: 2,
              projects: [],
              sessions: [
                {
                  id: "session-1",
                  name: "Codex 1",
                  emoji: "O",
                  agent: "Codex",
                  workdir: "/tmp",
                  model: "gpt-5.4",
                  approvalPolicy: "never",
                  reasoningEffort: "medium",
                  sandboxMode: "workspace-write",
                  status: "idle",
                  preview: "Ready for a prompt.",
                  messages: [],
                },
              ],
            },
          });
          await flushUiWork();
        });

        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve({
            revision: 3,
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Codex 1",
                emoji: "O",
                agent: "Codex",
                workdir: "/tmp",
                model: "gpt-5.4",
                modelOptions: [
                  {
                    label: "gpt-5.4",
                    value: "gpt-5.4",
                    description: "Latest frontier agentic coding model.",
                    defaultReasoningEffort: "medium",
                    supportedReasoningEfforts: [
                      "low",
                      "medium",
                      "high",
                      "xhigh",
                    ],
                  },
                ],
                approvalPolicy: "never",
                reasoningEffort: "medium",
                sandboxMode: "workspace-write",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          });
          await flushUiWork();
        });
        await screen.findAllByText("Codex 1");
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("resyncs on the first wake-gap tick for a newly created active session before any SSE arrives", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const baseline = new Date("2026-04-02T09:00:00.000Z");
      let mockedNow = baseline.getTime();
      const dateNowSpy = vi
        .spyOn(Date, "now")
        .mockImplementation(() => mockedNow);
      const createSessionDeferred = createDeferred<{
        sessionId: string;
        state: Awaited<ReturnType<typeof api.fetchState>>;
      }>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            return {
              revision: 1,
              projects: [],
              sessions: [],
            };
          }
          if (fetchStateCallCount === 2) {
            return {
              revision: 4,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Codex 1",
                  status: "idle",
                  preview: "Here after create wake.",
                  modelOptions: [
                    {
                      label: "gpt-5.4",
                      value: "gpt-5.4",
                      description: "Latest frontier agentic coding model.",
                      defaultReasoningEffort: "medium",
                      supportedReasoningEfforts: [
                        "low",
                        "medium",
                        "high",
                        "xhigh",
                      ],
                    },
                  ],
                  messages: [
                    {
                      id: "message-user-1",
                      type: "text",
                      timestamp: "10:00",
                      author: "you",
                      text: "run the command",
                    },
                    {
                      id: "message-assistant-1",
                      type: "text",
                      timestamp: "10:02",
                      author: "assistant",
                      text: "Here after create wake.",
                    },
                  ],
                }),
              ],
            };
          }

          throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
        });
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
      const refreshSessionModelOptionsSpy = vi
        .spyOn(api, "refreshSessionModelOptions")
        .mockImplementation(() => refreshSessionModelOptionsDeferred.promise);

      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        expect(createSessionSpy).toHaveBeenCalledTimes(1);
        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-1",
            state: {
              revision: 2,
              projects: [],
              sessions: [
                makeSession("session-1", {
                  name: "Codex 1",
                  status: "active",
                  preview: "run the command",
                  messages: [
                    {
                      id: "message-user-1",
                      type: "text",
                      timestamp: "10:00",
                      author: "you",
                      text: "run the command",
                    },
                  ],
                }),
              ],
            },
          });
          await flushUiWork();
        });

        expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith("session-1");
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve({
            revision: 3,
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex 1",
                status: "active",
                preview: "run the command",
                modelOptions: [
                  {
                    label: "gpt-5.4",
                    value: "gpt-5.4",
                    description: "Latest frontier agentic coding model.",
                    defaultReasoningEffort: "medium",
                    supportedReasoningEfforts: [
                      "low",
                      "medium",
                      "high",
                      "xhigh",
                    ],
                  },
                ],
                messages: [
                  {
                    id: "message-user-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "run the command",
                  },
                ],
              }),
            ],
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        // No SSE state arrives here: the active session exists only because the
        // create-session REST flow adopted it locally.
        expect(
          screen.getByText("Waiting for the next chunk of output..."),
        ).toBeInTheDocument();
        fetchStateSpy.mockClear();

        // Fake timers trigger overlapping React act() work in this create-session
        // flow, so keep one real watchdog interval and drive only Date.now.
        mockedNow =
          baseline.getTime() + LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 2000;
        await new Promise((resolve) => window.setTimeout(resolve, 1500));
        await settleAsyncUi();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            screen.queryByText("Waiting for the next chunk of output..."),
          ).not.toBeInTheDocument();
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        dateNowSpy.mockRestore();
        fetchStateSpy.mockRestore();
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("adopts the full orchestrator-start state so the next delta does not force a resync", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue({
        revision: 1,
        projects: [],
        sessions: [],
      });
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            updatedAt: "2026-03-30 09:01:00",
          }),
        );
      const fetchTemplatesSpy = vi
        .spyOn(api, "fetchOrchestratorTemplates")
        .mockResolvedValue({
          templates: [
            {
              id: "delivery-flow",
              name: "Delivery Flow",
              description: "Implement and review a change.",
              createdAt: "2026-03-30 09:00:00",
              updatedAt: "2026-03-30 09:05:00",
              projectId: "project-local",
              sessions: [
                {
                  id: "builder",
                  name: "Builder",
                  agent: "Codex",
                  model: null,
                  instructions: "Implement the change.",
                  autoApprove: true,
                  inputMode: "queue",
                  position: { x: 220, y: 420 },
                },
              ],
              transitions: [],
            },
          ],
        });
      const createOrchestratorInstanceSpy = vi
        .spyOn(api, "createOrchestratorInstance")
        .mockResolvedValue({
          orchestrator: {
            id: "orchestrator-1",
            templateId: "delivery-flow",
            projectId: "project-local",
            templateSnapshot: {
              id: "delivery-flow",
              name: "Delivery Flow",
              description: "Implement and review a change.",
              createdAt: "2026-03-30 09:00:00",
              updatedAt: "2026-03-30 09:05:00",
              projectId: "project-local",
              sessions: [
                {
                  id: "builder",
                  name: "Builder",
                  agent: "Codex",
                  model: null,
                  instructions: "Implement the change.",
                  autoApprove: true,
                  inputMode: "queue",
                  position: { x: 220, y: 420 },
                },
              ],
              transitions: [],
            },
            status: "running",
            sessionInstances: [],
            createdAt: "2026-03-30 09:06:00",
            completedAt: null,
          },
          state: {
            revision: 3,
            projects: [
              {
                id: "project-local",
                name: "Local Project",
                rootPath: "/repo",
                remoteId: "local",
              },
              {
                id: "project-added",
                name: "Added By Start",
                rootPath: "/repo-added",
              },
            ],
            sessions: [
              makeSession("session-orchestrated", {
                name: "Orchestrated Builder",
                projectId: "project-local",
                preview: "Waiting for work",
                status: "active",
                workdir: "/repo",
              }),
            ],
          },
        });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();

        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 2,
            projects: [
              {
                id: "project-local",
                name: "Local Project",
                rootPath: "/repo",
                remoteId: "local",
              },
            ],
            sessions: [],
          });
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(
          screen.getByRole("tab", { name: "Orchestrators" }),
        );
        expect(
          await screen.findByDisplayValue("Delivery Flow"),
        ).toBeInTheDocument();

        const templateProjectSelect = screen.getByLabelText("Project", {
          selector: "select#orchestrator-template-project",
        });
        fireEvent.change(templateProjectSelect, {
          target: { value: "project-local" },
        });
        expect(templateProjectSelect).toHaveValue("project-local");
        const runButton = document.querySelector<HTMLButtonElement>(
          ".orchestrator-run-button",
        );
        if (!runButton) {
          throw new Error("Run button not found");
        }
        expect(runButton).toBeEnabled();
        await clickAndSettle(runButton);

        await waitFor(() => {
          expect(createOrchestratorInstanceSpy).toHaveBeenCalledWith(
            "delivery-flow",
            "project-local",
          );
        });
        expect(
          await screen.findByText("Orchestrated Builder"),
        ).toBeInTheDocument();

        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageCreated",
            revision: 4,
            sessionId: "session-orchestrated",
            messageId: "message-1",
            messageIndex: 0,
            message: {
              id: "message-1",
              type: "text",
              timestamp: "09:07",
              author: "assistant",
              text: "Orchestration delta applied.",
            },
            preview: "Orchestration delta applied.",
            status: "active",
          });
        });

        await screen.findByText("Orchestration delta applied.");
        expect(fetchStateSpy).not.toHaveBeenCalled();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        fetchTemplatesSpy.mockRestore();
        createOrchestratorInstanceSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("updates the orchestrator library from live orchestrator deltas without forcing a resync", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue({
        revision: 1,
        projects: [
          {
            id: "project-local",
            name: "Local Project",
            rootPath: "/repo",
            remoteId: "local",
          },
        ],
        orchestrators: [makeOrchestrator()],
        sessions: [
          makeSession("session-1", {
            name: "Builder",
            projectId: "project-local",
            workdir: "/repo",
          }),
        ],
      });
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            updatedAt: "2026-03-30 09:07:00",
          }),
        );
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();

        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "orchestratorsUpdated",
            revision: 2,
            orchestrators: [
              makeOrchestrator({
                status: "paused",
              }),
            ],
          });
        });
        await settleAsyncUi();

        expect(fetchStateSpy).toHaveBeenCalledTimes(1);
      } finally {
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("merges sessions carried by live orchestrator deltas without forcing a resync", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue({
        revision: 1,
        projects: [
          {
            id: "project-local",
            name: "Local Project",
            rootPath: "/repo",
            remoteId: "local",
          },
        ],
        orchestrators: [makeOrchestrator()],
        sessions: [
          makeSession("session-1", {
            name: "Builder",
            projectId: "project-local",
            workdir: "/repo",
          }),
        ],
      });
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            updatedAt: "2026-03-30 09:07:00",
          }),
        );
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();
      const reviewerTemplateSession = {
        id: "reviewer",
        name: "Reviewer",
        agent: "Claude" as const,
        model: null,
        instructions: "Review the queued work.",
        autoApprove: false,
        inputMode: "queue" as const,
        position: { x: 520, y: 420 },
      };

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchStateEvent(eventSource, {
          revision: 1,
          projects: [
            {
              id: "project-local",
              name: "Local Project",
              rootPath: "/repo",
              remoteId: "local",
            },
          ],
          orchestrators: [makeOrchestrator()],
          sessions: [
            makeSession("session-1", {
              name: "Builder",
              projectId: "project-local",
              workdir: "/repo",
            }),
          ],
        });

        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        await screen.findByText("Builder");
        fetchStateSpy.mockClear();
        expect(within(sessionList).queryByText("Reviewer")).not.toBeInTheDocument();

        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "orchestratorsUpdated",
            revision: 2,
            orchestrators: [
              makeOrchestrator({
                status: "paused",
                templateSnapshot: {
                  ...makeOrchestrator().templateSnapshot,
                  sessions: [
                    ...makeOrchestrator().templateSnapshot.sessions,
                    reviewerTemplateSession,
                  ],
                },
                sessionInstances: [
                  ...makeOrchestrator().sessionInstances,
                  {
                    templateSessionId: "reviewer",
                    sessionId: "session-2",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                ],
              }),
            ],
            sessions: [
              makeSession("session-2", {
                name: "Reviewer",
                agent: "Claude",
                model: "claude-sonnet-4-5",
                projectId: "project-local",
                workdir: "/repo",
                preview: "Draft review ready.",
                messages: [
                  {
                    id: "message-user-reviewer-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "review the implementation",
                  },
                  {
                    id: "message-assistant-reviewer-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Draft review ready.",
                  },
                ],
              }),
            ],
          });
        });
        await settleAsyncUi();

        const reviewerRowLabel = await waitFor(() =>
          within(sessionList).getByText("Reviewer"),
        );
        const reviewerRowButton = reviewerRowLabel.closest("button");
        if (!reviewerRowButton) {
          throw new Error("Reviewer session row button not found");
        }
        await clickAndSettle(reviewerRowButton);
        expect(screen.getAllByText("Draft review ready.").length).toBeGreaterThan(0);

        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: 3,
            sessionId: "session-2",
            messageId: "message-assistant-reviewer-1",
            messageIndex: 1,
            text: "Reviewer output updated.",
            preview: "Reviewer output updated.",
          });
        });
        await settleAsyncUi();

        expect(screen.getAllByText("Reviewer output updated.").length).toBeGreaterThan(0);
        expect(fetchStateSpy).not.toHaveBeenCalled();
      } finally {
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("filters sessions from the control panel project selector", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-api",
                name: "API",
                rootPath: "/projects/api",
              },
              {
                id: "project-web",
                name: "Web",
                rootPath: "/projects/web",
              },
            ],
            sessions: [
              makeSession("session-web", {
                name: "Web Session",
                projectId: "project-web",
                workdir: "/projects/web",
              }),
            ],
          });
        }

        throw new Error(`Unexpected fetch: ${url}`);
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();
        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        await screen.findByText("API");
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );

        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("All projects");

        await selectComboboxOption("Project", /^API$/i);

        await waitFor(() => {
          expect(screen.getByText("No sessions in API.")).toBeInTheDocument();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Files" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("API");
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("keeps standalone project tabs independent from the docked control panel scope", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-api",
                name: "API",
                rootPath: "/projects/api",
              },
              {
                id: "project-web",
                name: "Web",
                rootPath: "/projects/web",
              },
            ],
            sessions: [
              makeSession("session-web", {
                name: "Web Session",
                projectId: "project-web",
                workdir: "/projects/web",
              }),
              makeSession("session-api", {
                name: "API Session",
                projectId: "project-api",
                workdir: "/projects/api",
              }),
            ],
          });
        }

        throw new Error(`Unexpected fetch: ${url}`);
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );

        const projectSurfaces = Array.from(
          document.querySelectorAll(".project-controls"),
        );
        const standaloneProjectsSurface =
          projectSurfaces[projectSurfaces.length - 1] ?? null;
        if (!(standaloneProjectsSurface instanceof HTMLElement)) {
          throw new Error("Standalone projects surface not found");
        }

        const apiRowLabel = within(standaloneProjectsSurface).getByText("API");
        const apiRowButton = apiRowLabel.closest("button");
        if (!apiRowButton) {
          throw new Error("Standalone API project row not found");
        }

        await clickAndSettle(apiRowButton);
        expect(apiRowButton).toHaveClass("selected");

        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("All projects");
        expect(screen.getByText("Web Session")).toBeInTheDocument();
        expect(screen.getByText("API Session")).toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("loads project files in the control panel without requiring a session", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const requestUrl = new URL(String(input), "http://localhost");
      if (requestUrl.pathname === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [
            {
              id: "project-api",
              name: "API",
              rootPath: "/projects/api",
            },
            {
              id: "project-web",
              name: "Web",
              rootPath: "/projects/web",
            },
          ],
          sessions: [
            makeSession("session-web", {
              name: "Web Session",
              projectId: "project-web",
              workdir: "/projects/web",
            }),
          ],
        });
      }

      if (requestUrl.pathname === "/api/fs") {
        expect(requestUrl.searchParams.get("path")).toBe("/projects/api");
        expect(requestUrl.searchParams.get("sessionId")).toBeNull();
        expect(requestUrl.searchParams.get("projectId")).toBe("project-api");
        return jsonResponse({
          entries: [
            {
              kind: "file",
              name: "README.md",
              path: "/projects/api/README.md",
            },
          ],
          name: "api",
          path: "/projects/api",
        });
      }

      if (requestUrl.pathname === "/api/git/status") {
        expect(requestUrl.searchParams.get("path")).toBe("/projects/api");
        expect(requestUrl.searchParams.get("sessionId")).toBeNull();
        expect(requestUrl.searchParams.get("projectId")).toBe("project-api");
        return jsonResponse({
          ahead: 0,
          behind: 0,
          branch: "main",
          files: [],
          isClean: true,
          repoRoot: "/projects/api",
          upstream: "origin/main",
          workdir: "/projects/api",
        });
      }

      throw new Error(
        `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
      );
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();
      const eventSource = latestEventSource();
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchError();
      });
      await settleAsyncUi();

      await selectComboboxOption("Project", /^API$/i);
      await clickAndSettle(
        await screen.findByRole("button", { name: "Files" }),
      );

      expect(
        await screen.findByRole("button", { name: /^README\.md/i }),
      ).toBeInTheDocument();
      expect(
        screen.queryByText(
          "This file browser is no longer associated with a live session or project.",
        ),
      ).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("opens standalone tabs for sessions, projects, and git from the control panel", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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

          if (requestUrl.pathname === "/api/git/status") {
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

          if (requestUrl.pathname === "/api/orchestrators/templates") {
            return jsonResponse({
              templates: [
                {
                  id: "delivery-flow",
                  name: "Delivery Flow",
                  description: "Implement and review a change.",
                  createdAt: "2026-03-26 10:00:00",
                  updatedAt: "2026-03-26 10:15:00",
                  sessions: [
                    {
                      id: "builder",
                      name: "Builder",
                      agent: "Codex",
                      model: null,
                      instructions: "Implement the change.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 220, y: 420 },
                    },
                  ],
                  transitions: [],
                },
              ],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      function getSessionTablist() {
        const sessionTablist = screen
          .getAllByRole("tablist")
          .find((candidate) => within(candidate).queryByText("Session 1"));

        if (!sessionTablist) {
          throw new Error("Session tablist not found");
        }

        return sessionTablist;
      }

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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }

        const sessionRowLabel =
          await within(sessionList).findByText("Session 1");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }

        await clickAndSettle(sessionRowButton);

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );
        expect(
          within(getSessionTablist()).getByText("Sessions"),
        ).toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );
        expect(
          within(getSessionTablist()).getByText("Projects"),
        ).toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Git status" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );
        expect(
          within(getSessionTablist()).getByText(/^Git:/),
        ).toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("TermAl");
        expect(
          screen.queryByRole("button", { name: /Load repo/i }),
        ).not.toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Orchestrators" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Edit canvas" }),
        );
        expect(
          within(getSessionTablist()).getByText("Orchestration: delivery-flow"),
        ).toBeInTheDocument();
        expect(
          await screen.findByRole("heading", {
            level: 3,
            name: "Edit template",
          }),
        ).toBeInTheDocument();
      } finally {
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("drags control panel dock sections into the workspace", async () => {
    await withSuppressedActWarnings(async () => {
      const context = await renderAppWithProjectAndSession({
        includeGitStatus: true,
        includeWorkspacePersistence: true,
      });

      try {
        async function dragDockSectionToWorkspace(
          buttonName: string,
          expectedTabName: RegExp,
        ) {
          const dock = await screen.findByRole("navigation", {
            name: "Control panel dock",
          });
          const sectionButton = await within(dock).findByRole("button", {
            name: buttonName,
          });
          expect(sectionButton).toHaveAttribute("draggable", "true");

          const dataTransfer = createDragDataTransfer();
          await act(async () => {
            fireEvent.dragStart(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          const rightDropZone = document.querySelector(".pane-drop-zone-right");
          if (!(rightDropZone instanceof HTMLDivElement)) {
            throw new Error("Right drop zone not found");
          }

          await act(async () => {
            fireEvent.dragEnter(rightDropZone, { dataTransfer });
            fireEvent.dragOver(rightDropZone, { dataTransfer });
            fireEvent.drop(rightDropZone, { dataTransfer });
            fireEvent.dragEnd(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          await waitFor(() => {
            expect(
              screen
                .getAllByRole("tab")
                .some((tab) =>
                  expectedTabName.test(
                    (tab.textContent ?? "").replace(/\u00d7/g, "").trim(),
                  ),
                ),
            ).toBe(true);
          });
        }

        await dragDockSectionToWorkspace("Sessions", /^Sessions$/i);
        await dragDockSectionToWorkspace("Orchestrators", /^Orchestrators$/i);
        await dragDockSectionToWorkspace("Files", /Files: termal/i);
        await dragDockSectionToWorkspace("Git status", /Git: termal/i);
      } finally {
        context.cleanup();
      }
    });
  });
  it("accepts control panel launcher drags in the pane body when dragover only exposes text/plain", async () => {
    await withSuppressedActWarnings(async () => {
      const context = await renderAppWithProjectAndSession({
        includeWorkspacePersistence: true,
      });

      try {
        const dock = await screen.findByRole("navigation", {
          name: "Control panel dock",
        });
        const sectionButton = await within(dock).findByRole("button", {
          name: "Sessions",
        });
        const dataTransfer = createDragDataTransfer();

        await act(async () => {
          fireEvent.dragStart(sectionButton, { dataTransfer });
        });

        const reducedMimeDataTransfer =
          createReducedMimeDragDataTransfer(dataTransfer);
        const workspaceTabList = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((tabList) =>
            within(tabList).queryByRole("tab", { name: /Session 1/i }),
          );
        if (!(workspaceTabList instanceof HTMLDivElement)) {
          throw new Error("Workspace tab list not found");
        }
        const workspacePane = workspaceTabList.closest(".workspace-pane");
        if (!(workspacePane instanceof HTMLElement)) {
          throw new Error("Workspace pane not found");
        }

        await act(async () => {
          fireEvent.dragEnter(workspacePane, {
            clientX: 240,
            clientY: 220,
            dataTransfer: reducedMimeDataTransfer,
          });
          fireEvent.dragOver(workspacePane, {
            clientX: 240,
            clientY: 220,
            dataTransfer: reducedMimeDataTransfer,
          });
          fireEvent.drop(workspacePane, {
            clientX: 240,
            clientY: 220,
            dataTransfer: reducedMimeDataTransfer,
          });
          fireEvent.dragEnd(sectionButton, { dataTransfer });
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            screen
              .getAllByRole("tab")
              .some((tab) => /Sessions/i.test(tab.textContent ?? "")),
          ).toBe(true);
        });
      } finally {
        context.cleanup();
      }
    });
  });
  it("drops control panel dock sections into the tab rail without splitting the pane", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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

          if (requestUrl.pathname === "/api/git/status") {
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

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }

        const sessionRowLabel =
          await within(sessionList).findByText("Session 1");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }

        await clickAndSettle(sessionRowButton);

        const initialTabLists = screen.getAllByRole("tablist", {
          name: "Tile tabs",
        });
        expect(initialTabLists).toHaveLength(2);

        const workspaceTabList = initialTabLists.find((tabList) =>
          /Session 1/i.test(tabList.textContent ?? ""),
        );
        if (!(workspaceTabList instanceof HTMLDivElement)) {
          throw new Error("Workspace tab list not found");
        }
        const workspaceTabRail = workspaceTabList;

        async function dragDockSectionToTabRail(
          buttonName: string,
          expectedTabLabel: RegExp,
        ) {
          const dock = await screen.findByRole("navigation", {
            name: "Control panel dock",
          });
          const sectionButton = await within(dock).findByRole("button", {
            name: buttonName,
          });
          const dataTransfer = createDragDataTransfer();

          await act(async () => {
            fireEvent.dragStart(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          await act(async () => {
            fireEvent.dragEnter(workspaceTabRail, {
              clientX: 200,
              dataTransfer,
            });
            fireEvent.dragOver(workspaceTabRail, {
              clientX: 200,
              dataTransfer,
            });
            fireEvent.drop(workspaceTabRail, { clientX: 200, dataTransfer });
            fireEvent.dragEnd(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          await waitFor(() => {
            const tabLists = screen.getAllByRole("tablist", {
              name: "Tile tabs",
            });
            expect(tabLists).toHaveLength(2);
            const updatedWorkspaceTabList = tabLists.find((tabList) =>
              /Session 1/i.test(tabList.textContent ?? ""),
            );
            expect(updatedWorkspaceTabList).toBeTruthy();
            expect(updatedWorkspaceTabList?.textContent ?? "").toMatch(
              expectedTabLabel,
            );
          });
        }

        await dragDockSectionToTabRail("Sessions", /^.*Sessions.*$/i);
        await dragDockSectionToTabRail("Orchestrators", /^.*Orchestrators.*$/i);
        await dragDockSectionToTabRail("Files", /Files:\s*termal/i);
        await dragDockSectionToTabRail("Git status", /Git:\s*termal/i);
      } finally {
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("keeps the control panel project aligned with the session when selecting a project-scoped tab", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
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

          if (requestUrl.pathname === "/api/git/status") {
            const repoPath = requestUrl.searchParams.get("path") ?? "";
            const repoSegments = repoPath.split("/").filter(Boolean);
            const repoName =
              repoSegments[repoSegments.length - 1] ?? "workspace";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: repoPath,
              upstream: "origin/main",
              workdir: repoPath,
              statusMessage: `${repoName} ready`,
            });
          }

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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

      function getSessionTablist() {
        const sessionTablist = screen
          .getAllByRole("tablist")
          .find((candidate) => within(candidate).queryByText("Session 1"));

        if (!sessionTablist) {
          throw new Error("Session tablist not found");
        }

        return sessionTablist;
      }

      function getControlPanelShell() {
        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        return controlPanelShell;
      }

      async function selectControlPanelProject(optionName: string | RegExp) {
        const combobox = within(getControlPanelShell()).getByRole("combobox", {
          name: "Project",
        });
        await clickAndSettle(combobox);

        const listbox = await screen.findByRole("listbox");
        const option = within(listbox)
          .getAllByRole("option")
          .find((candidate) => {
            const label =
              candidate
                .querySelector(".combo-option-label")
                ?.textContent?.trim() ??
              candidate.textContent?.trim() ??
              "";

            return typeof optionName === "string"
              ? label === optionName
              : optionName.test(label);
          });

        if (!option) {
          throw new Error(
            `Control panel project option not found for ${String(optionName)}`,
          );
        }

        await clickAndSettle(option);
      }

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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }

        const sessionRowLabel =
          await within(sessionList).findByText("Session 1");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }

        await clickAndSettle(sessionRowButton);
        await clickAndSettle(
          await screen.findByRole("button", { name: "Git status" }),
        );

        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");

        await selectControlPanelProject(/^API$/i);
        await clickAndSettle(
          within(getControlPanelShell()).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );

        const sessionTablist = getSessionTablist();
        expect(
          within(sessionTablist).getByText("Git: api"),
        ).toBeInTheDocument();

        await selectControlPanelProject(/^TermAl$/i);
        await clickAndSettle(
          within(getControlPanelShell()).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );
        expect(
          within(sessionTablist).getByText("Git: termal"),
        ).toBeInTheDocument();
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");

        await clickAndSettle(
          within(sessionTablist).getByRole("tab", { name: /Git: api/i }),
        );
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");
      } finally {
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("persists the nearest session context when selecting the control panel tab", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
              ],
            });
          }

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-control-panel-tab-context-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-control-panel-tab-context-sync",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.22,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                type: "pane",
                paneId: "pane-review",
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
          },
        }),
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          screen.getByRole("tab", { name: /Control panel/i }),
        );

        await waitFor(() => {
          const persistedLayoutRaw = window.localStorage.getItem(
            "termal-workspace-layout:test-control-panel-tab-context-sync",
          );
          expect(persistedLayoutRaw).not.toBeNull();
          const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
            workspace: {
              activePaneId: string | null;
              panes: Array<{
                id: string;
                tabs: Array<Record<string, unknown>>;
              }>;
            };
          };
          const persistedControlPane = persistedLayout.workspace.panes.find(
            (pane) => pane.id === "pane-control",
          );

          expect(persistedLayout.workspace.activePaneId).toBe("pane-control");
          expect(persistedControlPane?.tabs).toContainEqual({
            id: "tab-control",
            kind: "controlPanel",
            originSessionId: "session-2",
            originProjectId: "project-api",
          });
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("opens canvas from the control panel using the pane-local session context", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
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

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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
      function getTablistForSession(name: string) {
        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) => within(candidate).queryByText(name));

        if (!tablist) {
          throw new Error(`Tablist not found for session ${name}`);
        }

        return tablist;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-pane-local-control-panel",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-pane-local-control-panel",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.22,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-content",
                type: "split",
                direction: "row",
                ratio: 0.5,
                first: {
                  type: "pane",
                  paneId: "pane-main",
                },
                second: {
                  type: "pane",
                  paneId: "pane-review",
                },
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: null,
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-main",
                tabs: [
                  {
                    id: "tab-main",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-canvas",
                    kind: "canvas",
                    cards: [],
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-main",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
          },
        }),
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        expect(
          within(getTablistForSession("Main")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Review")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Canvas" }),
        );

        expect(
          within(getTablistForSession("Main")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Review")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("moves an existing shared canvas into the new launch context and syncs its pane state", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/fs") {
            const path = requestUrl.searchParams.get("path") ?? "";
            const segments = path.split("/").filter(Boolean);
            const name = segments[segments.length - 1] ?? "workspace";
            return jsonResponse({
              entries: [],
              name,
              path,
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            const path = requestUrl.searchParams.get("path") ?? "/projects/api";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: path,
              upstream: "origin/main",
              workdir: path,
            });
          }

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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
      function getTablistForSession(name: string) {
        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) => within(candidate).queryByText(name));

        if (!tablist) {
          throw new Error(`Tablist not found for session ${name}`);
        }

        return tablist;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-canvas-relocation-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-canvas-relocation-sync",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.22,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-content",
                type: "split",
                direction: "row",
                ratio: 0.45,
                first: {
                  type: "pane",
                  paneId: "pane-review",
                },
                second: {
                  type: "pane",
                  paneId: "pane-main",
                },
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: null,
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-main",
                tabs: [
                  {
                    id: "tab-main",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-canvas",
                    kind: "canvas",
                    cards: [],
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-main",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
          },
        }),
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        expect(
          within(getTablistForSession("Main")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Review")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Canvas" }),
        );

        expect(
          within(getTablistForSession("Review")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Main")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();

        const persistedLayoutRaw = window.localStorage.getItem(
          "termal-workspace-layout:test-canvas-relocation-sync",
        );
        expect(persistedLayoutRaw).not.toBeNull();
        const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
          workspace: {
            panes: Array<{
              id: string;
              activeTabId: string | null;
              activeSessionId: string | null;
              tabs: Array<Record<string, unknown>>;
            }>;
          };
        };
        const persistedReviewPane = persistedLayout.workspace.panes.find(
          (pane) => pane.id === "pane-review",
        );
        const persistedMainPane = persistedLayout.workspace.panes.find(
          (pane) => pane.id === "pane-main",
        );

        expect(persistedReviewPane?.activeTabId).toBe("tab-canvas");
        expect(persistedReviewPane?.activeSessionId).toBe("session-2");
        expect(persistedReviewPane?.tabs).toContainEqual({
          id: "tab-canvas",
          kind: "canvas",
          cards: [],
          originSessionId: "session-2",
          originProjectId: "project-api",
        });
        expect(persistedMainPane?.tabs).toEqual([
          {
            id: "tab-main",
            kind: "session",
            sessionId: "session-1",
          },
        ]);
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("re-scopes standalone Files and Git panes to the nearest session when selected", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/fs") {
            const path = requestUrl.searchParams.get("path") ?? "";
            const segments = path.split("/").filter(Boolean);
            const name = segments[segments.length - 1] ?? "workspace";
            return jsonResponse({
              entries: [],
              name,
              path,
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            const path = requestUrl.searchParams.get("path") ?? "";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: path,
              upstream: "origin/main",
              workdir: path,
            });
          }

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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

      function getControlPanelShell() {
        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        return controlPanelShell;
      }

      function getPaneByTabName(name: string | RegExp) {
        const tab = screen.getByRole("tab", { name });
        const pane = tab.closest(".workspace-pane");
        if (!(pane instanceof HTMLElement)) {
          throw new Error(`Workspace pane not found for ${String(name)}`);
        }

        return pane;
      }

      function latestRequestTo(pathname: string) {
        const requests = fetchMock.mock.calls
          .map(([input]) => new URL(String(input), "http://localhost"))
          .filter((requestUrl) => requestUrl.pathname === pathname);
        const request = requests[requests.length - 1];
        if (!request) {
          throw new Error(`No request captured for ${pathname}`);
        }

        return request;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-standalone-control-surface-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-standalone-control-surface-sync",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.18,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-right-1",
                type: "split",
                direction: "row",
                ratio: 0.33,
                first: {
                  type: "pane",
                  paneId: "pane-files",
                },
                second: {
                  id: "split-right-2",
                  type: "split",
                  direction: "row",
                  ratio: 0.5,
                  first: {
                    type: "pane",
                    paneId: "pane-git",
                  },
                  second: {
                    type: "pane",
                    paneId: "pane-review",
                  },
                },
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: null,
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
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
              {
                id: "pane-git",
                tabs: [
                  {
                    id: "tab-git",
                    kind: "gitStatus",
                    workdir: "/projects/termal",
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-git",
                activeSessionId: "session-1",
                viewMode: "gitStatus",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
          },
        }),
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          screen.getByRole("tab", { name: /Files: termal/i }),
        );

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Files: api/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        expect(
          within(getPaneByTabName(/Files: api/i)).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        await waitFor(() => {
          const request = latestRequestTo("/api/fs");
          expect(request.searchParams.get("path")).toBe("/projects/api");
          expect(request.searchParams.get("sessionId")).toBe("session-2");
          expect(request.searchParams.get("projectId")).toBe("project-api");
        });

        await clickAndSettle(screen.getByRole("tab", { name: /Git: termal/i }));

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Git: api/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        expect(
          within(getPaneByTabName(/Git: api/i)).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        await waitFor(() => {
          const request = latestRequestTo("/api/git/status");
          expect(request.searchParams.get("path")).toBe("/projects/api");
          expect(request.searchParams.get("sessionId")).toBe("session-2");
          expect(request.searchParams.get("projectId")).toBe("project-api");
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("re-scopes a standalone Files pane to a projectless nearest session and resets the control panel filter", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
                makeSession("session-2", {
                  name: "Workspace Only",
                  projectId: null,
                  workdir: "/workspace/review-only",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/fs") {
            const path = requestUrl.searchParams.get("path") ?? "";
            const segments = path.split("/").filter(Boolean);
            const name = segments[segments.length - 1] ?? "workspace";
            return jsonResponse({
              entries: [],
              name,
              path,
            });
          }

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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

      function getControlPanelShell() {
        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        return controlPanelShell;
      }

      function getPaneByTabName(name: string | RegExp) {
        const tab = screen.getByRole("tab", { name });
        const pane = tab.closest(".workspace-pane");
        if (!(pane instanceof HTMLElement)) {
          throw new Error(`Workspace pane not found for ${String(name)}`);
        }

        return pane;
      }

      function latestRequestTo(pathname: string) {
        const requests = fetchMock.mock.calls
          .map(([input]) => new URL(String(input), "http://localhost"))
          .filter((requestUrl) => requestUrl.pathname === pathname);
        const request = requests[requests.length - 1];
        if (!request) {
          throw new Error(`No request captured for ${pathname}`);
        }

        return request;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-projectless-standalone-control-surface-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-projectless-standalone-control-surface-sync",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.18,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-right",
                type: "split",
                direction: "row",
                ratio: 0.45,
                first: {
                  type: "pane",
                  paneId: "pane-files",
                },
                second: {
                  type: "pane",
                  paneId: "pane-workspace",
                },
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: null,
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-files",
                tabs: [
                  {
                    id: "tab-files",
                    kind: "filesystem",
                    rootPath: "/projects/api",
                    originSessionId: "session-1",
                    originProjectId: "project-api",
                  },
                ],
                activeTabId: "tab-files",
                activeSessionId: "session-1",
                viewMode: "filesystem",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-workspace",
                tabs: [
                  {
                    id: "tab-workspace",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-workspace",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-workspace",
          },
        }),
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        );
        const projectListbox = await screen.findByRole("listbox");
        const apiOption = within(projectListbox)
          .getAllByRole("option")
          .find((candidate) => {
            const label =
              candidate
                .querySelector(".combo-option-label")
                ?.textContent?.trim() ??
              candidate.textContent?.trim() ??
              "";
            return /^API$/i.test(label);
          });
        if (!apiOption) {
          throw new Error("Project option not found for API");
        }
        await clickAndSettle(apiOption);
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");

        await clickAndSettle(screen.getByRole("tab", { name: /Files: api/i }));

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Files: review-only/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("All projects");
        expect(
          within(getPaneByTabName(/Files: review-only/i)).getByDisplayValue(
            "/workspace/review-only",
          ),
        ).toBeInTheDocument();
        await waitFor(() => {
          const request = latestRequestTo("/api/fs");
          expect(request.searchParams.get("path")).toBe(
            "/workspace/review-only",
          );
          expect(request.searchParams.get("sessionId")).toBe("session-2");
          expect(request.searchParams.has("projectId")).toBe(false);
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("opens Files and Git status from the control panel using the pane-local session context", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/fs") {
            const path = requestUrl.searchParams.get("path") ?? "";
            const segments = path.split("/").filter(Boolean);
            const name = segments[segments.length - 1] ?? "workspace";
            return jsonResponse({
              entries: [],
              name,
              path,
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            const path = requestUrl.searchParams.get("path") ?? "";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: path,
              upstream: "origin/main",
              workdir: path,
            });
          }

          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
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

      function getControlPanelShell() {
        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        return controlPanelShell;
      }

      function latestRequestTo(pathname: string) {
        const requests = fetchMock.mock.calls
          .map(([input]) => new URL(String(input), "http://localhost"))
          .filter((requestUrl) => requestUrl.pathname === pathname);
        const request = requests[requests.length - 1];
        if (!request) {
          throw new Error(`No request captured for ${pathname}`);
        }

        return request;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-pane-local-control-panel-files-git",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-pane-local-control-panel-files-git",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.22,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-content",
                type: "split",
                direction: "row",
                ratio: 0.5,
                first: {
                  type: "pane",
                  paneId: "pane-main",
                },
                second: {
                  type: "pane",
                  paneId: "pane-review",
                },
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: null,
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-main",
                tabs: [
                  {
                    id: "tab-main",
                    kind: "session",
                    sessionId: "session-1",
                  },
                ],
                activeTabId: "tab-main",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
          },
        }),
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
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const controlPanelShell = getControlPanelShell();

        expect(
          screen.queryByRole("tab", { name: /Files: termal/i }),
        ).toBeNull();
        expect(screen.queryByRole("tab", { name: /Files: api/i })).toBeNull();

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Files" }),
        );
        await clickAndSettle(
          within(controlPanelShell).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Files: termal/i }),
          ).toBeInTheDocument();
        });
        expect(screen.queryByRole("tab", { name: /Files: api/i })).toBeNull();
        await waitFor(() => {
          const request = latestRequestTo("/api/fs");
          expect(request.searchParams.get("path")).toBe("/projects/termal");
          expect(request.searchParams.get("sessionId")).toBe("session-1");
          expect(request.searchParams.get("projectId")).toBe("project-termal");
        });

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Git status" }),
        );
        await clickAndSettle(
          within(controlPanelShell).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Git: termal/i }),
          ).toBeInTheDocument();
        });
        expect(screen.queryByRole("tab", { name: /Git: api/i })).toBeNull();
        await waitFor(() => {
          const request = latestRequestTo("/api/git/status");
          expect(request.searchParams.get("path")).toBe("/projects/termal");
          expect(request.searchParams.get("sessionId")).toBe("session-1");
          expect(request.searchParams.get("projectId")).toBe("project-termal");
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("keeps the control panel divider resizable when a session pane is open", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              preview: "Ready for a prompt.",
            }),
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
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
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();
      const eventSource = latestEventSource();
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchError();
      });
      await settleAsyncUi();
      await clickAndSettle(
        await screen.findByRole("button", { name: "Sessions" }),
      );

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

      expect(document.querySelector(".tile-divider-row")).not.toBeNull();
      expect(document.querySelector(".tile-divider-row.fixed")).toBeNull();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("uses the control panel pixel minimum instead of the generic row split clamp", () => {
    document.documentElement.style.setProperty(
      "--control-panel-pane-min-width",
      "14rem",
    );

    const bounds = getWorkspaceSplitResizeBounds(
      {
        id: "split-1",
        type: "split",
        direction: "row",
        ratio: 0.24,
        first: {
          type: "pane",
          paneId: "control-panel-pane",
        },
        second: {
          type: "pane",
          paneId: "session-pane",
        },
      },
      "split-1",
      "row",
      1600,
      new Map([
        [
          "control-panel-pane",
          {
            id: "control-panel-pane",
            tabs: [
              {
                id: "control-panel-tab",
                kind: "controlPanel",
                originSessionId: null,
              },
            ],
            activeTabId: "control-panel-tab",
            activeSessionId: null,
            viewMode: "controlPanel",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        [
          "session-pane",
          {
            id: "session-pane",
            tabs: [
              {
                id: "session-tab",
                kind: "session",
                sessionId: "session-1",
              },
            ],
            activeTabId: "session-tab",
            activeSessionId: "session-1",
            viewMode: "session",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
      ]),
    );

    expect(bounds.minRatio).toBeCloseTo(14 / 100, 4);
    expect(bounds.maxRatio).toBeCloseTo(78 / 100, 4);
  });

  it("matches the standalone control panel width when resolving the initial dock ratio", () => {
    const previousPaneWidth = document.documentElement.style.getPropertyValue(
      "--control-panel-pane-width",
    );
    document.documentElement.style.setProperty(
      "--control-panel-pane-width",
      "23rem",
    );

    const workspaceStage = document.createElement("div");
    workspaceStage.className =
      "workspace-stage workspace-stage-control-panel-only";
    Object.defineProperty(workspaceStage, "clientWidth", {
      configurable: true,
      value: 1200,
    });
    document.body.appendChild(workspaceStage);

    try {
      expect(resolveStandaloneControlPanelDockWidthRatio(0.24)).toBeCloseTo(
        (23 * 16) / 1200,
        5,
      );
    } finally {
      workspaceStage.remove();
      if (previousPaneWidth) {
        document.documentElement.style.setProperty(
          "--control-panel-pane-width",
          previousPaneWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--control-panel-pane-width",
        );
      }
    }
  });

  it("clamps the initial dock ratio when the standalone width would crowd out the session pane", () => {
    const previousPaneWidth = document.documentElement.style.getPropertyValue(
      "--control-panel-pane-width",
    );
    const previousPaneMinWidth =
      document.documentElement.style.getPropertyValue(
        "--control-panel-pane-min-width",
      );
    document.documentElement.style.setProperty(
      "--control-panel-pane-width",
      "23rem",
    );
    document.documentElement.style.setProperty(
      "--control-panel-pane-min-width",
      "20rem",
    );

    const workspaceStage = document.createElement("div");
    workspaceStage.className =
      "workspace-stage workspace-stage-control-panel-only";
    Object.defineProperty(workspaceStage, "clientWidth", {
      configurable: true,
      value: 400,
    });
    document.body.appendChild(workspaceStage);

    try {
      expect(resolveStandaloneControlPanelDockWidthRatio(0.24)).toBeCloseTo(
        (20 * 16) / (20 * 16 + 400 * 0.22),
        5,
      );
    } finally {
      workspaceStage.remove();
      if (previousPaneWidth) {
        document.documentElement.style.setProperty(
          "--control-panel-pane-width",
          previousPaneWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--control-panel-pane-width",
        );
      }
      if (previousPaneMinWidth) {
        document.documentElement.style.setProperty(
          "--control-panel-pane-min-width",
          previousPaneMinWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--control-panel-pane-min-width",
        );
      }
    }
  });
  it("shows a Codex notice when live model refresh resets reasoning effort after session creation", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const createSessionDeferred = createDeferred<{
        sessionId: string;
        state: Awaited<ReturnType<typeof api.fetchState>>;
      }>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => fetchStateDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
      const refreshSessionModelOptionsSpy = vi
        .spyOn(api, "refreshSessionModelOptions")
        .mockImplementation(() => refreshSessionModelOptionsDeferred.promise);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        await act(async () => {
          fetchStateDeferred.resolve({
            revision: 1,
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );
        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalled();
        });
        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-1",
            state: {
              revision: 2,
              projects: [],
              sessions: [
                {
                  id: "session-1",
                  name: "Codex 1",
                  emoji: "O",
                  agent: "Codex",
                  workdir: "/tmp",
                  model: "gpt-5-codex-mini",
                  approvalPolicy: "never",
                  reasoningEffort: "minimal",
                  sandboxMode: "workspace-write",
                  status: "idle",
                  preview: "Ready for a prompt.",
                  messages: [],
                },
              ],
            },
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve({
            revision: 3,
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Codex 1",
                emoji: "O",
                agent: "Codex",
                workdir: "/tmp",
                model: "gpt-5-codex-mini",
                modelOptions: [
                  {
                    label: "GPT-5 Codex Mini",
                    value: "gpt-5-codex-mini",
                    description:
                      "Optimized for codex. Cheaper, faster, but less capable.",
                    defaultReasoningEffort: "medium",
                    supportedReasoningEfforts: ["medium", "high"],
                  },
                ],
                approvalPolicy: "never",
                reasoningEffort: "medium",
                sandboxMode: "workspace-write",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          });
          await flushUiWork();
        });
        await clickAndSettle(
          await screen.findByRole("button", { name: "Prompt" }),
        );

        await waitFor(() => {
          expect(
            screen.getByText(
              "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
            ),
          ).toBeInTheDocument();
        });
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("applies the configured Codex reasoning effort to new Codex sessions", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const updateSettingsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const createSessionDeferred = createDeferred<{
        sessionId: string;
        state: Awaited<ReturnType<typeof api.fetchState>>;
      }>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => fetchStateDeferred.promise);
      const updateAppSettingsSpy = vi
        .spyOn(api, "updateAppSettings")
        .mockImplementation(() => updateSettingsDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
      const refreshSessionModelOptionsSpy = vi
        .spyOn(api, "refreshSessionModelOptions")
        .mockImplementation(() => refreshSessionModelOptionsDeferred.promise);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        await act(async () => {
          fetchStateDeferred.resolve({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
            },
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(
          screen.getByRole("tab", { name: "Codex defaults" }),
        );
        await selectComboboxOption("Default reasoning effort", /high/i);
        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultCodexReasoningEffort: "high",
          });
        });
        await act(async () => {
          updateSettingsDeferred.resolve({
            revision: 2,
            preferences: {
              defaultCodexReasoningEffort: "high",
              defaultClaudeEffort: "default",
            },
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });
        await clickAndSettle(
          screen.getByRole("button", { name: "Close dialog" }),
        );

        await openCreateSessionDialog();
        await settleAsyncUi();
        expect(
          screen.getByRole("combobox", { name: "Codex reasoning effort" }),
        ).toHaveTextContent("high");
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledWith(
            expect.objectContaining({
              agent: "Codex",
              reasoningEffort: "high",
            }),
          );
        });
        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-1",
            state: {
              revision: 3,
              preferences: {
                defaultCodexReasoningEffort: "high",
                defaultClaudeEffort: "default",
              },
              projects: [],
              sessions: [
                {
                  id: "session-1",
                  name: "Codex 1",
                  emoji: "O",
                  agent: "Codex",
                  workdir: "/tmp",
                  model: "gpt-5.4",
                  approvalPolicy: "never",
                  reasoningEffort: "high",
                  sandboxMode: "workspace-write",
                  status: "idle",
                  preview: "Ready for a prompt.",
                  messages: [],
                },
              ],
            },
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve({
            revision: 4,
            preferences: {
              defaultCodexReasoningEffort: "high",
              defaultClaudeEffort: "default",
            },
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Codex 1",
                emoji: "O",
                agent: "Codex",
                workdir: "/tmp",
                model: "gpt-5.4",
                approvalPolicy: "never",
                reasoningEffort: "high",
                sandboxMode: "workspace-write",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          });
          await flushUiWork();
        });
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        updateAppSettingsSpy.mockRestore();
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("applies the configured Claude effort to new Claude sessions", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const updateSettingsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const createSessionDeferred = createDeferred<{
        sessionId: string;
        state: Awaited<ReturnType<typeof api.fetchState>>;
      }>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => fetchStateDeferred.promise);
      const updateAppSettingsSpy = vi
        .spyOn(api, "updateAppSettings")
        .mockImplementation(() => updateSettingsDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
      const refreshSessionModelOptionsSpy = vi
        .spyOn(api, "refreshSessionModelOptions")
        .mockImplementation(() => refreshSessionModelOptionsDeferred.promise);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      try {
        await renderApp();
        await act(async () => {
          fetchStateDeferred.resolve({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
            },
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(
          screen.getByRole("tab", { name: "Claude defaults" }),
        );
        await selectComboboxOption("Default Claude effort", /max/i);
        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultClaudeEffort: "max",
          });
        });
        await act(async () => {
          updateSettingsDeferred.resolve({
            revision: 2,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "max",
            },
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });
        await clickAndSettle(
          screen.getByRole("button", { name: "Close dialog" }),
        );

        await openCreateSessionDialog();
        await settleAsyncUi();
        await selectComboboxOption("Assistant", /^Claude$/i);
        expect(
          screen.getByRole("combobox", { name: "Claude effort" }),
        ).toHaveTextContent("max");
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledWith(
            expect.objectContaining({
              agent: "Claude",
              claudeEffort: "max",
            }),
          );
        });
        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-1",
            state: {
              revision: 3,
              preferences: {
                defaultCodexReasoningEffort: "medium",
                defaultClaudeEffort: "max",
              },
              projects: [],
              sessions: [
                {
                  id: "session-1",
                  name: "Claude 1",
                  emoji: "C",
                  agent: "Claude",
                  workdir: "/tmp",
                  model: "claude-sonnet-4-20250514",
                  claudeApprovalMode: "ask",
                  claudeEffort: "max",
                  status: "idle",
                  preview: "Ready for a prompt.",
                  messages: [],
                },
              ],
            },
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve({
            revision: 4,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "max",
            },
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Claude 1",
                emoji: "C",
                agent: "Claude",
                workdir: "/tmp",
                model: "claude-sonnet-4-20250514",
                claudeApprovalMode: "ask",
                claudeEffort: "max",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          });
          await flushUiWork();
        });
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        fetchStateSpy.mockRestore();
        updateAppSettingsSpy.mockRestore();
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("keeps unsaved remote draft edits across unrelated state refreshes", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      const remotes = [
        {
          id: "local",
          name: "Local",
          transport: "local" as const,
          enabled: true,
          host: null,
          port: null,
          user: null,
        },
        {
          id: "ssh-lab",
          name: "SSH Lab",
          transport: "ssh" as const,
          enabled: true,
          host: "example.com",
          port: 22,
          user: "alice",
        },
      ];

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
              remotes,
            },
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                preview: "Initial preview",
              }),
            ],
          });
          await flushUiWork();
        });
        await screen.findAllByText("Codex Session");

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(screen.getByRole("tab", { name: "Remotes" }));
        await screen.findByRole("heading", {
          level: 3,
          name: "Remote definitions",
        });
        const remoteName = await screen.findByText("SSH Lab");
        const remoteRow = remoteName.closest(".remote-settings-row");
        if (!(remoteRow instanceof HTMLElement)) {
          throw new Error("SSH remote row not found");
        }
        expect(
          within(remoteRow).getByText("Enabled for projects and sessions"),
        ).toBeInTheDocument();

        const hostInput = within(remoteRow).getByDisplayValue("example.com");
        expect(hostInput).toHaveValue("example.com");

        await act(async () => {
          fireEvent.change(hostInput, {
            target: { value: "draft.example.com" },
          });
          await flushUiWork();
        });

        expect(within(remoteRow).getByDisplayValue("draft.example.com")).toBe(
          hostInput,
        );

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 2,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
              remotes,
            },
            projects: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                preview: "Updated preview",
              }),
            ],
          });
          await flushUiWork();
        });

        expect(within(remoteRow).getByDisplayValue("draft.example.com")).toBe(
          hostInput,
        );
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("routes current-workspace session creation through the active remote project", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const createSessionDeferred = createDeferred<{
        sessionId: string;
        state: Awaited<ReturnType<typeof api.fetchState>>;
      }>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
      const refreshSessionModelOptionsSpy = vi
        .spyOn(api, "refreshSessionModelOptions")
        .mockImplementation(() => refreshSessionModelOptionsDeferred.promise);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
      HTMLElement.prototype.scrollIntoView = vi.fn();

      const remotes = [
        {
          id: "local",
          name: "Local",
          transport: "local" as const,
          enabled: true,
          host: null,
          port: null,
          user: null,
        },
        {
          id: "ssh-lab",
          name: "SSH Lab",
          transport: "ssh" as const,
          enabled: true,
          host: "example.com",
          port: 22,
          user: "alice",
        },
      ];
      const projects = [
        {
          id: "project-remote",
          name: "Remote Project",
          rootPath: "/remote/repo",
          remoteId: "ssh-lab",
        },
      ];

      try {
        await renderApp();
        const eventSource = latestEventSource();
        expect(eventSource).toBeTruthy();

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
              remotes,
            },
            projects,
            sessions: [
              makeSession("session-1", {
                name: "Remote Session",
                workdir: "/remote/repo/subdir",
                projectId: "project-remote",
              }),
            ],
          });
          await flushUiWork();
        });
        await screen.findAllByText("Remote Session");

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowLabel =
          await within(sessionList).findByText("Remote Session");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!sessionRowButton) {
          throw new Error("Remote session row button not found");
        }
        await clickAndSettle(sessionRowButton);

        await openCreateSessionDialog();
        const createSessionDialog = screen.getByRole("dialog", {
          name: "New session",
        });
        const projectCombobox = within(createSessionDialog).getByRole(
          "combobox",
          {
            name: "Project",
          },
        );
        await clickAndSettle(projectCombobox);
        const projectListbox = await screen.findByRole("listbox");
        const currentWorkspaceOption = within(projectListbox)
          .getAllByRole("option")
          .find((candidate) =>
            /^Current workspace$/i.test(
              candidate
                .querySelector(".combo-option-label")
                ?.textContent?.trim() ??
                candidate.textContent?.trim() ??
                "",
            ),
          );
        if (!currentWorkspaceOption) {
          throw new Error("Current workspace option not found");
        }
        await clickAndSettle(currentWorkspaceOption);
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledWith(
            expect.objectContaining({
              projectId: "project-remote",
              workdir: undefined,
            }),
          );
        });

        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-2",
            state: {
              revision: 2,
              preferences: {
                defaultCodexReasoningEffort: "medium",
                defaultClaudeEffort: "default",
                remotes,
              },
              projects,
              sessions: [
                makeSession("session-1", {
                  name: "Remote Session",
                  workdir: "/remote/repo/subdir",
                  projectId: "project-remote",
                }),
                makeSession("session-2", {
                  name: "Codex 2",
                  workdir: "/remote/repo",
                  projectId: "project-remote",
                }),
              ],
            },
          });
          await flushUiWork();
        });

        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-2",
          );
        });

        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve({
            revision: 3,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
              remotes,
            },
            projects,
            sessions: [
              makeSession("session-1", {
                name: "Remote Session",
                workdir: "/remote/repo/subdir",
                projectId: "project-remote",
              }),
              makeSession("session-2", {
                name: "Codex 2",
                workdir: "/remote/repo",
                projectId: "project-remote",
              }),
            ],
          });
          await flushUiWork();
        });

        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("separates theme selection from editor and UI appearance controls in preferences", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
    const fetchStateSpy = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => fetchStateDeferred.promise);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          sessions: [],
        });
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );

      expect(
        screen.getByRole("radiogroup", { name: "UI theme" }),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("radiogroup", { name: "UI style" }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("heading", { level: 3, name: "Font sizes" }),
      ).not.toBeInTheDocument();

      await clickAndSettle(
        screen.getByRole("tab", { name: "Editor & UI appearance" }),
      );

      expect(
        screen.getByRole("heading", { level: 3, name: "Font sizes" }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("radiogroup", { name: "UI theme" }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("radiogroup", { name: "UI style" }),
      ).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      fetchStateSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("persists UI density changes from the appearance preferences", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
    const fetchStateSpy = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => fetchStateDeferred.promise);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    window.localStorage.clear();
    document.documentElement.style.removeProperty("--density-scale");

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          sessions: [],
        });
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );
      await clickAndSettle(
        screen.getByRole("tab", { name: "Editor & UI appearance" }),
      );

      const densitySlider = screen.getByRole("slider", { name: "UI density" });
      expect((densitySlider as HTMLInputElement).value).toBe("100");

      await act(async () => {
        fireEvent.change(densitySlider, { target: { value: "85" } });
      });
      await settleAsyncUi();

      expect(
        document.documentElement.style.getPropertyValue("--density-scale"),
      ).toBe("0.85");
      expect(window.localStorage.getItem("termal-ui-density")).toBe("85");
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      fetchStateSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("persists UI style changes from the themes preferences", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
    const fetchStateSpy = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => fetchStateDeferred.promise);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-ui-style");

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          sessions: [],
        });
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );
      const styleGroup = screen.getByRole("radiogroup", { name: "UI style" });
      await clickAndSettle(
        within(styleGroup).getByRole("radio", { name: /Blueprint/i }),
      );

      expect(document.documentElement.dataset.uiStyle).toBe("blueprint-style");
      expect(window.localStorage.getItem("termal-ui-style")).toBe(
        "blueprint-style",
      );
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      fetchStateSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("warns once before sending with an unknown model, then allows the retry", () => {
    const session = makeSession("session-1", {
      agent: "Codex",
      model: "gpt-5.5-preview",
      modelOptions: [{ label: "GPT-5.4", value: "gpt-5.4" }],
    });

    const firstAttempt = resolveUnknownSessionModelSendAttempt(
      new Set(),
      session,
    );
    expect(firstAttempt.allowSend).toBe(false);
    expect(firstAttempt.warning).toBe(
      "Codex is set to gpt-5.5-preview, but that model is not in the current live list. Refresh models to verify it, or send the prompt again to continue anyway.",
    );

    const secondAttempt = resolveUnknownSessionModelSendAttempt(
      firstAttempt.nextConfirmedKeys,
      session,
    );
    expect(secondAttempt.allowSend).toBe(true);
    expect(secondAttempt.warning).toBeNull();
  });
});
