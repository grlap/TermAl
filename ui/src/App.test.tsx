import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import App, {
  MarkdownContent,
  ThemedCombobox,
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  getWorkspaceSplitResizeBounds,
  describeUnknownSessionModelWarning,
  resolveUnknownSessionModelSendAttempt,
} from "./App";
import type { AgentReadiness, Session } from "./types";

class EventSourceMock {
  static instances: EventSourceMock[] = [];

  onerror: ((event: Event) => void) | null = null;

  onopen: ((event: Event) => void) | null = null;

  private listeners = new Map<string, Set<(event: MessageEvent<string>) => void>>();

  constructor(_url?: string) {
    EventSourceMock.instances.push(this);
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    const listeners = this.listeners.get(type) ?? new Set<(event: MessageEvent<string>) => void>();
    listeners.add(normalizeMessageEventListener(listener));
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListenerOrEventListenerObject) {
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
function normalizeMessageEventListener(listener: EventListenerOrEventListenerObject) {
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

async function settleAsyncUi() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((resolve) => window.setTimeout(resolve, 0));
    await Promise.resolve();
  });
}

async function renderApp() {
  render(<App />);
  await settleAsyncUi();
}

async function clickAndSettle(target: HTMLElement) {
  fireEvent.click(target);
  await settleAsyncUi();
}

