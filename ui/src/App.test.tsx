import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  StrictMode,
  forwardRef,
  useEffect,
  useImperativeHandle,
  type ForwardedRef,
} from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import App, {
  MarkdownContent,
  ThemedCombobox,
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  resolveRecoveredWorkspaceLayoutRequestError,
  resolveAdoptedStateSlices,
  resolveControlPanelWorkspaceRoot,
  resolveControlSurfaceSectionIdForWorkspaceTab,
  resolveSettledScrollMinimumAttempts,
  describeUnknownSessionModelWarning,
  resolveUnknownSessionModelSendAttempt,
  setAppTestHooksForTests,
  syncMessageStackScrollPosition,
} from "./App";
import { resolveStandaloneControlPanelDockWidthRatio } from "./control-panel-layout";
import {
  buildControlSurfaceSessionListEntries,
  formatSessionOrchestratorGroupName,
} from "./control-surface-state";
import { collectRestoredGitDiffDocumentContentRefreshes } from "./git-diff-refresh";
import { getWorkspaceSplitResizeBounds } from "./workspace-queries";
import {
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
} from "./live-updates";
import type { AgentReadiness, OrchestratorInstance, Session } from "./types";
import * as workspaceStorage from "./workspace-storage";
import { WORKSPACE_LAYOUT_STORAGE_KEY } from "./workspace-storage";
import type { WorkspaceState, WorkspaceTab } from "./workspace";

vi.mock("./MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function MonacoDiffEditorMock(
    {
      modifiedValue,
      onChange,
      onSave,
      onStatusChange,
      originalValue,
      readOnly = true,
    }: {
      modifiedValue: string;
      onChange?: (value: string) => void;
      onSave?: () => void;
      onStatusChange?: (status: {
        line: number;
        column: number;
        tabSize: number;
        insertSpaces: boolean;
        endOfLine: "LF" | "CRLF";
        changeCount: number;
        currentChange: number;
      }) => void;
      originalValue: string;
      readOnly?: boolean;
    },
    ref: ForwardedRef<{
      getScrollTop: () => number;
      goToNextChange: () => void;
      goToPreviousChange: () => void;
      setScrollTop: (scrollTop: number) => void;
    }>,
  ) {
    useImperativeHandle(ref, () => ({
      getScrollTop: () => 0,
      goToNextChange: () => {},
      goToPreviousChange: () => {},
      setScrollTop: () => {},
    }));

    useEffect(() => {
      onStatusChange?.({
        line: 1,
        column: 1,
        tabSize: 2,
        insertSpaces: true,
        endOfLine: "LF",
        changeCount: 2,
        currentChange: 1,
      });
    }, [onStatusChange]);

    return (
      <div>
        <div data-testid="monaco-diff-editor">{`${originalValue}=>${modifiedValue}`}</div>
        <textarea
          data-testid="monaco-diff-editor-modified"
          readOnly={readOnly}
          value={modifiedValue}
          onChange={(event) => onChange?.(event.target.value)}
        />
        <button type="button" onClick={() => onSave?.()}>
          Mock diff save
        </button>
      </div>
    );
  }),
}));

vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function MonacoCodeEditorMock(
    {
      onChange,
      onSave,
      onStatusChange,
      value,
    }: {
      onChange?: (value: string) => void;
      onSave?: () => void;
      onStatusChange?: (status: {
        line: number;
        column: number;
        tabSize: number;
        insertSpaces: boolean;
        endOfLine: "LF" | "CRLF";
      }) => void;
      value: string;
    },
    ref: ForwardedRef<{
      focus: () => void;
      getScrollTop: () => number;
      setScrollTop: (scrollTop: number) => void;
    }>,
  ) {
    useImperativeHandle(ref, () => ({
      focus: () => {},
      getScrollTop: () => 0,
      setScrollTop: () => {},
    }));

    useEffect(() => {
      onStatusChange?.({
        line: 1,
        column: 1,
        tabSize: 2,
        insertSpaces: true,
        endOfLine: "LF",
      });
    }, [onStatusChange]);

    return (
      <textarea
        data-testid="monaco-code-editor"
        value={value}
        onChange={(event) => onChange?.(event.target.value)}
        onKeyDown={(event) => {
          if ((event.ctrlKey || event.metaKey) && event.key === "s") {
            event.preventDefault();
            onSave?.();
          }
        }}
      />
    );
  }),
}));

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

type AppTestStateResponse = Awaited<ReturnType<typeof api.fetchState>>;
type AppTestStateResponseOverrides = Pick<
  AppTestStateResponse,
  "revision" | "projects" | "orchestrators" | "workspaces" | "sessions"
> &
  Partial<
    Pick<AppTestStateResponse, "codex" | "agentReadiness" | "serverInstanceId"> & {
      preferences: Partial<AppTestStateResponse["preferences"]>;
    }
  >;

