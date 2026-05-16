// App.live-state.hydration.test.tsx
//
// Owns: hydration-focused App live-state tests where delayed session
// hydration and text/message deltas must reconcile without losing visible
// transcript content.
//
// Does not own:
//   - reconnect-focused tests, which live in
//     App.live-state.reconnect.test.tsx
//   - visibility / wake-gap recovery tests, which live in
//     App.live-state.visibility.test.tsx
//   - queued-follow-up and watchdog cooldown tests, which
//     live in App.live-state.watchdog.test.tsx
//   - state-resync and server-instance hydration recovery tests, which live in
//     App.live-state.hydration-recovery.test.tsx
//   - general delta-gap core tests, which live in
//     App.live-state.deltas.test.tsx
// Split out of: ui/src/App.live-state.deltas.test.tsx.
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
import { ACTIVE_PROMPT_POLL_INTERVAL_MS } from "./active-prompt-poll";
import App from "./App";
import { ThemedCombobox } from "./preferences-panels";
import {
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  describeUnknownSessionModelWarning,
  resolveControlPanelWorkspaceRoot,
  resolveUnknownSessionModelSendAttempt,
} from "./session-model-utils";
import { setAppTestHooksForTests } from "./app-test-hooks";
import { resolveStandaloneControlPanelDockWidthRatio } from "./control-panel-layout";
import {
  buildControlSurfaceSessionListEntries,
  formatSessionOrchestratorGroupName,
} from "./control-surface-state";
import { collectRestoredGitDiffDocumentContentRefreshes } from "./git-diff-refresh";
import {
  resolveSettledScrollMinimumAttempts,
  syncMessageStackScrollPosition,
} from "./scroll-position";
import {
  resolveAdoptedStateSlices,
  resolveRecoveredWorkspaceLayoutRequestError,
} from "./state-adoption";
import {
  getWorkspaceSplitResizeBounds,
  resolveControlSurfaceSectionIdForWorkspaceTab,
} from "./workspace-queries";
import {
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
} from "./live-updates";
import type { AgentReadiness, OrchestratorInstance, Session } from "./types";
import * as workspaceStorage from "./workspace-storage";
import { WORKSPACE_LAYOUT_STORAGE_KEY } from "./workspace-storage";
import type { WorkspaceState, WorkspaceTab } from "./workspace";
import type { AppTestStateResponse } from "./app-test-harness";
import {
  EventSourceMock,
  ResizeObserverMock,
  advanceTimers,
  clickAndSettle,
  createActWrappedAnimationFrameMocks,
  createDeferred,
  createDragDataTransfer,
  createReducedMimeDragDataTransfer,
  dispatchOpenedStateEvent,
  dispatchStateEvent,
  filterScrollToCallsAt,
  flushUiWork,
  jsonResponse,
  latestEventSource,
  makeOrchestrator,
  makeReadiness,
  makeSession,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  mockScrollToAndApplyTop,
  openCreateSessionDialog,
  renderApp,
  renderAppWithProjectAndSession,
  restoreGlobal,
  selectComboboxOption,
  setDocumentVisibilityState,
  settleAsyncUi,
  stubElementScrollGeometry,
  stubScrollIntoView,
  submitButtonAndSettle,
  withFallbackStateHarness,
  withSuppressedActWarnings,
} from "./app-test-harness";

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