async function openCreateSessionDialog() {
  await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
  await clickAndSettle(await screen.findByRole("button", { name: "New" }));
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

      return typeof optionName === "string" ? label === optionName : optionName.test(label);
    });

  if (!option) {
    throw new Error(`Combobox option not found for ${String(optionName)}`);
  }

  await clickAndSettle(option);
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

    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

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
    HTMLElement.prototype.scrollTo = vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
  });

  afterEach(async () => {
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
  });

  it("applies the active combobox option on space without closing the menu", () => {
    const onChange = vi.fn();
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

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
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    }
  });

  it("scrolls an off-screen combobox selection into view when the menu opens", async () => {
    const originalGetBoundingClientRect = HTMLElement.prototype.getBoundingClientRect;
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
      HTMLElement.prototype.getBoundingClientRect = originalGetBoundingClientRect;
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
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      const eventSource = EventSourceMock.instances[0];
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
        expect(fetchMock.mock.calls.some(([url]) => String(url) === "/api/state")).toBe(true);
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
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(screen.getByText("Newer Session")).toBeInTheDocument();
      });
      expect(screen.queryByText("Older Session")).not.toBeInTheDocument();
      expect(screen.queryByText("Stale preview")).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("refreshes model options after creating a new Codex session", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue({
      revision: 1,
      projects: [],
      sessions: [],
    });
    const createSessionSpy = vi.spyOn(api, "createSession").mockResolvedValue({
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
    const refreshSessionModelOptionsSpy = vi.spyOn(api, "refreshSessionModelOptions").mockResolvedValue({
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
              supportedReasoningEfforts: ["low", "medium", "high", "xhigh"],
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

    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      await openCreateSessionDialog();
      await clickAndSettle(screen.getByRole("button", { name: "Create session" }));

      await waitFor(() => {
        expect(refreshSessionModelOptionsSpy).toHaveBeenCalledWith("session-1");
      });
      await screen.findAllByText("Codex 1");
      await settleAsyncUi();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      fetchStateSpy.mockRestore();
      createSessionSpy.mockRestore();
      refreshSessionModelOptionsSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("filters sessions from the control panel project selector", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
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
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();
      const eventSource = EventSourceMock.instances[0];
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchError();
      });
      await settleAsyncUi();
      await clickAndSettle(await screen.findByRole("button", { name: "Projects" }));
      await screen.findByText("API");
      await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));

      expect(screen.getByRole("combobox", { name: "Project" })).toHaveTextContent("All projects");

      await selectComboboxOption("Project", /^API$/i);

      await waitFor(() => {
        expect(screen.getByText("No sessions in API.")).toBeInTheDocument();
      });

      await clickAndSettle(await screen.findByRole("button", { name: "Files" }));
      expect(screen.getByRole("combobox", { name: "Project" })).toHaveTextContent("API");
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
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

      throw new Error(`Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();
      const eventSource = EventSourceMock.instances[0];
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchError();
      });
      await settleAsyncUi();

      await selectComboboxOption("Project", /^API$/i);
      await clickAndSettle(await screen.findByRole("button", { name: "Files" }));

      expect(await screen.findByRole("button", { name: /^README\.md/i })).toBeInTheDocument();
      expect(
        screen.queryByText("This file browser is no longer associated with a live session or project."),
      ).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
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
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();
      const eventSource = EventSourceMock.instances[0];
      expect(eventSource).toBeTruthy();
      act(() => {
        eventSource.dispatchError();
      });
      await settleAsyncUi();
      await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));

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
    document.documentElement.style.setProperty("--control-panel-pane-min-width", "14rem");

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

  it("shows a Codex notice when live model refresh resets reasoning effort after session creation", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/sessions") {
        return jsonResponse({
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
      }
      if (url === "/api/sessions/session-1/model-options/refresh") {
        return jsonResponse({
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
                  description: "Optimized for codex. Cheaper, faster, but less capable.",
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
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      await openCreateSessionDialog();
      await clickAndSettle(screen.getByRole("button", { name: "Create session" }));
      await clickAndSettle(await screen.findByRole("button", { name: "Prompt" }));

      await waitFor(() => {
        expect(
          screen.getByText(
            "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
          ),
        ).toBeInTheDocument();
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });


  it("applies the configured Codex reasoning effort to new Codex sessions", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let createSessionBody: Record<string, unknown> | null = null;
    let settingsBody: Record<string, unknown> | null = null;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/settings") {
        settingsBody = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        return jsonResponse({
          revision: 2,
          preferences: {
            defaultCodexReasoningEffort: "high",
            defaultClaudeEffort: "default",
          },
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/sessions") {
        createSessionBody = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        return jsonResponse({
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
      }
      if (url === "/api/sessions/session-1/model-options/refresh") {
        return jsonResponse({
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
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      await clickAndSettle(await screen.findByRole("button", { name: "Open preferences" }));
      await clickAndSettle(screen.getByRole("tab", { name: "Codex defaults" }));
      await selectComboboxOption("Default reasoning effort", /high/i);
      await waitFor(() => {
        expect(settingsBody).not.toBeNull();
      });
      expect(settingsBody).toMatchObject({
        defaultCodexReasoningEffort: "high",
      });
      await clickAndSettle(screen.getByRole("button", { name: "Close dialog" }));

      await openCreateSessionDialog();
      expect(screen.getByRole("combobox", { name: "Codex reasoning effort" })).toHaveTextContent("high");
      await clickAndSettle(screen.getByRole("button", { name: "Create session" }));

      await waitFor(() => {
        expect(createSessionBody).not.toBeNull();
      });
      expect(createSessionBody).toMatchObject({
        agent: "Codex",
        reasoningEffort: "high",
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("applies the configured Claude effort to new Claude sessions", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    let createSessionBody: Record<string, unknown> | null = null;
    let settingsBody: Record<string, unknown> | null = null;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/settings") {
        settingsBody = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        return jsonResponse({
          revision: 2,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "max",
          },
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/sessions") {
        createSessionBody = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        return jsonResponse({
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
      }
      if (url === "/api/sessions/session-1/model-options/refresh") {
        return jsonResponse({
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
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      await renderApp();

      await clickAndSettle(await screen.findByRole("button", { name: "Open preferences" }));
      await clickAndSettle(screen.getByRole("tab", { name: "Claude defaults" }));
      await selectComboboxOption("Default Claude effort", /max/i);
      await waitFor(() => {
        expect(settingsBody).not.toBeNull();
      });
      expect(settingsBody).toMatchObject({
        defaultClaudeEffort: "max",
      });
      await clickAndSettle(screen.getByRole("button", { name: "Close dialog" }));

      await openCreateSessionDialog();
      await selectComboboxOption("Assistant", /^Claude$/i);
      expect(screen.getByRole("combobox", { name: "Claude effort" })).toHaveTextContent("max");
      await clickAndSettle(screen.getByRole("button", { name: "Create session" }));

      await waitFor(() => {
        expect(createSessionBody).not.toBeNull();
      });
      expect(createSessionBody).toMatchObject({
        agent: "Claude",
        claudeEffort: "max",
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      restoreGlobal("fetch", originalFetch);
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

    const firstAttempt = resolveUnknownSessionModelSendAttempt(new Set(), session);
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