function makeStateResponse(overrides: AppTestStateResponseOverrides): AppTestStateResponse {
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

function stubScrollIntoView() {
  return vi
    .spyOn(HTMLElement.prototype, "scrollIntoView")
    .mockImplementation(() => {});
}

function mockScrollToAndApplyTop() {
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

function filterScrollToCallsAt(
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

function stubElementScrollGeometry({
  clientHeight,
  scrollHeight,
}: {
  clientHeight: number;
  scrollHeight: number;
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
      return scrollHeight;
    },
  });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get() {
      return clientHeight;
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
    setAppTestHooksForTests(null);
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

      let context:
        | Awaited<ReturnType<typeof renderAppWithProjectAndSession>>
        | null = null;
      try {
        context = await renderAppWithProjectAndSession();
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
        EventSourceMock.instances.splice(priorEventSourceCount);
        context?.cleanup();
      }
    });
  });

  it("opens session find on Ctrl+F even when focused session controls stop propagation", async () => {
    await withSuppressedActWarnings(async () => {
      const context = await renderAppWithProjectAndSession();
      const composer = await screen.findByLabelText("Message Session 1");
      const stopPropagation = (event: KeyboardEvent) => {
        event.stopPropagation();
      };
      composer.addEventListener("keydown", stopPropagation);

      try {
        await act(async () => {
          fireEvent.keyDown(composer, {
            key: "f",
            code: "KeyF",
            ctrlKey: true,
          });
        });
        await settleAsyncUi();

        expect(
          screen.getByRole("search", { name: "Find in session" }),
        ).toBeInTheDocument();
        expect(screen.getByPlaceholderText("Find in session")).toHaveFocus();
      } finally {
        composer.removeEventListener("keydown", stopPropagation);
        context.cleanup();
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

  it("maps workspace tabs to control panel sections", () => {
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-files",
        kind: "filesystem",
        rootPath: "/repo",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("files");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-git",
        kind: "gitStatus",
        workdir: "/repo",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("git");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-orchestrators",
        kind: "orchestratorList",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("orchestrators");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-projects",
        kind: "projectList",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("projects");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-sessions",
        kind: "sessionList",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("sessions");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-source",
        kind: "source",
        path: "/repo/src/main.rs",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBeNull();
  });

  it("collects restored Git diff document refreshes without duplicating transient loading placeholders", () => {
    const durableRequest = {
      path: "docs/README.md",
      sectionId: "unstaged" as const,
      workdir: "/repo",
    };
    const workspace: WorkspaceState = {
      root: {
        type: "pane",
        paneId: "pane-a",
      },
      panes: [
        {
          id: "pane-a",
          activeTabId: "restored",
          activeSessionId: null,
          viewMode: "diffPreview",
          lastSessionViewMode: "session",
          sourcePath: null,
          tabs: [
            {
              id: "transient",
              kind: "diffPreview",
              changeType: "edit",
              diff: "",
              diffMessageId: "git-preview:/repo:unstaged::loading.md",
              filePath: "/repo/loading.md",
              gitDiffRequest: {
                path: "loading.md",
                sectionId: "unstaged",
                workdir: "/repo",
              },
              gitDiffRequestKey: "git-preview:/repo:unstaged::loading.md",
              gitSectionId: "unstaged",
              isLoading: true,
              language: "markdown",
              originSessionId: null,
              summary: "Loading",
            },
            {
              id: "restored",
              kind: "diffPreview",
              changeType: "edit",
              diff: "-before\n+after",
              diffMessageId: "git-preview:/repo:unstaged::docs/README.md",
              filePath: "/repo/docs/README.md",
              gitDiffRequest: durableRequest,
              gitDiffRequestKey: "git-preview:/repo:unstaged::docs/README.md",
              gitSectionId: "unstaged",
              isLoading: true,
              language: "markdown",
              originSessionId: null,
              summary: "Updated README",
            },
            {
              id: "attempted",
              kind: "diffPreview",
              changeType: "edit",
              diff: "-old\n+new",
              diffMessageId: "git-preview:/repo:unstaged::attempted.md",
              filePath: "/repo/attempted.md",
              gitDiffRequest: {
                path: "attempted.md",
                sectionId: "unstaged",
                workdir: "/repo",
              },
              gitDiffRequestKey: "git-preview:/repo:unstaged::attempted.md",
              gitSectionId: "unstaged",
              language: "markdown",
              originSessionId: null,
              summary: "Attempted",
            },
          ],
        },
      ],
      activePaneId: "pane-a",
    };

    expect(
      collectRestoredGitDiffDocumentContentRefreshes(
        workspace,
        new Set(),
        new Set(["git-preview:/repo:unstaged::attempted.md"]),
      ),
    ).toEqual([
      {
        request: durableRequest,
        requestKey: "git-preview:/repo:unstaged::docs/README.md",
        sectionId: "unstaged",
      },
    ]);
  });

  type DiffPreviewWorkspaceTab = Extract<
    WorkspaceTab,
    { kind: "diffPreview" }
  >;

  const restoredGitDiffRequest = {
    path: "docs/README.md",
    sectionId: "unstaged",
    workdir: "/repo",
  } satisfies api.GitDiffRequestPayload;
  const restoredGitDiffRequestKey =
    "git-preview:/repo:unstaged::docs/README.md";

  function makeRestoredGitDiffWorkspace(
    tabOverrides: Partial<DiffPreviewWorkspaceTab> = {},
  ): WorkspaceState {
    const restoredTab: DiffPreviewWorkspaceTab = {
      id: restoredGitDiffRequestKey,
      kind: "diffPreview",
      changeType: "edit",
      diff: "-# Before restored\n+# After restored\n",
      diffMessageId: restoredGitDiffRequestKey,
      filePath: "/repo/docs/README.md",
      gitDiffRequest: restoredGitDiffRequest,
      gitDiffRequestKey: restoredGitDiffRequestKey,
      gitSectionId: "unstaged",
      isLoading: true,
      language: "markdown",
      originSessionId: null,
      originProjectId: null,
      summary: "Updated README",
      ...tabOverrides,
    };

    return {
      root: {
        type: "pane",
        paneId: "pane-restored",
      },
      panes: [
        {
          id: "pane-restored",
          activeTabId: restoredGitDiffRequestKey,
          activeSessionId: null,
          viewMode: "diffPreview",
          lastSessionViewMode: "session",
          sourcePath: null,
          tabs: [restoredTab],
        },
      ],
      activePaneId: "pane-restored",
    };
  }

  function makeRestoredGitDiffResponse(
    overrides: Partial<api.GitDiffResponse> = {},
  ): api.GitDiffResponse {
    return {
      changeType: "edit",
      changeSetId: "restored-change-set",
      diff: "-# Before restored\n+# After restored\n",
      diffId: restoredGitDiffRequestKey,
      documentEnrichmentNote: "Loaded full Markdown document.",
      documentContent: {
        before: {
          content: "# Before restored\n\nBefore body.\n",
          source: "index",
        },
        after: {
          content: "# After restored\n\nAfter restored body.\n",
          source: "worktree",
        },
        canEdit: true,
        isCompleteDocument: true,
      },
      filePath: "/repo/docs/README.md",
      language: "markdown",
      summary: "Restored README",
      ...overrides,
    };
  }

  it("hydrates stripped restored Git diff tabs after workspace layout readiness", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-15 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: makeRestoredGitDiffWorkspace(),
          }),
        );
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockResolvedValue(makeRestoredGitDiffResponse());
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();

        await waitFor(() => {
          expect(fetchWorkspaceLayoutSpy).toHaveBeenCalledWith(
            "workspace-test",
          );
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });
        expect(fetchGitDiffSpy).toHaveBeenCalledWith(restoredGitDiffRequest);

        expect(
          await screen.findByText("After restored body."),
        ).toBeInTheDocument();
        await clickAndSettle(screen.getByRole("button", { name: "Raw patch" }));
        expect(
          await screen.findByText("Loaded full Markdown document."),
        ).toBeInTheDocument();

        await settleAsyncUi();
        expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("forwards diff preview save options through the App save adapter", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const diffWorkspace: WorkspaceState = {
        root: {
          type: "pane",
          paneId: "pane-diff",
        },
        panes: [
          {
            id: "pane-diff",
            activeTabId: "diff-options",
            activeSessionId: null,
            viewMode: "diffPreview",
            lastSessionViewMode: "session",
            sourcePath: null,
            tabs: [
              {
                id: "diff-options",
                kind: "diffPreview",
                changeType: "edit",
                diff: [
                  "@@ -1,3 +1,3 @@",
                  " # Title",
                  " ",
                  "-Original body.",
                  "+Saved body.",
                ].join("\n"),
                diffMessageId: "diff-options",
                filePath: "/repo/docs/README.md",
                gitSectionId: "unstaged",
                language: "markdown",
                originProjectId: "project-termal",
                originSessionId: null,
                summary: "Updated README",
                documentContent: {
                  before: {
                    content: "# Title\n\nOriginal body.\n",
                    source: "index",
                  },
                  after: {
                    content: "# Title\n\nSaved body.\n",
                    source: "worktree",
                  },
                  canEdit: true,
                  isCompleteDocument: true,
                },
              },
            ],
          },
        ],
        activePaneId: "pane-diff",
      };
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/repo",
          },
        ],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-16 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: diffWorkspace,
          }),
        );
      const fetchFileSpy = vi.spyOn(api, "fetchFile").mockResolvedValue({
        path: "/repo/docs/README.md",
        content: "# Title\n\nSaved body.\n",
        contentHash: "sha256:base",
        language: "markdown",
      });
      const saveFileSpy = vi
        .spyOn(api, "saveFile")
        .mockRejectedValueOnce(new Error("file changed on disk before save"))
        .mockResolvedValueOnce({
          path: "/repo/docs/README.md",
          content: "# Title\n\nSaved body refined.\n",
          contentHash: "sha256:saved",
          language: "markdown",
        });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();
        await waitFor(() => {
          expect(fetchFileSpy).toHaveBeenCalledWith("/repo/docs/README.md", {
            projectId: "project-termal",
            sessionId: null,
          });
        });
        const editor = await screen.findByTestId("monaco-diff-editor-modified");
        await act(async () => {
          fireEvent.change(editor, {
            target: { value: "# Title\n\nSaved body refined.\n" },
          });
        });

        await clickAndSettle(screen.getByRole("button", { name: "Mock diff save" }));

        await waitFor(() => {
          expect(saveFileSpy).toHaveBeenNthCalledWith(
            1,
            "/repo/docs/README.md",
            "# Title\n\nSaved body refined.\n",
            {
              baseHash: "sha256:base",
              overwrite: undefined,
              projectId: "project-termal",
              sessionId: null,
            },
          );
        });

        expect(await screen.findByText("Save failed")).toBeInTheDocument();
        await clickAndSettle(screen.getByRole("button", { name: "Save anyway" }));

        await waitFor(() => {
          expect(saveFileSpy).toHaveBeenNthCalledWith(
            2,
            "/repo/docs/README.md",
            "# Title\n\nSaved body refined.\n",
            {
              baseHash: "sha256:base",
              overwrite: true,
              projectId: "project-termal",
              sessionId: null,
            },
          );
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchFileSpy.mockRestore();
        saveFileSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows a restore error on stripped Git diff tabs when document refresh fails", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-15 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: makeRestoredGitDiffWorkspace(),
          }),
        );
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockRejectedValue(new Error("restore failed"));
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();

        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });
        const alert = await screen.findByRole("alert");
        expect(within(alert).getByText("Unable to load diff")).toBeInTheDocument();
        expect(within(alert).getByText("restore failed")).toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("ignores late restored Git diff document responses after App unmount", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const restoreUpdateSpy = vi.fn();
      const restoreDeferred = createDeferred<api.GitDiffResponse>();
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-15 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: makeRestoredGitDiffWorkspace(),
          }),
        );
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockImplementation(() => restoreDeferred.promise);
      setAppTestHooksForTests({
        onRestoredGitDiffDocumentContentUpdate: restoreUpdateSpy,
      });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();
        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          cleanup();
          await flushUiWork();
        });
        await act(async () => {
          restoreDeferred.resolve(makeRestoredGitDiffResponse());
          await flushUiWork();
        });

        expect(restoreUpdateSpy).not.toHaveBeenCalled();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        setAppTestHooksForTests(null);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("ignores stale manual Git diff responses after reopening the same request key", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const staleDiffDeferred = createDeferred<api.GitDiffResponse>();
      const currentDiffDeferred = createDeferred<api.GitDiffResponse>();
      const gitWorkspace: WorkspaceState = {
        root: {
          type: "pane",
          paneId: "pane-git",
        },
        panes: [
          {
            id: "pane-git",
            activeSessionId: null,
            activeTabId: "git-status",
            lastSessionViewMode: "session",
            sourcePath: null,
            tabs: [
              {
                id: "git-status",
                kind: "gitStatus",
                originProjectId: null,
                originSessionId: null,
                workdir: "/repo",
              },
            ],
            viewMode: "gitStatus",
          },
        ],
        activePaneId: "pane-git",
      };
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-16 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: gitWorkspace,
          }),
        );
      const fetchGitStatusSpy = vi.spyOn(api, "fetchGitStatus").mockResolvedValue({
        ahead: 0,
        behind: 0,
        branch: "main",
        files: [
          {
            path: "src/example.ts",
            worktreeStatus: "M",
          },
        ],
        isClean: false,
        repoRoot: "/repo",
        upstream: "origin/main",
        workdir: "/repo",
      });
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockImplementationOnce(() => staleDiffDeferred.promise)
        .mockImplementationOnce(() => currentDiffDeferred.promise);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();

        await clickAndSettle(await screen.findByRole("button", { name: /^example\.ts$/i }));
        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });

        await clickAndSettle(screen.getByRole("tab", { name: /Git status: repo/i }));
        await clickAndSettle(await screen.findByRole("button", { name: /^example\.ts$/i }));
        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(2);
        });

        await act(async () => {
          currentDiffDeferred.resolve({
            changeType: "edit",
            diff: ["@@ -1 +1 @@", "-const value = 1;", "+const value = 2;"].join("\n"),
            diffId: "current-diff",
            filePath: "src/example.ts",
            language: "typescript",
            summary: "Current diff",
          });
          await flushUiWork();
        });
        expect(await screen.findByTestId("monaco-diff-editor")).toHaveTextContent(
          "const value = 2;",
        );

        await act(async () => {
          staleDiffDeferred.resolve({
            changeType: "edit",
            diff: ["@@ -1 +1 @@", "-const value = 1;", "+const value = 999;"].join("\n"),
            diffId: "stale-diff",
            filePath: "src/example.ts",
            language: "typescript",
            summary: "Stale diff",
          });
          await flushUiWork();
        });

        expect(screen.getByTestId("monaco-diff-editor")).toHaveTextContent(
          "const value = 2;",
        );
        expect(screen.getByTestId("monaco-diff-editor")).not.toHaveTextContent(
          "const value = 999;",
        );
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitStatusSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("keeps omitted adoptState slices unchanged", () => {
    const preservedCodex = {
      notices: [
        {
          kind: "runtimeNotice" as const,
          level: "warning" as const,
          title: "Existing notice",
          detail: "Keep this codex state when omitted.",
          timestamp: "2026-04-06T00:00:00Z",
        },
      ],
    };
    const preservedReadiness = [
      makeReadiness({
        agent: "Codex",
        detail: "Keep this readiness state when omitted.",
      }),
    ];
    const preservedProjects = [
      {
        id: "project-local",
        name: "Local",
        rootPath: "/repo",
      },
    ];
    const preservedOrchestrators = [
      makeOrchestrator({
        id: "orchestrator-existing",
      }),
    ];
    const preservedWorkspaces = [
      {
        id: "workspace-existing",
        revision: 3,
        updatedAt: "2026-04-06 00:00:00",
        controlPanelSide: "left" as const,
      },
    ];

    const adopted = resolveAdoptedStateSlices(
      {
        codex: preservedCodex,
        agentReadiness: preservedReadiness,
        projects: preservedProjects,
        orchestrators: preservedOrchestrators,
        workspaces: preservedWorkspaces,
      },
      {},
    );

    expect(adopted.codex).toBe(preservedCodex);
    expect(adopted.agentReadiness).toBe(preservedReadiness);
    expect(adopted.projects).toBe(preservedProjects);
    expect(adopted.orchestrators).toBe(preservedOrchestrators);
    expect(adopted.workspaces).toBe(preservedWorkspaces);
  });

  it("falls back to the template id for blank orchestrator group names", () => {
    const blankNamedOrchestrator = makeOrchestrator({
      templateId: "review-flow",
      templateSnapshot: {
        ...makeOrchestrator().templateSnapshot,
        id: "review-flow",
        name: "   ",
      },
    });

    expect(formatSessionOrchestratorGroupName(blankNamedOrchestrator)).toBe(
      "review-flow",
    );
  });

  it("builds control-surface entries with standalone sessions and the newest orchestrator mapping", () => {
    const templateSnapshot = makeOrchestrator().templateSnapshot;
    const standaloneSession = makeSession("session-standalone", {
      name: "Standalone",
    });
    const sharedSession = makeSession("session-shared", {
      name: "Shared",
    });
    const latestSession = makeSession("session-latest", {
      name: "Latest",
    });
    const olderOrchestrator = makeOrchestrator({
      id: "orchestrator-older",
      templateId: "older-flow",
      createdAt: "2026-03-30 09:00:00",
      templateSnapshot: {
        ...templateSnapshot,
        id: "older-flow",
        name: "Older Flow",
      },
      sessionInstances: [
        {
          templateSessionId: "builder",
          sessionId: "session-shared",
          lastCompletionRevision: null,
          lastDeliveredCompletionRevision: null,
        },
      ],
    });
    const newestOrchestrator = makeOrchestrator({
      id: "orchestrator-newest",
      templateId: "newest-flow",
      createdAt: "2026-03-30 10:00:00",
      templateSnapshot: {
        ...templateSnapshot,
        id: "newest-flow",
        name: "   ",
      },
      sessionInstances: [
        {
          templateSessionId: "builder",
          sessionId: "session-shared",
          lastCompletionRevision: null,
          lastDeliveredCompletionRevision: null,
        },
        {
          templateSessionId: "reviewer",
          sessionId: "session-latest",
          lastCompletionRevision: null,
          lastDeliveredCompletionRevision: null,
        },
      ],
    });

    const entries = buildControlSurfaceSessionListEntries(
      [standaloneSession, sharedSession, latestSession],
      [olderOrchestrator, newestOrchestrator],
    );

    expect(entries).toHaveLength(2);
    expect(entries[0]).toEqual({
      kind: "session",
      session: standaloneSession,
    });

    const groupedEntry = entries[1];
    if (!groupedEntry || groupedEntry.kind !== "orchestratorGroup") {
      throw new Error("Expected an orchestrator group entry");
    }
    expect(groupedEntry.orchestrator.id).toBe("orchestrator-newest");
    expect(groupedEntry.sessions.map((session) => session.id)).toEqual([
      "session-shared",
      "session-latest",
    ]);
    expect(formatSessionOrchestratorGroupName(groupedEntry.orchestrator)).toBe(
      "newest-flow",
    );
  });

  it("returns no control-surface entries for an empty session list", () => {
    expect(buildControlSurfaceSessionListEntries([], [makeOrchestrator()])).toEqual(
      [],
    );
  });

  it("adopts explicitly empty adoptState slices", () => {
    const currentCodex = {
      notices: [
        {
          kind: "runtimeNotice" as const,
          level: "warning" as const,
          title: "Existing notice",
          detail: "This should be replaced by the next state.",
          timestamp: "2026-04-06T00:00:00Z",
        },
      ],
    };
    const currentReadiness = [
      makeReadiness({
        agent: "Codex",
        detail: "This should be replaced by the next state.",
      }),
    ];
    const currentProjects = [
      {
        id: "project-local",
        name: "Local",
        rootPath: "/repo",
      },
    ];
    const currentOrchestrators = [
      makeOrchestrator({
        id: "orchestrator-existing",
      }),
    ];
    const currentWorkspaces = [
      {
        id: "workspace-existing",
        revision: 3,
        updatedAt: "2026-04-06 00:00:00",
        controlPanelSide: "left" as const,
      },
    ];
    const emptyCodex = { notices: [] };
    const emptyReadiness: typeof currentReadiness = [];
    const emptyProjects: typeof currentProjects = [];
    const emptyOrchestrators: typeof currentOrchestrators = [];
    const emptyWorkspaces: typeof currentWorkspaces = [];

    const adopted = resolveAdoptedStateSlices(
      {
        codex: currentCodex,
        agentReadiness: currentReadiness,
        projects: currentProjects,
        orchestrators: currentOrchestrators,
        workspaces: currentWorkspaces,
      },
      {
        codex: emptyCodex,
        agentReadiness: emptyReadiness,
        projects: emptyProjects,
        orchestrators: emptyOrchestrators,
        workspaces: emptyWorkspaces,
      },
    );

    expect(adopted.codex).toBe(emptyCodex);
    expect(adopted.agentReadiness).toBe(emptyReadiness);
    expect(adopted.projects).toBe(emptyProjects);
    expect(adopted.orchestrators).toBe(emptyOrchestrators);
    expect(adopted.workspaces).toBe(emptyWorkspaces);
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

  it("shows the Gemini interactive-shell warning when Gemini is selected", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse({
            revision: 1,
            projects: [],
            agentReadiness: [
              makeReadiness({
                status: "ready",
                blocking: false,
                detail:
                  "Gemini CLI is ready with Google login credentials from /home/testuser/.gemini/oauth_creds.json.",
                warningDetail:
                  "TermAl forces Gemini `tools.shell.enableInteractiveShell` to `false` for Windows ACP sessions to avoid PTY startup crashes. The setting in /home/testuser/.gemini/settings.json is left unchanged.",
              }),
            ],
            sessions: [],
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();
        await openCreateSessionDialog();

        expect(document.body.textContent ?? "").not.toContain(
          "TermAl forces Gemini",
        );

        await selectComboboxOption("Assistant", "Gemini");

        await waitFor(() => {
          expect(document.body.textContent ?? "").toContain(
            "TermAl forces Gemini",
          );
          expect(document.body.textContent ?? "").toContain(
            "Gemini CLI is ready with Google login credentials from /home/testuser/.gemini/oauth_creds.json.",
          );
        });

        await selectComboboxOption("Assistant", "Codex");

        await waitFor(() => {
          expect(document.body.textContent ?? "").not.toContain(
            "TermAl forces Gemini",
          );
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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
    const scrollIntoViewSpy = stubScrollIntoView();

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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();

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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
            resyncStateDeferred.resolve(makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered from /api/state",
                }),
              ],
            }));
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
            resyncStateDeferred.resolve(makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered from /api/state",
                }),
              ],
            }));
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
            return makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered after retry",
                }),
              ],
            });
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
            return makeStateResponse({
              revision: 2,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered after live fallback retry",
                }),
              ],
            });
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
            return makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Recovered Session",
                  preview: "Recovered after initial fallback retry",
                }),
              ],
            });
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
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 99,
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }));
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
        scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      // When the connection recovers, the ControlPanelConnectionIndicator
      // returns null (no badge). Verify no issue badge is present.
      expect(
        screen.queryByLabelText("Control panel backend reconnecting"),
      ).toBeNull();
      expect(
        screen.queryByLabelText("Control panel backend connecting"),
      ).toBeNull();
      expect(screen.getAllByText("Partial output.")).toHaveLength(2);
      expect(
        screen.getByText("Waiting for the next chunk of output..."),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
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
      scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();
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
        scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();

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
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("opens the workspace switcher with one refresh under StrictMode", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
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
          ],
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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
        await act(async () => {
          render(
            <StrictMode>
              <App />
            </StrictMode>,
          );
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await waitFor(() => {
          expect(fetchWorkspaceLayoutsSpy).toHaveBeenCalledTimes(1);
        });
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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

  it("deletes a saved workspace from the workspace switcher", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
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
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteStoredWorkspaceLayoutSpy = vi.spyOn(
        workspaceStorage,
        "deleteStoredWorkspaceLayout",
      );
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        const deleteButton = within(switcherDialog).getByRole("button", {
          name: "Delete workspace monitor-left",
        });

        await clickAndSettle(deleteButton);

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-right",
          }),
        ).toBeInTheDocument();
        expect(deleteStoredWorkspaceLayoutSpy).toHaveBeenCalledWith(
          "monitor-left",
        );
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        deleteStoredWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
        window.localStorage.removeItem(
          `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        );
      }
    });
  });


  it("shows workspace delete errors and restores the delete button", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
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
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockRejectedValue(new Error("Delete failed."));
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        expect(await within(switcherDialog).findByText("Delete failed.")).toBeInTheDocument();
        expect(
          within(switcherDialog).getAllByText("monitor-left").length,
        ).toBeGreaterThan(0);
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toBeEnabled();
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toHaveTextContent("Delete");
        expect(
          window.localStorage.getItem(
            `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
          ),
        ).not.toBeNull();
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
        window.localStorage.removeItem(
          `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        );
      }
    });
  });

  it("disables the workspace delete button while the request is in flight", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const deleteWorkspaceDeferred = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
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
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockImplementation((workspaceId: string) => {
          if (workspaceId === "monitor-left") {
            return deleteWorkspaceDeferred.promise;
          }
          throw new Error(`Unexpected workspace delete: ${workspaceId}`);
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toBeDisabled();
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toHaveTextContent("Deleting");

        deleteWorkspaceDeferred.resolve({
          workspaces: [
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
        window.localStorage.removeItem(
          `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        );
      }
    });
  });

  it("ignores stale workspace refresh results after a delete", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const staleRefresh = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
      const fetchWorkspaceLayoutsSpy = vi.mocked(api.fetchWorkspaceLayouts);
      fetchWorkspaceLayoutsSpy.mockResolvedValueOnce({
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
      fetchWorkspaceLayoutsSpy.mockReturnValueOnce(staleRefresh.promise);
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

      try {
        await renderApp();

        const switcherTrigger = await screen.findByRole("button", {
          name: /workspace /i,
        });
        await clickAndSettle(switcherTrigger);

        let switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        expect(within(switcherDialog).getAllByText("monitor-left").length).toBeGreaterThan(0);

        await clickAndSettle(switcherTrigger);
        await waitFor(() => {
          expect(
            screen.queryByRole("dialog", { name: "Workspace switcher" }),
          ).not.toBeInTheDocument();
        });

        await clickAndSettle(switcherTrigger);
        switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });

        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });

        staleRefresh.resolve({
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
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
        expect(fetchWorkspaceLayoutsSpy).toHaveBeenCalledTimes(2);
        expect(
          within(switcherDialog).getAllByText("monitor-right").length,
        ).toBeGreaterThan(0);
      } finally {
        deleteWorkspaceLayoutSpy.mockRestore();
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("applies overlapping workspace deletes in completion order", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const deleteMonitorLeft = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
      const deleteMonitorRight = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
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
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockImplementation((workspaceId: string) => {
          if (workspaceId === "monitor-left") {
            return deleteMonitorLeft.promise;
          }
          if (workspaceId === "monitor-right") {
            return deleteMonitorRight.promise;
          }
          throw new Error(`Unexpected workspace delete: ${workspaceId}`);
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
          },
        }),
      );
      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-right`,
        JSON.stringify({
          controlPanelSide: "right",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );
        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenNthCalledWith(
            1,
            "monitor-left",
          );
        });

        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-right",
          }),
        );
        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenNthCalledWith(
            2,
            "monitor-right",
          );
        });

        deleteMonitorRight.resolve({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
          ],
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryByText("monitor-right"),
          ).not.toBeInTheDocument();
        });
        expect(
          within(switcherDialog).getAllByText("monitor-left").length,
        ).toBeGreaterThan(0);

        deleteMonitorLeft.resolve({ workspaces: [] });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
        expect(
          window.localStorage.getItem(
            `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
          ),
        ).toBeNull();
        expect(
          window.localStorage.getItem(
            `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-right`,
          ),
        ).toBeNull();
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("does not offer delete for the active workspace in the workspace switcher", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
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
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockResolvedValue({ workspaces: [] });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
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

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=monitor-left",
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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });

        expect(
          within(switcherDialog).queryByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).not.toBeInTheDocument();
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-right",
          }),
        ).toBeInTheDocument();
        expect(deleteWorkspaceLayoutSpy).not.toHaveBeenCalled();
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        window.history.replaceState(window.history.state, "", originalUrl);
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
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }));
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

  it("does not resave workspace layout when SSE state preserves the same sessions", async () => {
    await withSuppressedActWarnings(async () => {
      vi.useFakeTimers();
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-current",
            revision: 1,
            updatedAt: "2026-04-10 10:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-current",
            updatedAt: "2026-04-10 10:00:01",
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
        await advanceTimers(200);
        saveWorkspaceLayoutSpy.mockClear();

        await dispatchStateEvent(
          latestEventSource(),
          makeStateResponse({
            revision: 2,
            projects: [],
            orchestrators: [],
            workspaces: [
              {
                id: "workspace-current",
                revision: 2,
                updatedAt: "2026-04-10 10:00:02",
                controlPanelSide: "left",
              },
            ],
            sessions: [],
          }),
        );

        await advanceTimers(200);
        expect(saveWorkspaceLayoutSpy).not.toHaveBeenCalled();
      } finally {
        window.localStorage.clear();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
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
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
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
      const scrollIntoViewSpy = stubScrollIntoView();

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
            revision: 2,
            serverInstanceId: "test-instance",
            session: {
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
          });
          await flushUiWork();
        });

        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await waitFor(() => {
          expect(
            screen.queryByRole("dialog", { name: "New session" }),
          ).not.toBeInTheDocument();
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve(makeStateResponse({
            revision: 3,
            projects: [],
            orchestrators: [],
            workspaces: [],
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
          }));
          await flushUiWork();
        });
        await screen.findAllByText("Codex 1");
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        createSessionSpy.mockRestore();
        refreshSessionModelOptionsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("uses a one-shot state probe after a backend-unavailable create-session error", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => {
          // Bootstrap state arrives via SSE, not fetchState, so the only
          // call here is the action-recovery one-shot probe.
          return actionRecoveryDeferred.promise;
        });
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockRejectedValue(
          new api.ApiRequestError(
            "backend-unavailable",
            "The TermAl backend is unavailable.",
            { status: 502 },
          ),
        );

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

        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchOpen();
        });
        await settleAsyncUi();

        expect(
          screen.queryByLabelText("Control panel backend connecting"),
        ).toBeNull();
        expect(
          screen.queryByLabelText("Control panel backend reconnecting"),
        ).toBeNull();
        expect(screen.queryByLabelText("Control panel issue")).toBeNull();

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledTimes(1);
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });
        expect(
          await screen.findByText("The TermAl backend is unavailable."),
        ).toBeInTheDocument();
        expect(
          screen.queryByLabelText("Control panel backend reconnecting"),
        ).toBeNull();

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 2,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await waitFor(() => {
          expect(
            screen.queryByText("The TermAl backend is unavailable."),
          ).toBeNull();
        });
        expect(screen.queryByLabelText("Control panel issue")).toBeNull();
        expect(
          screen.queryByLabelText("Control panel backend connecting"),
        ).toBeNull();
        expect(
          screen.queryByLabelText("Control panel backend reconnecting"),
        ).toBeNull();
        expect(
          screen.queryByLabelText("Control panel backend offline"),
        ).toBeNull();

        vi.useFakeTimers();
        await advanceTimers(5000);
        expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        expect(
          screen.queryByLabelText("Control panel backend reconnecting"),
        ).toBeNull();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        createSessionSpy.mockRestore();
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
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
      const refreshSessionModelOptionsDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      let fetchStateCallCount = 0;
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(async () => {
          fetchStateCallCount += 1;
          if (fetchStateCallCount === 1) {
            return makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            });
          }
          if (fetchStateCallCount === 2) {
            return makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
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
            });
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
      const scrollIntoViewSpy = stubScrollIntoView();

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
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-1", {
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
          });
          await flushUiWork();
        });

        expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith("session-1");
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve(makeStateResponse({
            revision: 3,
            projects: [],
            orchestrators: [],
            workspaces: [],
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
          }));
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
        scrollIntoViewSpy.mockRestore();
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
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }));
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
          state: makeStateResponse({
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
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-orchestrated", {
                name: "Orchestrated Builder",
                projectId: "project-local",
                preview: "Waiting for work",
                status: "active",
                workdir: "/repo",
              }),
            ],
          }),
        });
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
        scrollIntoViewSpy.mockRestore();
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
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
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
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Builder",
            projectId: "project-local",
            workdir: "/repo",
          }),
        ],
      }));
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
      const scrollIntoViewSpy = stubScrollIntoView();

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
        scrollIntoViewSpy.mockRestore();
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
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
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
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Builder",
            projectId: "project-local",
            workdir: "/repo",
          }),
        ],
      }));
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
      const scrollIntoViewSpy = stubScrollIntoView();
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
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("groups orchestrated sessions inside the control panel session list", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          const baseOrchestrator = makeOrchestrator();
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-questly",
                name: "Questly",
                rootPath: "/projects/questly",
              },
            ],
            orchestrators: [
              makeOrchestrator({
                id: "orchestrator-review-flow",
                projectId: "project-questly",
                templateId: "review-flow",
                templateSnapshot: {
                  ...baseOrchestrator.templateSnapshot,
                  id: "review-flow",
                  name: "Review Flow",
                  projectId: "project-questly",
                  sessions: [
                    {
                      id: "entry",
                      name: "Entry",
                      agent: "Codex",
                      model: null,
                      instructions: "Start the review flow.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 160, y: 220 },
                    },
                    {
                      id: "codex-reviewer",
                      name: "Codex Reviewer",
                      agent: "Codex",
                      model: null,
                      instructions: "Review the changes.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 420, y: 220 },
                    },
                    {
                      id: "claude-reviewer",
                      name: "Claude Reviewer",
                      agent: "Claude",
                      model: null,
                      instructions: "Double-check the review.",
                      autoApprove: false,
                      inputMode: "queue",
                      position: { x: 680, y: 220 },
                    },
                  ],
                },
                sessionInstances: [
                  {
                    templateSessionId: "entry",
                    sessionId: "session-entry",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                  {
                    templateSessionId: "codex-reviewer",
                    sessionId: "session-codex-reviewer",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                  {
                    templateSessionId: "claude-reviewer",
                    sessionId: "session-claude-reviewer",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                ],
                createdAt: "2026-04-03 10:05:00",
              }),
            ],
            sessions: [
              makeSession("session-entry", {
                name: "Entry",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: 'Running "C:\\WINDOWS\\system32\\wi..."',
              }),
              makeSession("session-standalone", {
                name: "Questly",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: "Current open tracked bugs in [bugs.md].",
              }),
              makeSession("session-codex-reviewer", {
                name: "Codex Reviewer",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: "Ready for a prompt.",
              }),
              makeSession("session-claude-reviewer", {
                name: "Claude Reviewer",
                agent: "Claude",
                model: "claude-sonnet-4-5",
                projectId: "project-questly",
                workdir: "/projects/questly",
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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

        await within(sessionList).findByText("Questly");
        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        expect(
          within(sessionList).getAllByRole("group", {
            name: /Orchestration Review Flow/i,
          }),
        ).toHaveLength(1);
        const orchestratorHeaderCopy = orchestratorGroup.querySelector(
          ".session-orchestrator-group-copy",
        );
        if (!(orchestratorHeaderCopy instanceof HTMLElement)) {
          throw new Error("Expected orchestrator header copy block");
        }
        expect(
          within(orchestratorHeaderCopy).getByText("Orchestration"),
        ).toBeInTheDocument();
        expect(within(orchestratorHeaderCopy).getByText("Review Flow")).toBeInTheDocument();
        expect(within(orchestratorHeaderCopy).getByText("3 sessions")).toBeInTheDocument();
        expect(within(orchestratorGroup).getByText("Entry")).toBeInTheDocument();
        expect(within(orchestratorGroup).getByText("Codex Reviewer")).toBeInTheDocument();
        expect(within(orchestratorGroup).getByText("Claude Reviewer")).toBeInTheDocument();
        expect(within(orchestratorGroup).queryByText("Questly")).not.toBeInTheDocument();

        const collapseButton = within(orchestratorGroup).getByRole("button", {
          name: /Collapse Review Flow sessions/i,
        });
        expect(
          collapseButton.querySelector(".session-orchestrator-group-chevron"),
        ).toHaveClass("expanded");

        await clickAndSettle(collapseButton);

        expect(
          within(orchestratorGroup).queryByText("Codex Reviewer"),
        ).not.toBeInTheDocument();
        const expandButton = within(orchestratorGroup).getByRole("button", {
          name: /Expand Review Flow sessions/i,
        });
        expect(
          expandButton.querySelector(".session-orchestrator-group-chevron"),
        ).not.toHaveClass("expanded");

        await clickAndSettle(expandButton);

        expect(within(orchestratorGroup).getByText("Codex Reviewer")).toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("controls orchestrators from the grouped session view", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const buildGroupedSessionState = (
        status: OrchestratorInstance["status"],
        revision: number,
      ) => {
        const baseOrchestrator = makeOrchestrator();
        return {
          revision,
          projects: [
            {
              id: "project-questly",
              name: "Questly",
              rootPath: "/projects/questly",
            },
          ],
          orchestrators: [
            makeOrchestrator({
              id: "orchestrator-review-flow",
              projectId: "project-questly",
              templateId: "review-flow",
              status,
              completedAt:
                status === "stopped" ? "2026-04-03 10:12:00" : null,
              templateSnapshot: {
                ...baseOrchestrator.templateSnapshot,
                id: "review-flow",
                name: "Review Flow",
                projectId: "project-questly",
                sessions: [
                  {
                    id: "entry",
                    name: "Entry",
                    agent: "Codex",
                    model: null,
                    instructions: "Start the review flow.",
                    autoApprove: true,
                    inputMode: "queue",
                    position: { x: 160, y: 220 },
                  },
                  {
                    id: "tester",
                    name: "Tester",
                    agent: "Codex",
                    model: null,
                    instructions: "Run the checks.",
                    autoApprove: true,
                    inputMode: "queue",
                    position: { x: 420, y: 220 },
                  },
                ],
              },
              sessionInstances: [
                {
                  templateSessionId: "entry",
                  sessionId: "session-entry",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
                {
                  templateSessionId: "tester",
                  sessionId: "session-tester",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
              ],
              createdAt: "2026-04-03 10:05:00",
            }),
          ],
          sessions: [
            makeSession("session-entry", {
              name: "Entry",
              projectId: "project-questly",
              workdir: "/projects/questly",
              preview: "[bugs.md](/C:/github/Personal/questly/docs/status...",
            }),
            makeSession("session-tester", {
              name: "Tester",
              projectId: "project-questly",
              workdir: "/projects/questly",
              preview: "`flutter test` passed on the current tree.",
            }),
          ],
        };
      };
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse(buildGroupedSessionState("running", 1));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/pause") {
          return jsonResponse(buildGroupedSessionState("paused", 2));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/resume") {
          return jsonResponse(buildGroupedSessionState("running", 3));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/stop") {
          return jsonResponse(buildGroupedSessionState("stopped", 4));
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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

        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        ).toBeInTheDocument();
        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Stop orchestration/,
          }),
        ).toBeInTheDocument();

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        );

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                String(input) ===
                "/api/orchestrators/orchestrator-review-flow/pause",
            ),
          ).toBe(true);
          expect(
            within(orchestratorGroup).getByRole("button", {
              name: /^Resume orchestration/,
            }),
          ).toBeInTheDocument();
        });

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Resume orchestration/,
          }),
        );

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                String(input) ===
                "/api/orchestrators/orchestrator-review-flow/resume",
            ),
          ).toBe(true);
          expect(
            within(orchestratorGroup).getByRole("button", {
              name: /^Pause orchestration/,
            }),
          ).toBeInTheDocument();
        });

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Stop orchestration/,
          }),
        );

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                String(input) ===
                "/api/orchestrators/orchestrator-review-flow/stop",
            ),
          ).toBe(true);
          expect(orchestratorGroup).toHaveAttribute("data-status", "stopped");
          expect(
            within(orchestratorGroup).queryByRole("button", {
              name: /^Resume orchestration/,
            }),
          ).not.toBeInTheDocument();
          expect(
            within(orchestratorGroup).queryByRole("button", {
              name: /^Stop orchestration/,
            }),
          ).not.toBeInTheDocument();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows orchestrator action errors from the grouped session view", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const baseOrchestrator = makeOrchestrator();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-questly",
                name: "Questly",
                rootPath: "/projects/questly",
              },
            ],
            orchestrators: [
              makeOrchestrator({
                id: "orchestrator-review-flow",
                projectId: "project-questly",
                templateId: "review-flow",
                status: "running",
                templateSnapshot: {
                  ...baseOrchestrator.templateSnapshot,
                  id: "review-flow",
                  name: "Review Flow",
                  projectId: "project-questly",
                  sessions: [
                    {
                      id: "entry",
                      name: "Entry",
                      agent: "Codex",
                      model: null,
                      instructions: "Start the review flow.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 160, y: 220 },
                    },
                  ],
                },
                sessionInstances: [
                  {
                    templateSessionId: "entry",
                    sessionId: "session-entry",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                ],
                createdAt: "2026-04-03 10:05:00",
              }),
            ],
            sessions: [
              makeSession("session-entry", {
                name: "Entry",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: "Ready for a prompt.",
              }),
            ],
          });
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/pause") {
          return new Response(JSON.stringify({ error: "pause failed" }), {
            headers: {
              "Content-Type": "application/json",
            },
            status: 500,
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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

        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        );

        await waitFor(() => {
          expect(screen.getByText("pause failed")).toBeInTheDocument();
        });
        expect(orchestratorGroup).toHaveAttribute("data-status", "running");
        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        ).toBeEnabled();
        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Stop orchestration/,
          }),
        ).toBeEnabled();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows pending orchestrator actions as busy and disabled until the request resolves", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const baseOrchestrator = makeOrchestrator();
      const pauseDeferred = createDeferred<Response>();
      const buildGroupedSessionState = (
        status: "running" | "paused" | "stopped",
        revision: number,
      ) => {
        const sessionInstances =
          status === "stopped"
            ? []
            : [
                {
                  templateSessionId: "entry",
                  sessionId: "session-entry",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
              ];
        const sessions =
          status === "stopped"
            ? []
            : [
                makeSession("session-entry", {
                  name: "Entry",
                  projectId: "project-questly",
                  workdir: "/projects/questly",
                  preview: "Ready for a prompt.",
                }),
              ];

        return {
          revision,
          projects: [
            {
              id: "project-questly",
              name: "Questly",
              rootPath: "/projects/questly",
            },
          ],
          orchestrators: [
            makeOrchestrator({
              id: "orchestrator-review-flow",
              projectId: "project-questly",
              templateId: "review-flow",
              status,
              templateSnapshot: {
                ...baseOrchestrator.templateSnapshot,
                id: "review-flow",
                name: "Review Flow",
                projectId: "project-questly",
                sessions: [
                  {
                    id: "entry",
                    name: "Entry",
                    agent: "Codex",
                    model: null,
                    instructions: "Start the review flow.",
                    autoApprove: true,
                    inputMode: "queue",
                    position: { x: 160, y: 220 },
                  },
                ],
              },
              sessionInstances,
              createdAt: "2026-04-03 10:05:00",
            }),
          ],
          sessions,
        };
      };
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse(buildGroupedSessionState("running", 1));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/pause") {
          return pauseDeferred.promise;
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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

        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        await act(async () => {
          fireEvent.click(
            within(orchestratorGroup).getByRole("button", {
              name: /^Pause orchestration/,
            }),
          );
        });
        await settleAsyncUi();

        expect(
          fetchMock.mock.calls.some(
            ([input]) =>
              String(input) === "/api/orchestrators/orchestrator-review-flow/pause",
          ),
        ).toBe(true);

        const pendingPauseButton = within(orchestratorGroup).getByRole("button", {
          name: /^Pause orchestration/,
        });
        const pendingStopButton = within(orchestratorGroup).getByRole("button", {
          name: /^Stop orchestration/,
        });

        expect(pendingPauseButton).toBeDisabled();
        expect(pendingPauseButton).toHaveAttribute("aria-busy", "true");
        expect(
          pendingPauseButton.querySelector(
            ".session-orchestrator-group-action-spinner",
          ),
        ).not.toBeNull();
        expect(pendingStopButton).toBeDisabled();
        expect(pendingStopButton).not.toHaveAttribute("aria-busy");

        await act(async () => {
          pauseDeferred.resolve(jsonResponse(buildGroupedSessionState("paused", 2)));
          await Promise.resolve();
        });
        await settleAsyncUi();

        await waitFor(() => {
          const resumeButton = within(orchestratorGroup).getByRole("button", {
            name: /^Resume orchestration/,
          });
          expect(orchestratorGroup).toHaveAttribute("data-status", "paused");
          expect(resumeButton).toBeEnabled();
          expect(resumeButton).not.toHaveAttribute("aria-busy");
          expect(
            within(orchestratorGroup).getByRole("button", {
              name: /^Stop orchestration/,
            }),
          ).toBeEnabled();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("removes projects from the context menu and resets project scopes", async () => {
    await withSuppressedActWarnings(async () => {
      const initialState = makeStateResponse({
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
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-api", {
            name: "API Session",
            projectId: "project-api",
            workdir: "/projects/api",
          }),
          makeSession("session-web", {
            name: "Web Session",
            projectId: "project-web",
            workdir: "/projects/web",
          }),
        ],
      });
      const deletedState = makeStateResponse({
        revision: 2,
        projects: [
          {
            id: "project-web",
            name: "Web",
            rootPath: "/projects/web",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-api", {
            name: "API Session",
            projectId: null,
            workdir: "/projects/api",
          }),
          makeSession("session-web", {
            name: "Web Session",
            projectId: "project-web",
            workdir: "/projects/web",
          }),
        ],
      });
      const workspaceWithApiOrigin: WorkspaceState = {
        root: { type: "pane", paneId: "pane-api-origin" },
        panes: [
          {
            id: "pane-api-origin",
            tabs: [
              {
                id: "terminal-api-origin",
                kind: "terminal",
                workdir: "/projects/api",
                originSessionId: null,
                originProjectId: "project-api",
              },
            ],
            activeTabId: "terminal-api-origin",
            activeSessionId: null,
            viewMode: "terminal",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-api-origin",
      };
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      vi.mocked(api.fetchWorkspaceLayout).mockResolvedValue(
        makeWorkspaceLayoutResponse({ workspace: workspaceWithApiOrigin }),
      );
      const saveWorkspaceLayoutSpy = vi.mocked(api.saveWorkspaceLayout);
      const deleteProjectSpy = vi
        .spyOn(api, "deleteProject")
        .mockResolvedValue(deletedState);
      const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      function projectRow(surface: HTMLElement, name: string) {
        const label = within(surface).getByText(name);
        const row = label.closest("button");
        if (!(row instanceof HTMLButtonElement)) {
          throw new Error(`Project row not found for ${name}`);
        }

        return row;
      }

      try {
        await renderApp();
        act(() => {
          latestEventSource().dispatchError();
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
        ).filter(
          (surface): surface is HTMLElement => surface instanceof HTMLElement,
        );
        const dockedProjectSurface = projectSurfaces[0];
        const standaloneProjectSurface =
          projectSurfaces[projectSurfaces.length - 1];
        if (!dockedProjectSurface || !standaloneProjectSurface) {
          throw new Error("Project surfaces not found");
        }

        await clickAndSettle(projectRow(dockedProjectSurface, "API"));
        await clickAndSettle(projectRow(standaloneProjectSurface, "API"));
        expect(projectRow(dockedProjectSurface, "API")).toHaveClass("selected");
        expect(projectRow(standaloneProjectSurface, "API")).toHaveClass(
          "selected",
        );

        await act(async () => {
          fireEvent.contextMenu(projectRow(dockedProjectSurface, "API"), {
            clientX: 160,
            clientY: 120,
          });
        });
        await settleAsyncUi();

        const menu = await screen.findByRole("menu", {
          name: "API project actions",
        });
        await clickAndSettle(
          within(menu).getByRole("menuitem", { name: "Remove project" }),
        );

        await waitFor(() => {
          expect(deleteProjectSpy).toHaveBeenCalledWith("project-api");
        });
        expect(confirmSpy).toHaveBeenCalledWith(
          'Remove "API" from TermAl? Existing sessions stay in All projects. Files on disk are not deleted.',
        );

        await waitFor(() => {
          const currentSurfaces = Array.from(
            document.querySelectorAll(".project-controls"),
          ).filter(
            (surface): surface is HTMLElement => surface instanceof HTMLElement,
          );
          expect(currentSurfaces.length).toBeGreaterThanOrEqual(2);
          for (const surface of currentSurfaces) {
            expect(within(surface).queryByText("API")).not.toBeInTheDocument();
            expect(projectRow(surface, "All projects")).toHaveClass("selected");
          }
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("All projects");
        expect(screen.getByText("API Session")).toBeInTheDocument();

        await waitFor(() => {
          const clearedWorkspaceSave = saveWorkspaceLayoutSpy.mock.calls.find(
            ([, payload]) => {
              const workspace = payload.workspace as WorkspaceState;
              return workspace.panes.some((pane) =>
                pane.tabs.some(
                  (tab) =>
                    tab.id === "terminal-api-origin" &&
                    "originProjectId" in tab &&
                    tab.originProjectId === null,
                ),
              );
            },
          );
          expect(clearedWorkspaceSave).toBeTruthy();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
      }
    });
  });

  it("swallows a late deleteProject resolution after the app unmounts without running post-unmount state updates", async () => {
    // Regression for the `if (!isMountedRef.current) return;` guard in the
    // try-branch of `handleProjectMenuRemoveProject` (round 7). The
    // existing `removes projects from the context menu...` test resolves
    // `deleteProject` synchronously while the component is still mounted,
    // so the guard never fires. This test uses a deferred to drive the
    // opposite timing: click Remove, unmount mid-request, then resolve.
    // Without the guard, the unmounted component would run
    // `adoptState(state)`, `resetRemovedProjectSelection(project.id)`, and
    // `setRequestError(null)`. In React 18 these setState calls are
    // silent no-ops. Pin the guard directly with a test-only hook placed
    // after the `isMountedRef` check but before the post-await state
    // update path.
    await withSuppressedActWarnings(async () => {
      let deleteResolve!: (state: AppTestStateResponse) => void;
      const deletePromise = new Promise<AppTestStateResponse>((resolve) => {
        deleteResolve = resolve;
      });
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-api",
            name: "API",
            rootPath: "/projects/api",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      });
      const deletedState = makeStateResponse({
        revision: 2,
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      const deleteProjectSpy = vi
        .spyOn(api, "deleteProject")
        .mockReturnValue(deletePromise);
      const postAwaitPathSpy = vi.fn();
      setAppTestHooksForTests({
        onDeleteProjectPostAwaitPath: postAwaitPathSpy,
      });
      const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();
      const consoleErrorMessages: string[] = [];
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
          if (typeof message === "string") {
            consoleErrorMessages.push(message);
          }
          originalConsoleError.call(console, message, ...args);
        });

      try {
        let renderResult!: ReturnType<typeof render>;
        await act(async () => {
          renderResult = render(<App />);
        });
        await settleAsyncUi();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        const apiRow = (
          await screen.findByText("API")
        ).closest("button") as HTMLButtonElement | null;
        if (!(apiRow instanceof HTMLButtonElement)) {
          throw new Error("API project row button not found");
        }

        await act(async () => {
          fireEvent.contextMenu(apiRow, { clientX: 160, clientY: 120 });
        });
        await settleAsyncUi();
        const menu = await screen.findByRole("menu", {
          name: "API project actions",
        });
        await clickAndSettle(
          within(menu).getByRole("menuitem", { name: "Remove project" }),
        );

        // At this point `deleteProject` was called but the deferred is
        // still pending. Unmount before resolving, then resolve — the
        // guard must prevent the post-unmount try-branch from running.
        expect(deleteProjectSpy).toHaveBeenCalledWith("project-api");
        expect(confirmSpy).toHaveBeenCalledTimes(1);

        await act(async () => {
          renderResult.unmount();
          await flushUiWork();
        });

        await act(async () => {
          deleteResolve(deletedState);
          await flushUiWork();
        });

        // No error messages should have been logged by the post-unmount
        // resolution. A missing guard would land on `adoptState(...)`
        // which calls `syncPreferencesFromState`, `adoptSessions`, and
        // several setState functions — none of which should fire after
        // unmount.
        expect(postAwaitPathSpy).not.toHaveBeenCalled();
        expect(consoleErrorMessages).toEqual([]);
      } finally {
        consoleErrorSpy.mockRestore();
        scrollIntoViewSpy.mockRestore();
      }
    });
  });

  it("swallows a late deleteProject rejection after the app unmounts without running post-unmount reportRequestError", async () => {
    // Regression for the `if (!isMountedRef.current) return;` guard in
    // the catch-branch of `handleProjectMenuRemoveProject`. Mirror of the
    // try-branch test above, but with a rejecting deferred: unmount
    // before rejection, then reject. Without the catch-branch guard,
    // `reportRequestError` would call `setRequestError` on the unmounted
    // component. React 18 makes that a silent no-op, so pin the guard
    // directly with a test-only hook placed after the mounted check and
    // before `reportRequestError`.
    await withSuppressedActWarnings(async () => {
      let deleteReject!: (error: unknown) => void;
      const deletePromise = new Promise<AppTestStateResponse>((_, reject) => {
        deleteReject = reject;
      });
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-api",
            name: "API",
            rootPath: "/projects/api",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      const deleteProjectSpy = vi
        .spyOn(api, "deleteProject")
        .mockReturnValue(deletePromise);
      const postAwaitPathSpy = vi.fn();
      setAppTestHooksForTests({
        onDeleteProjectPostAwaitPath: postAwaitPathSpy,
      });
      const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();
      const consoleErrorMessages: string[] = [];
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
          if (typeof message === "string") {
            consoleErrorMessages.push(message);
          }
          originalConsoleError.call(console, message, ...args);
        });

      try {
        let renderResult!: ReturnType<typeof render>;
        await act(async () => {
          renderResult = render(<App />);
        });
        await settleAsyncUi();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        const apiRow = (
          await screen.findByText("API")
        ).closest("button") as HTMLButtonElement | null;
        if (!(apiRow instanceof HTMLButtonElement)) {
          throw new Error("API project row button not found");
        }

        await act(async () => {
          fireEvent.contextMenu(apiRow, { clientX: 160, clientY: 120 });
        });
        await settleAsyncUi();
        const menu = await screen.findByRole("menu", {
          name: "API project actions",
        });
        await clickAndSettle(
          within(menu).getByRole("menuitem", { name: "Remove project" }),
        );

        expect(deleteProjectSpy).toHaveBeenCalledWith("project-api");
        expect(confirmSpy).toHaveBeenCalledTimes(1);

        await act(async () => {
          renderResult.unmount();
          await flushUiWork();
        });

        await act(async () => {
          deleteReject(new Error("backend rejected the delete"));
          await flushUiWork();
        });

        // Same invariant as the resolve test: no console errors from the
        // post-unmount rejection path. The guard prevents
        // `reportRequestError` from firing after the unmount.
        expect(postAwaitPathSpy).not.toHaveBeenCalled();
        expect(consoleErrorMessages).toEqual([]);
      } finally {
        consoleErrorSpy.mockRestore();
        scrollIntoViewSpy.mockRestore();
      }
    });
  });

  it("resolves settled-scroll minimum attempts from the fallback threshold and explicit clamp", () => {
    expect(resolveSettledScrollMinimumAttempts(60)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(13)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(12)).toBe(4);
    expect(resolveSettledScrollMinimumAttempts(4)).toBe(4);
    expect(resolveSettledScrollMinimumAttempts(12, 8)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(6, 8)).toBe(6);
    expect(resolveSettledScrollMinimumAttempts(60, 8)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(0)).toBe(0);
  });

  it("keeps the new-response button scroll correction alive for the explicit minAttempts floor", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();

      try {
        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          for (let iteration = 0; iteration < 10; iteration += 1) {
            await settleAsyncUi();
          }

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          if (!(messageStack instanceof HTMLElement)) {
            throw new Error("Message stack not found");
          }

          scrollToMock.mockClear();
          messageStack.scrollTop = 0;
          await act(async () => {
            fireEvent.scroll(messageStack);
            await flushUiWork();
          });

          await dispatchStateEvent(latestEventSource(), {
            revision: 2,
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
                preview: "Fresh assistant response.",
                messages: [
                  {
                    id: "message-assistant-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Fresh assistant response.",
                  },
                ],
              }),
            ],
          });

          const scrollToLatestButton = await screen.findByRole("button", {
            name: "New response",
          });
          scrollToMock.mockClear();
          await clickAndSettle(scrollToLatestButton);
          for (let iteration = 0; iteration < 10; iteration += 1) {
            await settleAsyncUi();
          }

          const callsAtBottom = filterScrollToCallsAt(
            scrollToMock,
            800,
            "auto",
          );
          expect(callsAtBottom.length).toBeGreaterThanOrEqual(8);
          expect(callsAtBottom.length).toBeLessThan(60);
        } finally {
          teardown();
        }
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("runs the default-scroll-to-bottom branch of the session scroll useLayoutEffect on mount and lets the cleanup return cleanly", async () => {
    // Regression for round 7's session-pane scroll restoration
    // `useLayoutEffect` restructure and the synchronous first `tick()`
    // inside `scheduleSettledScrollToBottom`. The two changes are
    // observed together here because the effect runs `tick()`
    // synchronously via `scheduleSettledScrollToBottom("auto", ...)` as
    // part of the default-scroll-to-bottom branch on first mount:
    //
    //  1. `messageStackRef.current` is the `<section class="message-stack">`
    //     rendered by `SessionPaneContent`.
    //  2. The effect hits the `else if (defaultScrollToBottom) { ... }`
    //     arm (branch 3) because there is no prior saved
    //     `paneScrollPositions[scrollStateKey]` entry.
    //  3. `scheduleSettledScrollToBottom("auto", { maxAttempts: 60 })`
    //     runs `tick()` synchronously inside its own call. `tick()`
    //     calls `scrollToLatestMessage("auto")`.
    //  4. `scrollToLatestMessage` computes
    //     `nextScrollTop = Math.max(scrollHeight - clientHeight, 0)`
    //     and invokes `node.scrollTo({ top, behavior })` when the
    //     current `scrollTop` is farther than 1 px from the target.
    //
    // jsdom reports `scrollHeight` and `clientHeight` as 0 on every
    // element, which would collapse `nextScrollTop` to 0 and skip the
    // `scrollTo` call via the 1-px tolerance. To observe the scroll the
    // test overrides the prototype getters for the duration of the test
    // so `scrollHeight - clientHeight = 800`, matches the sibling
    // `TerminalPanel.test.tsx` `stubScrollGeometry` helper's spirit
    // while staying scoped to a single test via a finally-block
    // restore. The test then checks `HTMLElement.prototype.scrollTo`
    // (which `beforeEach` already stubs with `vi.fn()`) to prove the
    // effect reached the `scrollTo({ top: 800, ... })` branch, which
    // simultaneously pins:
    //
    //  - Branch 3 of the restored `useLayoutEffect` if-else chain,
    //  - The synchronous first `tick()` in `scheduleSettledScrollToBottom`,
    //  - The `Math.max(scrollHeight - clientHeight, 0)` / 1-px tolerance
    //    pattern shared between `scrollToLatestMessage` and
    //    `scrollTerminalHistoryToBottom`,
    //  - And finally the cleanup branch: after `cleanup()` fires in
    //    `afterEach`, the returned cleanup function from
    //    `scheduleSettledScrollToBottom` runs `if (frameId !== 0)
    //    cancelAnimationFrame(frameId)`. A regression that dropped the
    //    `frameId !== 0` guard after the synchronous complete would
    //    still produce a noisy `cancelAnimationFrame(0)` call that
    //    would surface via `cancelAnimationFrameMock`'s tracking map
    //    (verified implicitly — the test's own afterEach would throw
    //    under the unhandled error if the cleanup propagated one).
    await withSuppressedActWarnings(async () => {
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
          return 1000;
        },
      });
      Object.defineProperty(HTMLElement.prototype, "clientHeight", {
        configurable: true,
        get() {
          return 200;
        },
      });

      // Wrap `cancelAnimationFrame` with a spy so the cleanup-guard
      // assertion below can prove the `frameId !== 0` guard fired.
      // `beforeEach` already installs `cancelAnimationFrameMock` via
      // `vi.stubGlobal`; spying on `globalThis.cancelAnimationFrame`
      // layers a `vi.fn` wrapper on top without dropping the underlying
      // map-tracking behavior.
      const cancelAnimationFrameSpy = vi.spyOn(
        globalThis,
        "cancelAnimationFrame",
      );

      try {
        const scrollToMock =
          HTMLElement.prototype.scrollTo as unknown as ReturnType<typeof vi.fn>;
        scrollToMock.mockClear?.();

        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          await settleAsyncUi();

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          expect(messageStack).not.toBeNull();

          // The synchronous first `tick()` in `scheduleSettledScrollToBottom`
          // must have scrolled to `top: 800` (Math.max(1000 - 200, 0) =
          // 800) on the message stack. We do not pin a single call
          // index because the scheduler's rAF follow-ups also run
          // (`requestAnimationFrameMock` queues them as microtasks),
          // so the mock may observe several scroll-to-bottom calls as
          // the stability loop settles — all of them should target
          // 800.
          const callsAtBottom = scrollToMock.mock.calls.filter((call) => {
            const arg = call[0];
            return (
              typeof arg === "object" &&
              arg !== null &&
              (arg as ScrollToOptions).top === 800 &&
              (arg as ScrollToOptions).behavior === "auto"
            );
          });
          expect(callsAtBottom.length).toBeGreaterThan(0);
        } finally {
          teardown();
        }

        // Explicit cleanup assertion: the scheduler's returned cleanup
        // closure checks `if (frameId !== 0) cancelAnimationFrame(frameId)`
        // to avoid a wasted `cancelAnimationFrame(0)` call after the
        // synchronous first `tick()` sets `frameId = 0` before scheduling
        // the next rAF. A regression that dropped the `frameId !== 0`
        // guard would be observable here because the cleanup would call
        // `cancelAnimationFrame(0)` at least once. Running this check
        // AFTER `teardown()` ensures the scheduler's cleanup has
        // definitely executed (SessionPaneContent unmounts on workspace
        // teardown) — the cleanup is otherwise only triggered by
        // effect-dep churn or `afterEach`'s global `cleanup()`.
        const zeroCancels = cancelAnimationFrameSpy.mock.calls.filter(
          ([frameId]) => frameId === 0,
        );
        expect(zeroCancels).toEqual([]);
      } finally {
        cancelAnimationFrameSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
        scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();
      const eventSource = latestEventSource();
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
      scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
          within(getSessionTablist()).getByText(/^termal$/i),
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
        scrollIntoViewSpy.mockRestore();
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
                    ((tab.getAttribute("aria-label") ?? tab.textContent ?? "").replace(/\u00d7/g, "").trim()),
                  ),
                ),
            ).toBe(true);
          });
        }

        await dragDockSectionToWorkspace("Sessions", /^Sessions$/i);
        await dragDockSectionToWorkspace("Orchestrators", /^Orchestrators$/i);
        await dragDockSectionToWorkspace("Files", /Files: termal/i);
        await dragDockSectionToWorkspace("Git status", /Git status: termal/i);
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
            expect(
              within(updatedWorkspaceTabList as HTMLElement)
                .getAllByRole("tab")
                .some((tab) =>
                  expectedTabLabel.test(
                    ((tab.getAttribute("aria-label") ?? tab.textContent ?? "").replace(/\u00d7/g, "").trim()),
                  ),
                ),
            ).toBe(true);
          });
        }

        await dragDockSectionToTabRail("Sessions", /^.*Sessions.*$/i);
        await dragDockSectionToTabRail("Orchestrators", /^.*Orchestrators.*$/i);
        await dragDockSectionToTabRail("Files", /Files:\s*termal/i);
        await dragDockSectionToTabRail("Git status", /Git status:\s*termal/i);
      } finally {
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
          within(sessionTablist).getByText(/^api$/i),
        ).toBeInTheDocument();

        await selectControlPanelProject(/^TermAl$/i);
        await clickAndSettle(
          within(getControlPanelShell()).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );
        expect(
          within(sessionTablist).getByText(/^termal$/i),
        ).toBeInTheDocument();
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");

        await clickAndSettle(
          within(sessionTablist).getByRole("tab", { name: /Git status: api/i }),
        );
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");
      } finally {
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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

        await clickAndSettle(screen.getByRole("tab", { name: /Git status: termal/i }));

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Git status: api/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        expect(
          within(getPaneByTabName(/Git status: api/i)).getByRole("combobox", {
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
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
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
            screen.getByRole("tab", { name: /Git status: termal/i }),
          ).toBeInTheDocument();
        });
        expect(screen.queryByRole("tab", { name: /Git status: api/i })).toBeNull();
        await waitFor(() => {
          const request = latestRequestTo("/api/git/status");
          expect(request.searchParams.get("path")).toBe("/projects/termal");
          expect(request.searchParams.get("sessionId")).toBe("session-1");
          expect(request.searchParams.get("projectId")).toBe("project-termal");
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();
      const eventSource = latestEventSource();
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
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("keeps a claimed local control panel layout side while merging server preferences", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-control-panel-resize-race`;
    const fetchWorkspaceLayoutDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchWorkspaceLayout>> | null>();
    const fetchWorkspaceLayoutSpy = vi
      .mocked(api.fetchWorkspaceLayout)
      .mockImplementation(() => fetchWorkspaceLayoutDeferred.promise);
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

    window.history.replaceState(
      window.history.state,
      "",
      "/?workspace=test-control-panel-resize-race",
    );
    window.localStorage.clear();
    window.localStorage.setItem(
      layoutStorageKey,
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
              paneId: "pane-session",
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
              id: "pane-session",
              tabs: [
                {
                  id: "tab-session",
                  kind: "session",
                  sessionId: "session-1",
                },
              ],
              activeTabId: "tab-session",
              activeSessionId: "session-1",
              viewMode: "session",
              lastSessionViewMode: "session",
              sourcePath: null,
            },
          ],
          activePaneId: "pane-session",
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
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();
      const divider = document.querySelector(".tile-divider-row");
      if (!(divider instanceof HTMLDivElement)) {
        throw new Error("Control panel divider not found");
      }
      const split = divider.parentElement;
      if (!(split instanceof HTMLDivElement)) {
        throw new Error("Control panel split container not found");
      }
      Object.defineProperty(split, "getBoundingClientRect", {
        configurable: true,
        value: () =>
          ({
            bottom: 800,
            height: 800,
            left: 0,
            right: 2000,
            top: 0,
            width: 2000,
            x: 0,
            y: 0,
            toJSON: () => ({}),
          }) satisfies DOMRect,
      });

      await act(async () => {
        fireEvent.pointerDown(divider, { clientX: 440, clientY: 40 });
        fireEvent.pointerMove(window, { clientX: 840, clientY: 40 });
        fireEvent.pointerUp(window);
        await flushUiWork();
      });

      await act(async () => {
        fetchWorkspaceLayoutDeferred.resolve({
          layout: {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-03-30 09:00:00",
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
                  paneId: "pane-session",
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
                  id: "pane-session",
                  tabs: [
                    {
                      id: "tab-session",
                      kind: "session",
                      sessionId: "session-1",
                    },
                  ],
                  activeTabId: "tab-session",
                  activeSessionId: "session-1",
                  viewMode: "session",
                  lastSessionViewMode: "session",
                  sourcePath: null,
                },
              ],
              activePaneId: "pane-session",
            },
          },
        });
        await flushUiWork();
      });

      await waitFor(() => {
        const persistedLayoutRaw = window.localStorage.getItem(layoutStorageKey);
        expect(persistedLayoutRaw).not.toBeNull();
        const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
          workspace: {
            root: {
              ratio: number;
            } | null;
          };
        };
        // The drag target is 840/2000 = 0.42, but the resize clamp nudges
        // the ratio upward to respect the control panel minimum width.
        expect(persistedLayout.workspace.root?.ratio).toBeCloseTo(0.44, 4);
      });
    } finally {
      window.history.replaceState(window.history.state, "", originalUrl);
      window.localStorage.clear();
      scrollIntoViewSpy.mockRestore();
      fetchWorkspaceLayoutSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("clears the tracked workspace-layout restart-required notice after recovery", () => {
    const restartMessage =
      "The running backend does not expose /api/workspaces/test-layout-restart-recovery (HTTP 200). Restart TermAl so the latest API routes are loaded.";

    expect(
      resolveRecoveredWorkspaceLayoutRequestError(
        restartMessage,
        restartMessage,
      ),
    ).toBeNull();
  });

  it("preserves unrelated request errors when a workspace layout recovers", () => {
    const restartMessage =
      "The running backend does not expose /api/workspaces/test-layout-restart-recovery (HTTP 200). Restart TermAl so the latest API routes are loaded.";
    const unrelatedError = "Could not refresh projects.";

    expect(
      resolveRecoveredWorkspaceLayoutRequestError(
        unrelatedError,
        restartMessage,
      ),
    ).toBe(unrelatedError);
    expect(
      resolveRecoveredWorkspaceLayoutRequestError(
        unrelatedError,
        null,
      ),
    ).toBe(unrelatedError);
  });

  it("clamps a saved docked control panel layout up to the current minimum width", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-control-panel-min-clamp`;
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

    window.history.replaceState(
      window.history.state,
      "",
      "/?workspace=test-control-panel-min-clamp",
    );
    window.localStorage.clear();
    window.localStorage.setItem(
      layoutStorageKey,
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
              paneId: "pane-session",
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
              id: "pane-session",
              tabs: [
                {
                  id: "tab-session",
                  kind: "session",
                  sessionId: "session-1",
                },
              ],
              activeTabId: "tab-session",
              activeSessionId: "session-1",
              viewMode: "session",
              lastSessionViewMode: "session",
              sourcePath: null,
            },
          ],
          activePaneId: "pane-session",
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
    Object.defineProperty(document.documentElement, "clientWidth", {
      configurable: true,
      value: 1000,
    });
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();

      await waitFor(() => {
        const persistedLayoutRaw = window.localStorage.getItem(layoutStorageKey);
        expect(persistedLayoutRaw).not.toBeNull();
        const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
          workspace: {
            root: {
              ratio: number;
            } | null;
          };
        };
        expect(persistedLayout.workspace.root?.ratio).toBeCloseTo(0.64, 5);
      });
    } finally {
      delete (document.documentElement as { clientWidth?: number }).clientWidth;
      window.history.replaceState(window.history.state, "", originalUrl);
      window.localStorage.clear();
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("stores manual message scroll state immediately when leaving the bottom", () => {
    const paneScrollPositions: Record<
      string,
      {
        top: number;
        shouldStick: boolean;
      }
    > = {
      "pane-1:session:session-1": {
        top: 1200,
        shouldStick: true,
      },
    };
    const node = {
      clientHeight: 800,
      scrollHeight: 2000,
      scrollTop: 960,
    };

    const next = syncMessageStackScrollPosition(
      node,
      "pane-1:session:session-1",
      paneScrollPositions,
    );

    expect(next).toEqual({
      top: 960,
      shouldStick: false,
    });
    expect(paneScrollPositions["pane-1:session:session-1"]).toEqual(next);
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

  it("uses the standalone control-surface pixel minimum instead of the generic row split clamp", () => {
    const previousStandalonePaneMinWidth =
      document.documentElement.style.getPropertyValue(
        "--standalone-control-surface-pane-min-width",
      );
    const previousDensityScale =
      document.documentElement.style.getPropertyValue("--density-scale");
    document.documentElement.style.setProperty(
      "--standalone-control-surface-pane-min-width",
      "calc(16rem * var(--density-scale))",
    );
    document.documentElement.style.setProperty("--density-scale", "1");

    try {
      const bounds = getWorkspaceSplitResizeBounds(
        {
          id: "split-1",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: {
            type: "pane",
            paneId: "session-pane",
          },
          second: {
            type: "pane",
            paneId: "git-pane",
          },
        },
        "split-1",
        "row",
        1600,
        new Map([
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
          [
            "git-pane",
            {
              id: "git-pane",
              tabs: [
                {
                  id: "git-tab",
                  kind: "gitStatus",
                  workdir: "C:/repo",
                  originSessionId: null,
                },
              ],
              activeTabId: "git-tab",
              activeSessionId: null,
              viewMode: "gitStatus",
              lastSessionViewMode: "session",
              sourcePath: null,
            },
          ],
        ]),
      );

      expect(bounds.minRatio).toBeCloseTo(22 / 100, 4);
      expect(bounds.maxRatio).toBeCloseTo(84 / 100, 4);
    } finally {
      if (previousStandalonePaneMinWidth) {
        document.documentElement.style.setProperty(
          "--standalone-control-surface-pane-min-width",
          previousStandalonePaneMinWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--standalone-control-surface-pane-min-width",
        );
      }
      if (previousDensityScale) {
        document.documentElement.style.setProperty(
          "--density-scale",
          previousDensityScale,
        );
      } else {
        document.documentElement.style.removeProperty("--density-scale");
      }
    }
  });

  it("matches the standalone control panel width when resolving the initial dock ratio", () => {
    const previousPaneWidth = document.documentElement.style.getPropertyValue(
      "--control-panel-pane-width",
    );
    document.documentElement.style.setProperty(
      "--control-panel-pane-width",
      "40rem",
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
        (40 * 16) / 1200,
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
      "40rem",
    );
    document.documentElement.style.setProperty(
      "--control-panel-pane-min-width",
      "40rem",
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
        1 / (1 + 0.22),
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
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        await act(async () => {
          fetchStateDeferred.resolve(makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }));
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
            revision: 2,
            serverInstanceId: "test-instance",
            session: {
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
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve(makeStateResponse({
            revision: 3,
            projects: [],
            orchestrators: [],
            workspaces: [],
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
          }));
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
        scrollIntoViewSpy.mockRestore();
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
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        await act(async () => {
          fetchStateDeferred.resolve(makeStateResponse({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeApprovalMode: "auto-approve",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }));
          await flushUiWork();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(
          screen.getByRole("tab", { name: "Codex" }),
        );
        await selectComboboxOption("Default reasoning effort", /high/i);
        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultCodexReasoningEffort: "high",
          });
        });
        await act(async () => {
          updateSettingsDeferred.resolve(makeStateResponse({
            revision: 2,
            preferences: {
              defaultCodexReasoningEffort: "high",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }));
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
            revision: 3,
            serverInstanceId: "test-instance",
            session: {
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
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-1",
          );
        });
        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve(makeStateResponse({
            revision: 4,
            preferences: {
              defaultCodexReasoningEffort: "high",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
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
          }));
          await flushUiWork();
        });
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
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
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        await act(async () => {
          fetchStateDeferred.resolve(makeStateResponse({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }));
          await flushUiWork();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(
          screen.getByRole("tab", { name: "Claude" }),
        );
        await selectComboboxOption("Default Claude effort", /max/i);
        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultClaudeEffort: "max",
          });
        });
        await act(async () => {
          updateSettingsDeferred.resolve(makeStateResponse({
            revision: 2,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeApprovalMode: "auto-approve",
              defaultClaudeEffort: "max",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }));
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
              claudeApprovalMode: "auto-approve",
              claudeEffort: "max",
            }),
          );
        });
        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-1",
            revision: 3,
            serverInstanceId: "test-instance",
            session: {
              id: "session-1",
              name: "Claude 1",
              emoji: "C",
              agent: "Claude",
              workdir: "/tmp",
              model: "claude-sonnet-4-20250514",
              claudeApprovalMode: "auto-approve",
              claudeEffort: "max",
              status: "idle",
              preview: "Ready for a prompt.",
              messages: [],
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
          refreshSessionModelOptionsDeferred.resolve(makeStateResponse({
            revision: 4,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeApprovalMode: "auto-approve",
              defaultClaudeEffort: "max",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              {
                id: "session-1",
                name: "Claude 1",
                emoji: "C",
                agent: "Claude",
                workdir: "/tmp",
                model: "claude-sonnet-4-20250514",
                claudeApprovalMode: "auto-approve",
                claudeEffort: "max",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          }));
          await flushUiWork();
        });
        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
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
      const scrollIntoViewSpy = stubScrollIntoView();

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
        scrollIntoViewSpy.mockRestore();
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
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
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
      const scrollIntoViewSpy = stubScrollIntoView();

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
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-2", {
              name: "Codex 2",
              workdir: "/remote/repo",
              projectId: "project-remote",
            }),
          });
          await flushUiWork();
        });

        await waitFor(() => {
          expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith(
            "session-2",
          );
        });

        await act(async () => {
          refreshSessionModelOptionsDeferred.resolve(makeStateResponse({
            revision: 3,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
              remotes,
            },
            projects,
            orchestrators: [],
            workspaces: [],
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
          }));
          await flushUiWork();
        });

        await settleAsyncUi();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve(makeStateResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        }));
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
        screen.getByRole("tab", { name: "Editor & UI" }),
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
    window.localStorage.clear();
    document.documentElement.style.removeProperty("--density-scale");

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve(makeStateResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        }));
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );
      await clickAndSettle(
        screen.getByRole("tab", { name: "Editor & UI" }),
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
      scrollIntoViewSpy.mockRestore();
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
    const scrollIntoViewSpy = stubScrollIntoView();
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-ui-style");

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve(makeStateResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        }));
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
      scrollIntoViewSpy.mockRestore();
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