describe("App live state - delta-gap core", () => {
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

  async function openSessionByName(name: string) {
    await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
    const sessionList = document.querySelector(".session-list");
    if (!(sessionList instanceof HTMLDivElement)) {
      throw new Error("Session list not found");
    }
    const sessionRowButton =
      within(sessionList).getByText(name).closest("button");
    if (!sessionRowButton) {
      throw new Error("Session row button not found");
    }
    await clickAndSettle(sessionRowButton);
  }

  function expectContainerTextContains(
    container: Element | null,
    expectedCells: string[],
  ) {
    expect(container).not.toBeNull();
    const containerText = container?.textContent ?? "";
    for (const cell of expectedCells) {
      expect(containerText).toContain(cell);
    }
  }

  function expectStreamingTableFragmentContains(expectedCells: string[]) {
    expect(document.querySelector(".markdown-table-scroll table")).toBeNull();
    expectContainerTextContains(
      document.querySelector(".markdown-streaming-fragment"),
      expectedCells,
    );
  }

  function expectSettledTableContains(expectedCells: string[]) {
    expect(document.querySelector(".markdown-streaming-fragment")).toBeNull();
    expectContainerTextContains(
      document.querySelector(".markdown-table-scroll table"),
      expectedCells,
    );
  }

  const expectedMarkdownTableCells = [
    "Backend",
    "107",
    "87,395",
    "3.19 MiB",
    "Frontend",
    "280",
    "173,265",
    "5.52 MiB",
    "Total",
    "452",
    "278,043",
    "9.55 MiB",
  ];

  it("rehydrates an active transcript when a summary snapshot overtakes a delayed delta", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Hello",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello",
          },
        ],
      });
      const hydratedSession = makeSession("session-1", {
        ...initialSession,
        preview: "Hello world",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 11,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello world",
          },
        ],
      });
      const sessionFetch = createDeferred<Response>();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [initialSession],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          return sessionFetch.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowButton =
          within(sessionList).getByText("Codex Session").closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);
        expect(screen.getAllByText("Hello").length).toBeGreaterThan(0);

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 3,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-1", {
                ...initialSession,
                preview: "Hello world",
                messagesLoaded: false,
                messageCount: 1,
                sessionMutationStamp: 11,
                messages: [],
              }),
            ],
          }),
        );
        await settleAsyncUi();
        expect(
          fetchMock.mock.calls.some(
            ([input]) =>
              new URL(String(input), "http://localhost").pathname ===
              "/api/sessions/session-1",
          ),
        ).toBe(true);

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 2,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: " world",
            preview: "Hello world",
            sessionMutationStamp: 11,
          });
          await flushUiWork();
        });

        await act(async () => {
          sessionFetch.resolve(
            jsonResponse({
              revision: 3,
              serverInstanceId: "test-instance",
              session: hydratedSession,
            }),
          );
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(screen.getAllByText("Hello world").length).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("does not let stale hydration clobber an in-flight text delta with matching metadata", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Hello",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 11,
        messages: [],
      });
      const loadedSession = makeSession("session-1", {
        ...initialSession,
        preview: "Hello",
        messagesLoaded: true,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello",
          },
        ],
      });
      const staleHydratedSession = makeSession("session-1", {
        ...loadedSession,
        preview: "Hello",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 11,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello",
          },
        ],
      });
      const sessionFetch = createDeferred<Response>();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [initialSession],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          return sessionFetch.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowButton =
          within(sessionList).getByText("Codex Session").closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);
        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                new URL(String(input), "http://localhost").pathname ===
                "/api/sessions/session-1",
            ),
          ).toBe(true);
        });

        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 2,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [loadedSession],
          }),
        );
        await settleAsyncUi();
        expect(screen.getAllByText("Hello").length).toBeGreaterThan(0);

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 3,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: " world",
            preview: "Hello world",
            sessionMutationStamp: 11,
          });
          await flushUiWork();
        });
        expect(screen.getAllByText("Hello world").length).toBeGreaterThan(0);

        await act(async () => {
          sessionFetch.resolve(
            jsonResponse({
              revision: 4,
              serverInstanceId: "test-instance",
              session: staleHydratedSession,
            }),
          );
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(screen.getAllByText("Hello world").length).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("applies an advancing session text delta across an unrelated global revision gap", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Source-F",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Source-F",
          },
        ],
      });
      const repairedSession = makeSession("session-1", {
        ...initialSession,
        preview: "Source-Focused",
        sessionMutationStamp: 12,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Source-Focused",
          },
        ],
      });
      const stateResync = createDeferred<Response>();
      const sessionHydration = createDeferred<Response>();
      let stateRequestCount = 0;
      let sessionHydrationRequestCount = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateRequestCount += 1;
          if (stateRequestCount === 1) {
            return jsonResponse(
              makeStateResponse({
                revision: 1,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [initialSession],
              }),
            );
          }
          return stateResync.promise;
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionHydrationRequestCount += 1;
          return sessionHydration.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowButton =
          within(sessionList).getByText("Codex Session").closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);
        expect(screen.getAllByText("Source-F").length).toBeGreaterThan(0);

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 2,
            codex: { notices: [] },
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 4,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: "ocused",
            preview: "Source-Focused",
            sessionMutationStamp: 12,
          });
          await flushUiWork();
        });

        expect(screen.getAllByText("Source-Focused").length).toBeGreaterThan(0);
        await waitFor(() => {
          expect(sessionHydrationRequestCount).toBe(1);
        });

        await act(async () => {
          stateResync.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 4,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [repairedSession],
              }),
            ),
          );
          sessionHydration.resolve(
            jsonResponse({
              revision: 4,
              serverInstanceId: "test-instance",
              session: repairedSession,
            }),
          );
          await flushUiWork();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("adopts lower-revision text-repair hydration after a newer unrelated live event", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Source-F",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Source-F",
          },
        ],
      });
      const repairedSession = makeSession("session-1", {
        ...initialSession,
        preview: "Source-Focused",
        sessionMutationStamp: 12,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Source-Focused",
          },
        ],
      });
      const stateResync = createDeferred<Response>();
      const sessionHydration = createDeferred<Response>();
      let stateRequestCount = 0;
      let sessionHydrationRequestCount = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateRequestCount += 1;
          if (stateRequestCount === 1) {
            return jsonResponse(
              makeStateResponse({
                revision: 1,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [initialSession],
              }),
            );
          }
          return stateResync.promise;
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionHydrationRequestCount += 1;
          return sessionHydration.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await openSessionByName("Codex Session");
        expect(screen.getAllByText("Source-F").length).toBeGreaterThan(0);

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 2,
            codex: { notices: [] },
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 4,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: "ocussed",
            preview: "Source-Focussed",
            sessionMutationStamp: 12,
          });
          await flushUiWork();
        });

        expect(screen.getAllByText("Source-Focussed").length).toBeGreaterThan(
          0,
        );
        expect(screen.queryByText("Source-Focused")).not.toBeInTheDocument();
        await waitFor(() => {
          expect(sessionHydrationRequestCount).toBe(1);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 5,
            codex: { notices: [] },
          });
          sessionHydration.resolve(
            jsonResponse({
              revision: 4,
              serverInstanceId: "test-instance",
              session: repairedSession,
            }),
          );
          await flushUiWork();
        });
        await waitFor(() => {
          expect(screen.getAllByText("Source-Focused").length).toBeGreaterThan(
            0,
          );
        });
        expect(screen.queryByText("Source-Focussed")).not.toBeInTheDocument();

        await act(async () => {
          stateResync.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 5,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [repairedSession],
              }),
            ),
          );
          await flushUiWork();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("keeps streamed markdown table chunks intact across repeated revision gaps", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const tableStart = [
        "Tracked Project Total",
        "",
        "| Group | Files | Lines | Size |",
      ].join("\n");
      const separatorAndBackend =
        "\n| --- | ---: | ---: | ---: |\n| Backend | 107 | 87,395 | 3.19 MiB |";
      const frontend =
        "\n| Frontend | 280 | 173,265 | 5.52 MiB |";
      const total =
        "\n| Total | 452 | 278,043 | 9.55 MiB |\n\n";
      const finalTable = `${tableStart}${separatorAndBackend}${frontend}${total}`;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Tracked Project Total",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 20,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: tableStart,
          },
        ],
      });
      const finalSession = makeSession("session-1", {
        ...initialSession,
        status: "idle",
        preview: "Tracked Project Total",
        sessionMutationStamp: 26,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: finalTable,
          },
        ],
      });
      const stateResync = createDeferred<Response>();
      const sessionHydration = createDeferred<Response>();
      let stateRequestCount = 0;
      let sessionHydrationRequestCount = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateRequestCount += 1;
          if (stateRequestCount === 1) {
            return jsonResponse(
              makeStateResponse({
                revision: 1,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [initialSession],
              }),
            );
          }
          return stateResync.promise;
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionHydrationRequestCount += 1;
          return sessionHydration.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await openSessionByName("Codex Session");
        expect(screen.getAllByText("Tracked Project Total").length).toBeGreaterThan(
          0,
        );

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 2,
            codex: { notices: [] },
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 4,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: separatorAndBackend,
            preview: "Tracked Project Total",
            sessionMutationStamp: 22,
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 5,
            codex: { notices: [] },
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 7,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: frontend,
            preview: "Tracked Project Total",
            sessionMutationStamp: 24,
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 8,
            codex: { notices: [] },
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 10,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            delta: total,
            preview: "Tracked Project Total",
            sessionMutationStamp: 26,
          });
          await flushUiWork();
        });

        await waitFor(() => {
          expectStreamingTableFragmentContains(expectedMarkdownTableCells);
        });
        await waitFor(() => {
          expect(sessionHydrationRequestCount).toBe(1);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageUpdated",
            revision: 11,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            message: {
              id: "message-assistant-1",
              type: "text",
              timestamp: "10:01",
              author: "assistant",
              text: finalTable,
            },
            preview: "Tracked Project Total",
            status: "idle",
            sessionMutationStamp: 27,
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expectSettledTableContains(expectedMarkdownTableCells);
        });

        await act(async () => {
          stateResync.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 10,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [finalSession],
              }),
            ),
          );
          sessionHydration.resolve(
            jsonResponse({
              revision: 10,
              serverInstanceId: "test-instance",
              session: finalSession,
            }),
          );
          await flushUiWork();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("replaces a corrupted streaming markdown table with final authoritative text across a revision gap", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const finalTable = [
        "Tracked Project Total",
        "",
        "| Group | Files | Lines | Size |",
        "| --- | ---: | ---: | ---: |",
        "| Backend | 107 | 87,395 | 3.19 MiB |",
        "| Frontend | 280 | 173,265 | 5.52 MiB |",
        "| Total | 452 | 278,043 | 9.55 MiB |",
        "",
      ].join("\n");
      const corruptedTable =
        "Tracked Project Total\n\n| Group Files | Lines | Size ||---|---:|---:|---:|| Backend107 | 87,395 | 3.19B |";
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Tracked Project Total",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 30,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: corruptedTable,
          },
        ],
      });
      const finalSession = makeSession("session-1", {
        ...initialSession,
        status: "idle",
        sessionMutationStamp: 32,
        messages: [
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: finalTable,
          },
        ],
      });
      const stateResync = createDeferred<Response>();
      let stateRequestCount = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateRequestCount += 1;
          if (stateRequestCount === 1) {
            return jsonResponse(
              makeStateResponse({
                revision: 1,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [initialSession],
              }),
            );
          }
          return stateResync.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await openSessionByName("Codex Session");
        expect(document.body.textContent).toContain("Backend107");

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "codexUpdated",
            revision: 2,
            codex: { notices: [] },
          });
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: 4,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            text: finalTable,
            preview: "Tracked Project Total",
            sessionMutationStamp: 32,
          });
          await flushUiWork();
        });

        await waitFor(() => {
          expectStreamingTableFragmentContains(expectedMarkdownTableCells);
        });
        expect(document.body.textContent).not.toContain("3.19B");

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageUpdated",
            revision: 5,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 0,
            messageCount: 1,
            message: {
              id: "message-assistant-1",
              type: "text",
              timestamp: "10:01",
              author: "assistant",
              text: finalTable,
            },
            preview: "Tracked Project Total",
            status: "idle",
            sessionMutationStamp: 33,
          });
          await flushUiWork();
        });
        await waitFor(() => {
          expectSettledTableContains(expectedMarkdownTableCells);
        });

        await act(async () => {
          stateResync.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 4,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [finalSession],
              }),
            ),
          );
          await flushUiWork();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("rejects stale session hydration after a newer metadata delta", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const staleHydration = createDeferred<Response>();
      let sessionFetchCount = 0;
      const summarySession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Old summary",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [],
      });
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [summarySession],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionFetchCount += 1;
          if (sessionFetchCount === 1) {
            return staleHydration.promise;
          }
          return jsonResponse({
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "Newer metadata-only message",
              messagesLoaded: true,
              messageCount: 2,
              sessionMutationStamp: 11,
              messages: [
                {
                  id: "message-existing",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Existing transcript",
                },
                {
                  id: "message-new",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Newer metadata-only message",
                },
              ],
            }),
          });
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [summarySession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowButton =
          within(sessionList).getByText("Codex Session").closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                new URL(String(input), "http://localhost").pathname ===
                "/api/sessions/session-1",
            ),
          ).toBe(true);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageCreated",
            revision: 2,
            sessionId: "session-1",
            messageId: "message-new",
            messageIndex: 1,
            messageCount: 2,
            message: {
              id: "message-new",
              type: "text",
              timestamp: "10:02",
              author: "assistant",
              text: "Newer metadata-only message",
            },
            preview: "Newer metadata-only message",
            status: "active",
            sessionMutationStamp: 11,
          });
          await flushUiWork();
        });

        await act(async () => {
          staleHydration.resolve(
            jsonResponse({
              revision: 1,
              serverInstanceId: "test-instance",
              session: makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Stale transcript",
                messagesLoaded: true,
                messageCount: 1,
                sessionMutationStamp: 10,
                messages: [
                  {
                    id: "message-old",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Stale transcript",
                  },
                ],
              }),
            }),
          );
          await flushUiWork();
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(sessionFetchCount).toBeGreaterThanOrEqual(2);
        });
        await waitFor(() => {
          expect(
            screen.getAllByText("Newer metadata-only message").length,
          ).toBeGreaterThan(0);
        });
        expect(screen.queryByText("Stale transcript")).not.toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("keeps a newly created prompt visible while active session hydration is pending", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const promptMessage = {
        id: "message-latest-prompt",
        type: "text" as const,
        timestamp: "10:02",
        author: "you" as const,
        text: "Latest prompt from user",
      };
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "idle",
        preview: "Previous answer",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      });
      const metadataOnlySummary = makeSession("session-1", {
        ...initialSession,
        status: "active",
        preview: "Latest prompt from user",
        messagesLoaded: false,
        messageCount: 2,
        sessionMutationStamp: 11,
        messages: [],
      });
      const hydratedSession = makeSession("session-1", {
        ...initialSession,
        status: "active",
        preview: "Stale hydration without prompt",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: initialSession.messages,
      });
      const sessionFetch = createDeferred<Response>();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [initialSession],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          return sessionFetch.promise;
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }

        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowButton =
          within(sessionList).getByText("Codex Session").closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);
        expect(screen.getAllByText("Previous answer").length).toBeGreaterThan(0);

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 2,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [metadataOnlySummary],
          }),
        );
        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                new URL(String(input), "http://localhost").pathname ===
                "/api/sessions/session-1",
            ),
          ).toBe(true);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageCreated",
            revision: 3,
            sessionId: "session-1",
            messageId: promptMessage.id,
            messageIndex: 1,
            messageCount: 2,
            message: promptMessage,
            preview: promptMessage.text,
            status: "active",
            sessionMutationStamp: 11,
          });
          await flushUiWork();
        });

        await waitFor(() => {
          const promptBubble = screen
            .getAllByText(promptMessage.text)
            .map((element) => element.closest(".message-card"))
            .find((element) => element?.classList.contains("bubble-you"));
          expect(promptBubble).toBeTruthy();
          expect(promptBubble?.classList.contains("pending-prompt-card")).toBe(
            false,
          );
        });

        await act(async () => {
          sessionFetch.resolve(
            jsonResponse({
              revision: 3,
              serverInstanceId: "test-instance",
              session: hydratedSession,
            }),
          );
          await flushUiWork();
        });
        await settleAsyncUi();

        await waitFor(() => {
          const promptBubble = screen
            .getAllByText(promptMessage.text)
            .map((element) => element.closest(".message-card"))
            .find((element) => element?.classList.contains("bubble-you"));
          expect(promptBubble).toBeTruthy();
          expect(promptBubble?.classList.contains("pending-prompt-card")).toBe(
            false,
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

  it.each([
    {
      label: "stopped",
      status: "idle" as const,
      terminalText: "Turn stopped by user.",
    },
    {
      label: "failed",
      status: "error" as const,
      terminalText: "Turn failed: runtime channel closed.",
    },
  ])(
    "keeps $label terminal message deltas visible across equal-revision state snapshots",
    async ({ status, terminalText }) => {
      await withSuppressedActWarnings(async () => {
        const originalFetch = globalThis.fetch;
        const originalEventSource = globalThis.EventSource;
        const originalResizeObserver = globalThis.ResizeObserver;
        const initialSession = makeSession("session-1", {
          name: "Codex Session",
          status: "active",
          preview: "Partial output",
          messagesLoaded: true,
          messageCount: 2,
          sessionMutationStamp: 10,
          messages: [
            {
              id: "message-user-1",
              type: "text",
              timestamp: "10:00",
              author: "you",
              text: "Run the task",
            },
            {
              id: "message-assistant-1",
              type: "text",
              timestamp: "10:01",
              author: "assistant",
              text: "Partial output",
            },
          ],
        });
        const terminalMessage = {
          id: "message-terminal",
          type: "text" as const,
          timestamp: "10:02",
          author: "assistant" as const,
          text: terminalText,
        };
        const equalRevisionSummary = makeSession("session-1", {
          ...initialSession,
          status,
          preview: terminalText,
          messagesLoaded: false,
          messageCount: 3,
          sessionMutationStamp: 11,
          messages: [],
        });
        const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(
              makeStateResponse({
                revision: 1,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [initialSession],
              }),
            );
          }
          if (requestUrl.pathname === "/api/git/status") {
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: "/tmp",
              upstream: "origin/main",
              workdir: "/tmp",
            });
          }

          throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
          await dispatchOpenedStateEvent(
            eventSource,
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [initialSession],
            }),
          );
          await openSessionByName("Codex Session");
          expect(screen.getAllByText("Partial output").length).toBeGreaterThan(0);

          await act(async () => {
            eventSource.dispatchNamedEvent("delta", {
              type: "messageCreated",
              revision: 2,
              sessionId: "session-1",
              messageId: terminalMessage.id,
              messageIndex: 2,
              messageCount: 3,
              message: terminalMessage,
              preview: terminalText,
              status,
              sessionMutationStamp: 11,
            });
            await flushUiWork();
          });
          await waitFor(() => {
            expect(screen.getAllByText(terminalText).length).toBeGreaterThan(0);
          });

          await dispatchStateEvent(
            eventSource,
            makeStateResponse({
              revision: 2,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [equalRevisionSummary],
            }),
          );

          await waitFor(() => {
            expect(screen.getAllByText(terminalText).length).toBeGreaterThan(0);
            expect(
              document.querySelector(".session-conversation-page")?.textContent,
            ).toContain(terminalText);
          });
        } finally {
          scrollIntoViewSpy.mockRestore();
          restoreGlobal("fetch", originalFetch);
          restoreGlobal("EventSource", originalEventSource);
          restoreGlobal("ResizeObserver", originalResizeObserver);
        }
      });
    },
  );


});
