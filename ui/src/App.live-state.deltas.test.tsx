// App.live-state.deltas.test.tsx
//
// Owns: integration tests for the App-level delta-gap handling
// that is driven by ignored or partial live traffic rather than
// reconnect snapshots: ignored deltas, orchestrator-only deltas,
// session-specific stale windows, and the "no assistant output yet"
// guard paths.
//
// Does not own:
//   - reconnect-focused tests, which live in
//     App.live-state.reconnect.test.tsx
//   - visibility / wake-gap recovery tests, which live in
//     App.live-state.visibility.test.tsx
//   - queued-follow-up and watchdog cooldown tests, which
//     live in App.live-state.watchdog.test.tsx
// Split out of: ui/src/App.test.tsx, then reduced in Slice 3R.
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

  function expectRenderedMarkdownTableContains(expectedCells: string[]) {
    // The streaming pipeline now defers any table/fence/math block
    // for the entire duration of `isStreaming` (see
    // `markdown-streaming-split.ts::deferAllBlocks`). That means a
    // streamed table's cells live in the
    // `.markdown-streaming-fragment` placeholder (an ASCII `<pre>`)
    // until `isStreaming` flips false at turn-end, at which point
    // react-markdown produces a real `<table>` inside
    // `.markdown-table-scroll`. The data-presence assertion below
    // therefore accepts EITHER container — these revision-gap tests
    // care that all table cells reached the bubble, not which
    // rendering shape they currently sit in.
    const table = document.querySelector(".markdown-table-scroll table");
    const fragment = document.querySelector(".markdown-streaming-fragment");
    expect(table || fragment).not.toBeNull();
    const containerText = table?.textContent ?? fragment?.textContent ?? "";
    for (const cell of expectedCells) {
      expect(containerText).toContain(cell);
    }
  }

  it("coalesces live session delta renders through one animation frame", async () => {
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
        await dispatchOpenedStateEvent(eventSource, {
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
        expect(screen.getAllByText("Partial output.").length).toBeGreaterThan(
          0,
        );

        let nextFrameId = 1;
        const pendingFrames = new Map<number, FrameRequestCallback>();
        const requestAnimationFrameMock = vi.fn(
          (callback: FrameRequestCallback) => {
            const frameId = nextFrameId;
            nextFrameId += 1;
            pendingFrames.set(frameId, callback);
            return frameId;
          },
        );
        const cancelAnimationFrameMock = vi.fn((frameId: number) => {
          pendingFrames.delete(frameId);
        });
        vi.stubGlobal(
          "requestAnimationFrame",
          requestAnimationFrameMock as unknown as typeof requestAnimationFrame,
        );
        vi.stubGlobal(
          "cancelAnimationFrame",
          cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame,
        );

        act(() => {
          for (let revision = 2; revision <= 4; revision += 1) {
            eventSource.dispatchNamedEvent("delta", {
              type: "textReplace",
              revision,
              sessionId: "session-1",
              messageId: "message-assistant-1",
              messageIndex: 1,
              messageCount: 2,
              text: `Live output ${revision}`,
              preview: `Live output ${revision}`,
            });
          }
        });

        expect(requestAnimationFrameMock).toHaveBeenCalledTimes(1);
        expect(screen.queryByText("Live output 4")).not.toBeInTheDocument();

        await act(async () => {
          const callbacks = [...pendingFrames.values()];
          pendingFrames.clear();
          callbacks.forEach((callback) => {
            callback(Date.now());
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(screen.getAllByText("Live output 4").length).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

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
          expectRenderedMarkdownTableContains([
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
          ]);
        });
        await waitFor(() => {
          expect(sessionHydrationRequestCount).toBe(1);
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
          expectRenderedMarkdownTableContains([
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
          ]);
        });
        expect(document.body.textContent).not.toContain("3.19B");

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

  it("requests state resync when session hydration is ahead of the announced summary", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      let stateFetchCount = 0;
      let stateFetchCountAfterHydration = 0;
      let sessionFetchCount = 0;
      const firstHydration = createDeferred<Response>();
      const stateResync = createDeferred<Response>();
      const secondHydration = createDeferred<Response>();
      const aheadSummaryPreview = "Ahead summary preview";
      const aheadTranscriptText = "Hydrated ahead transcript";
      const oldSummary = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Old summary",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [],
      });
      const newSummary = makeSession("session-1", {
        ...oldSummary,
        preview: aheadSummaryPreview,
        messageCount: 2,
        sessionMutationStamp: 11,
      });
      const hydratedSession = makeSession("session-1", {
        ...newSummary,
        messagesLoaded: true,
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
            text: aheadTranscriptText,
          },
        ],
      });
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateFetchCount += 1;
          if (sessionFetchCount > 0) {
            stateFetchCountAfterHydration += 1;
            return stateResync.promise;
          }
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [oldSummary],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionFetchCount += 1;
          return sessionFetchCount === 1
            ? firstHydration.promise
            : secondHydration.promise;
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
        await dispatchOpenedStateEvent(
          latestEventSource(),
          makeStateResponse({
            revision: 1,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [oldSummary],
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
          expect(sessionFetchCount).toBeGreaterThanOrEqual(1);
        });
        await act(async () => {
          firstHydration.resolve(
            jsonResponse({
              revision: 2,
              serverInstanceId: "test-instance",
              session: hydratedSession,
            }),
          );
          await flushUiWork();
        });
        await waitFor(() => {
          expect(stateFetchCountAfterHydration).toBeGreaterThanOrEqual(1);
        });
        expect(
          screen
            .queryAllByText(aheadTranscriptText)
            .some((element) => element.closest(".message-card")),
        ).toBe(false);

        await act(async () => {
          stateResync.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 2,
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [newSummary],
              }),
            ),
          );
          await flushUiWork();
        });
        await waitFor(() => {
          expect(sessionFetchCount).toBeGreaterThanOrEqual(2);
        });
        await act(async () => {
          secondHydration.resolve(
            jsonResponse({
              revision: 2,
              serverInstanceId: "test-instance",
              session: hydratedSession,
            }),
          );
          await flushUiWork();
        });
        await waitFor(() => {
          const messageCard = screen
            .getAllByText(aheadTranscriptText)
            .map((element) => element.closest(".message-card"))
            .find((element) => element?.classList.contains("bubble-assistant"));
          expect(messageCard).toBeTruthy();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("rejects stale session hydration from a superseded server instance", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const staleHydration = createDeferred<Response>();
      let sessionFetchCount = 0;
      const oldInstanceSummary = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Old instance summary",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [],
      });
      const newInstanceSummary = makeSession("session-1", {
        ...oldInstanceSummary,
        preview: "New instance summary",
      });
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 5,
              serverInstanceId: "old-instance",
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [oldInstanceSummary],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionFetchCount += 1;
          if (sessionFetchCount === 1) {
            return staleHydration.promise;
          }
          return jsonResponse({
            revision: 1,
            serverInstanceId: "new-instance",
            session: makeSession("session-1", {
              name: "Codex Session",
              status: "active",
              preview: "New instance transcript",
              messagesLoaded: true,
              messageCount: 1,
              sessionMutationStamp: 10,
              messages: [
                {
                  id: "message-new-instance",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "New instance transcript",
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
            revision: 5,
            serverInstanceId: "old-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [oldInstanceSummary],
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
            revision: 1,
            serverInstanceId: "new-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [newInstanceSummary],
          }),
        );

        await act(async () => {
          staleHydration.resolve(
            jsonResponse({
              revision: 5,
              serverInstanceId: "old-instance",
              session: makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Old instance transcript",
                messagesLoaded: true,
                messageCount: 1,
                sessionMutationStamp: 10,
                messages: [
                  {
                    id: "message-old-instance",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Old instance transcript",
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
            screen.getAllByText("New instance transcript").length,
          ).toBeGreaterThan(0);
        });
        expect(
          screen.queryByText("Old instance transcript"),
        ).not.toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("rejects an unknown cross-instance full state snapshot without restart evidence", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const currentSession = makeSession("session-current", {
        name: "Current Session",
        status: "idle",
        preview: "Current instance preview",
      });
      const staleSession = makeSession("session-stale", {
        name: "Stale Session",
        status: "idle",
        preview: "Unknown old instance preview",
      });
      const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const requestUrl = new URL(String(input), "http://localhost");
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
        if (requestUrl.pathname.startsWith("/api/workspaces/")) {
          if ((init?.method ?? "GET").toUpperCase() === "PUT") {
            return jsonResponse({ ok: true });
          }

          return new Response("", { status: 404 });
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
            revision: 5,
            serverInstanceId: "current-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [currentSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        expect(
          within(sessionList).getByText("Current instance preview"),
        ).toBeInTheDocument();

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 6,
            serverInstanceId: "unknown-old-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [staleSession],
          }),
        );
        await settleAsyncUi();

        expect(
          within(sessionList).getByText("Current instance preview"),
        ).toBeInTheDocument();
        expect(
          within(sessionList).queryByText("Unknown old instance preview"),
        ).not.toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("resyncs instead of adopting an unknown mismatched session hydration response", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const currentSummary = makeSession("session-1", {
        name: "Codex Session",
        projectId: "project-termal",
        workdir: "/projects/termal",
        status: "active",
        preview: "Current instance summary",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 20,
        messages: [],
      });
      const currentFullSession = makeSession("session-1", {
        ...currentSummary,
        preview: "Current authoritative transcript",
        messagesLoaded: true,
        messages: [
          {
            id: "message-current-instance",
            type: "text",
            timestamp: "10:04",
            author: "assistant",
            text: "Current authoritative transcript",
          },
        ],
      });
      const firstHydration = createDeferred<Response>();
      const stateResync = createDeferred<Response>();
      let sessionHydrationFetchCount = 0;
      let stateFetchCountAfterSessionHydration = 0;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            const isHydrationResync = sessionHydrationFetchCount > 0;
            if (isHydrationResync) {
              stateFetchCountAfterSessionHydration += 1;
              return stateResync.promise;
            }
            return jsonResponse(
              makeStateResponse({
                revision: 5,
                serverInstanceId: "current-instance",
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [currentSummary],
              }),
            );
          }
          if (requestUrl.pathname === "/api/sessions/session-1") {
            sessionHydrationFetchCount += 1;
            return firstHydration.promise;
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
          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
            if ((init?.method ?? "GET").toUpperCase() === "PUT") {
              return jsonResponse({ ok: true });
            }

            return new Response("", { status: 404 });
          }

          throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
        },
      );
      const workspace: WorkspaceState = {
        root: { type: "pane", paneId: "pane-main" },
        panes: [
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
        ],
        activePaneId: "pane-main",
      };
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=unknown-mismatched-session-hydration",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:unknown-mismatched-session-hydration",
        JSON.stringify({
          controlPanelSide: "right",
          workspace,
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 5,
            serverInstanceId: "current-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [currentSummary],
          }),
        );

        await waitFor(() => {
          expect(sessionHydrationFetchCount).toBe(1);
        });
        const setTimeoutSpy = vi.spyOn(window, "setTimeout");
        setTimeoutSpy.mockClear();

        await act(async () => {
          firstHydration.resolve(
            jsonResponse({
              revision: 4,
              serverInstanceId: "unknown-old-instance",
              session: makeSession("session-1", {
                name: "Codex Session",
                status: "active",
                preview: "Unknown old transcript",
                messagesLoaded: true,
                messageCount: 1,
                sessionMutationStamp: 20,
                messages: [
                  {
                    id: "message-unknown-old",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Unknown old transcript",
                  },
                ],
              }),
            }),
          );
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(stateFetchCountAfterSessionHydration).toBeGreaterThanOrEqual(1);
        expect(
          setTimeoutSpy.mock.calls.some(([, delay]) => delay === 50),
        ).toBe(false);
        setTimeoutSpy.mockRestore();
        expect(sessionHydrationFetchCount).toBe(1);

        await act(async () => {
          stateResync.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 6,
                serverInstanceId: "current-instance",
                projects: [],
                orchestrators: [],
                workspaces: [],
                sessions: [currentFullSession],
              }),
            ),
          );
          await flushUiWork();
        });
        await waitFor(() => {
          expect(stateFetchCountAfterSessionHydration).toBeGreaterThanOrEqual(1);
        });
        await waitFor(() => {
          expect(
            screen.getAllByText("Current authoritative transcript").length,
          ).toBeGreaterThan(0);
        });
        expect(screen.queryByText("Unknown old transcript")).not.toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("deep-reconciles the first full snapshot after a mismatched session hydration response", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const sessionOneOldSummary = makeSession("session-1", {
        name: "Session One",
        projectId: "project-termal",
        workdir: "/projects/termal",
        status: "active",
        preview: "Old instance first summary",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 0,
        messages: [],
      });
      const sessionOneNew = makeSession("session-1", {
        ...sessionOneOldSummary,
        preview: "New instance first transcript",
        messagesLoaded: true,
        messages: [
          {
            id: "message-new-first",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "New instance first transcript",
          },
        ],
      });
      const sessionTwoOld = makeSession("session-2", {
        name: "Session Two",
        projectId: "project-termal",
        workdir: "/projects/termal",
        status: "idle",
        preview: "Old instance second transcript",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 0,
        messages: [
          {
            id: "message-old-second",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Old instance second transcript",
          },
        ],
      });
      const sessionTwoNew = makeSession("session-2", {
        ...sessionTwoOld,
        preview: "New instance second transcript",
        messages: [
          {
            id: "message-new-second",
            type: "text",
            timestamp: "10:02",
            author: "assistant",
            text: "New instance second transcript",
          },
        ],
      });
      let sessionHydrationFetchSeen = false;
      let stateFetchCountAfterSessionHydration = 0;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            const isHydrationResync = sessionHydrationFetchSeen;
            if (isHydrationResync) {
              stateFetchCountAfterSessionHydration += 1;
            }
            return jsonResponse(
              !isHydrationResync
                ? makeStateResponse({
                    revision: 5,
                    serverInstanceId: "old-instance",
                    projects: [],
                    orchestrators: [],
                    workspaces: [],
                    sessions: [sessionOneOldSummary, sessionTwoOld],
                  })
                : makeStateResponse({
                    revision: 2,
                    serverInstanceId: "new-instance",
                    projects: [],
                    orchestrators: [],
                    workspaces: [],
                    sessions: [sessionOneNew, sessionTwoNew],
                  }),
            );
          }
          if (requestUrl.pathname === "/api/sessions/session-1") {
            sessionHydrationFetchSeen = true;
            return jsonResponse({
              revision: 1,
              serverInstanceId: "new-instance",
              session: sessionOneNew,
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
          if (requestUrl.pathname.startsWith("/api/workspaces/")) {
            if ((init?.method ?? "GET").toUpperCase() === "PUT") {
              return jsonResponse({ ok: true });
            }

            return new Response("", { status: 404 });
          }

          throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
        },
      );
      const workspace: WorkspaceState = {
        root: { type: "pane", paneId: "pane-main" },
        panes: [
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
        ],
        activePaneId: "pane-main",
      };
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=mismatched-session-hydration-resync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:mismatched-session-hydration-resync",
        JSON.stringify({
          controlPanelSide: "right",
          workspace,
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
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 5,
            serverInstanceId: "old-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [sessionOneOldSummary, sessionTwoOld],
          }),
        );

        await waitFor(() => {
          expect(sessionHydrationFetchSeen).toBe(true);
          expect(stateFetchCountAfterSessionHydration).toBeGreaterThanOrEqual(1);
          expect(
            screen.getAllByText("New instance first transcript").length,
          ).toBeGreaterThan(0);
        });

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 2,
            serverInstanceId: "new-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [sessionOneNew, sessionTwoNew],
          }),
        );
        await settleAsyncUi();

        await waitFor(() => {
          const latestSessionList = document.querySelector(".session-list");
          if (!(latestSessionList instanceof HTMLDivElement)) {
            throw new Error("Session list not found after full restart state");
          }
          expect(
            within(latestSessionList).getByText(
              "New instance second transcript",
            ),
          ).toBeInTheDocument();
          expect(
            within(latestSessionList).queryByText(
              "Old instance second transcript",
            ),
          ).not.toBeInTheDocument();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("hydrates all visible metadata-only session panes after startup", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const state = makeStateResponse({
        revision: 1,
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
          makeSession("session-left", {
            name: "Left Session",
            projectId: "project-termal",
            workdir: "/projects/termal",
            messagesLoaded: false,
            messageCount: 1,
            preview: "Left summary",
            messages: [],
          }),
          makeSession("session-right", {
            name: "Right Session",
            projectId: "project-termal",
            workdir: "/projects/termal",
            messagesLoaded: false,
            messageCount: 1,
            preview: "Right summary",
            messages: [],
          }),
        ],
      });
      const hydratedSessions = new Map([
        [
          "session-left",
          makeSession("session-left", {
            name: "Left Session",
            projectId: "project-termal",
            workdir: "/projects/termal",
            messagesLoaded: true,
            messageCount: 1,
            preview: "Left transcript",
            messages: [
              {
                id: "message-left",
                type: "text",
                timestamp: "10:01",
                author: "assistant",
                text: "Left transcript",
              },
            ],
          }),
        ],
        [
          "session-right",
          makeSession("session-right", {
            name: "Right Session",
            projectId: "project-termal",
            workdir: "/projects/termal",
            messagesLoaded: true,
            messageCount: 1,
            preview: "Right transcript",
            messages: [
              {
                id: "message-right",
                type: "text",
                timestamp: "10:02",
                author: "assistant",
                text: "Right transcript",
              },
            ],
          }),
        ],
      ]);
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(state);
          }
          if (requestUrl.pathname.startsWith("/api/sessions/")) {
            const sessionId = decodeURIComponent(
              requestUrl.pathname.split("/").pop() ?? "",
            );
            const session = hydratedSessions.get(sessionId);
            if (!session) {
              return new Response("", { status: 404 });
            }
            return jsonResponse({
              revision: 2,
              serverInstanceId: "test-instance",
              session,
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
      const workspace: WorkspaceState = {
        root: {
          id: "split-root",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: { type: "pane", paneId: "pane-left" },
          second: { type: "pane", paneId: "pane-right" },
        },
        panes: [
          {
            id: "pane-left",
            tabs: [
              {
                id: "tab-left",
                kind: "session",
                sessionId: "session-left",
              },
            ],
            activeTabId: "tab-left",
            activeSessionId: "session-left",
            viewMode: "session",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
          {
            id: "pane-right",
            tabs: [
              {
                id: "tab-right",
                kind: "session",
                sessionId: "session-right",
              },
            ],
            activeTabId: "tab-right",
            activeSessionId: "session-right",
            viewMode: "session",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-right",
      };

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=visible-session-hydration",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:visible-session-hydration",
        JSON.stringify({
          controlPanelSide: "right",
          workspace,
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
        await dispatchOpenedStateEvent(latestEventSource(), state);

        await waitFor(() => {
          const fetchedSessionIds = fetchMock.mock.calls
            .map(([input]) => new URL(String(input), "http://localhost"))
            .filter((url) => url.pathname.startsWith("/api/sessions/"))
            .map((url) => decodeURIComponent(url.pathname.split("/").pop() ?? ""));
          expect(fetchedSessionIds).toEqual(
            expect.arrayContaining(["session-left", "session-right"]),
          );
        });
        expect(
          (await screen.findAllByText("Left transcript")).length,
        ).toBeGreaterThan(0);
        expect(
          (await screen.findAllByText("Right transcript")).length,
        ).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("coalesces Codex global-state delta renders through one animation frame", async () => {
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
            sessions: [],
            codex: {},
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
        await dispatchOpenedStateEvent(eventSource, {
          revision: 1,
          projects: [],
          sessions: [],
          codex: {},
        });

        const pendingFrames = new Map<number, FrameRequestCallback>();
        const requestAnimationFrameMock = vi.fn(
          (callback: FrameRequestCallback) => {
            const frameId = pendingFrames.size + 1;
            pendingFrames.set(frameId, callback);
            return frameId;
          },
        );
        const cancelAnimationFrameMock = vi.fn((frameId: number) => {
          pendingFrames.delete(frameId);
        });
        vi.stubGlobal(
          "requestAnimationFrame",
          requestAnimationFrameMock as unknown as typeof requestAnimationFrame,
        );
        vi.stubGlobal(
          "cancelAnimationFrame",
          cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame,
        );

        act(() => {
          for (let revision = 2; revision <= 4; revision += 1) {
            eventSource.dispatchNamedEvent("delta", {
              type: "codexUpdated",
              revision,
              codex: {
                notices: [
                  {
                    kind: "runtimeNotice",
                    level: "warning",
                    title: `Codex notice ${revision}`,
                    detail: "Rendered through a coalesced frame.",
                    timestamp: "2026-04-06T00:00:00Z",
                  },
                ],
              },
            });
          }
        });

        expect(requestAnimationFrameMock).toHaveBeenCalledTimes(1);

        await act(async () => {
          const callbacks = [...pendingFrames.values()];
          pendingFrames.clear();
          callbacks.forEach((callback) => {
            callback(Date.now());
          });
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
            messageCount: 2,
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
            messageCount: 2,
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
          messageCount: 2,
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
            messageCount: 2,
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
            messageCount: 2,
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

  it("watchdog-resyncs a quiet active session even before any assistant output arrives", async () => {
    // The user sent a prompt and the session went active, but the assistant's
    // first SSE delta never arrived. Without watchdog recovery the user is
    // stuck staring at "Waiting for the next chunk of output..." forever.
    // Once the staleness window elapses, the watchdog must resync — see
    // bugs.md "Watchdog ignored user-prompt turn boundaries…".
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

      // After one stale window the watchdog should fire even though no
      // assistant chunks have arrived yet — the user prompt at the turn
      // boundary counts as in-turn activity that's gone silent for too long.
      await advanceTimers(
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000,
      );
      await settleAsyncUi();

      expect(stateFetchCallCount()).toBeGreaterThanOrEqual(1);
      // The recovery snapshot the watchdog fetched shows the assistant reply
      // that the lost SSE chunk would have streamed in, so the recovered
      // text becomes visible (the "waiting" affordance only disappears once
      // the session transitions to idle, which it does in the recovery
      // snapshot).
      expect(
        screen.getAllByText("Quiet turn finished.").length,
      ).toBeGreaterThan(0);
    } finally {
      vi.useRealTimers();
      setDocumentVisibilityState(originalVisibilityState);
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("force re-hydrates the session when a delta arrives whose target is missing on a hydrated transcript", async () => {
    // Scenario: a hydrated session ends a turn but the SSE deltas that
    // would have streamed in the assistant reply were dropped. The next
    // delta references an unknown messageId. The reducer returns
    // `needsResync`, the caller schedules `/api/state`, and ALSO calls
    // `startSessionHydration` directly so the per-session full-transcript
    // fetch happens regardless of whether the metadata-first summary
    // would flip `messagesLoaded` back to false. Without the explicit
    // hydration trigger the user can stay on a stale transcript until they
    // refresh the page. See bugs.md "Stuck assistant reply visible only
    // after refresh".
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const initialSession = makeSession("session-1", {
      name: "Codex Session",
      status: "active",
      preview: "First message",
      messagesLoaded: true,
      messageCount: 1,
      sessionMutationStamp: 11,
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First message",
        },
      ],
    });
    const recoveredSession = makeSession("session-1", {
      ...initialSession,
      status: "idle",
      preview: "Recovered assistant reply.",
      messagesLoaded: true,
      messageCount: 2,
      sessionMutationStamp: 12,
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First message",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Recovered assistant reply.",
        },
      ],
    });
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const requestUrl = new URL(String(input), "http://localhost");
      if (requestUrl.pathname === "/api/state") {
        return jsonResponse(
          makeStateResponse({
            revision: 5,
            projects: [],
            orchestrators: [],
            workspaces: [],
            // Summary stamp matches the local stamp the metadata-only
            // patch would have settled on, so reconcileSummarySession
            // would NOT downgrade messagesLoaded — proving the explicit
            // hydration trigger is what surfaces the missing message
            // body.
            sessions: [
              makeSession("session-1", {
                ...initialSession,
                status: "idle",
                preview: "Recovered assistant reply.",
                messageCount: 2,
                sessionMutationStamp: 12,
                messagesLoaded: true,
              }),
            ],
          }),
        );
      }
      if (requestUrl.pathname === "/api/sessions/session-1") {
        return jsonResponse({
          revision: 5,
          serverInstanceId: "test-instance",
          session: recoveredSession,
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
      expect(screen.getAllByText("First message").length).toBeGreaterThan(0);
      fetchMock.mockClear();

      // Dispatch a textDelta whose target message is unknown to the local
      // hydrated transcript. The reducer returns needsResync.
      await act(async () => {
        eventSource.dispatchNamedEvent("delta", {
          type: "textDelta",
          revision: 2,
          sessionId: "session-1",
          messageId: "message-assistant-1",
          messageIndex: 1,
          messageCount: 2,
          delta: " reply.",
          preview: "Recovered assistant reply.",
          sessionMutationStamp: 12,
        });
        await flushUiWork();
      });
      await settleAsyncUi();

      // Both /api/state AND /api/sessions/session-1 should be fetched —
      // the latter is the per-session re-hydration that this regression
      // pins. Without the direct hydration trigger, only /api/state would
      // fire and the missing message would never appear.
      expect(
        fetchMock.mock.calls.some(
          ([input]) =>
            new URL(String(input), "http://localhost").pathname ===
            "/api/sessions/session-1",
        ),
      ).toBe(true);
      expect(
        screen.getAllByText("Recovered assistant reply.").length,
      ).toBeGreaterThan(0);
    } finally {
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("applies metadata patch immediately and hydrates when an unhydrated session receives a missing-target delta", async () => {
    // Sibling regression for the "force re-hydrates" test above. That test
    // covers the HYDRATED case (`messagesLoaded === true`) which goes
    // through the reducer's `needsResync` branch. THIS test covers the
    // UNHYDRATED case (`messagesLoaded === false`) which goes through the
    // distinct `appliedNeedsResync` branch — the reducer returns the
    // metadata patch (preview/messageCount/status updates) AND the caller
    // schedules `requestStateResync` AND triggers `startSessionHydration`.
    // Closes the bugs.md "appliedNeedsResync end-to-end integration path
    // has no targeted regression" gap; without this test, a regression
    // that swapped `appliedNeedsResync` handling for plain `applied`
    // (skipping the hydration trigger) or `needsResync` (skipping the
    // metadata patch) would slip through unit-only coverage.
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    // Unhydrated initial session: only the user prompt is in retained
    // transcript memory, but `messageCount` reflects what the backend has
    // already created (the assistant reply that hasn't been hydrated).
    const initialSession = makeSession("session-1", {
      name: "Codex Session",
      status: "active",
      preview: "First message",
      messagesLoaded: false,
      messageCount: 1,
      sessionMutationStamp: 11,
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First message",
        },
      ],
    });
    const recoveredSession = makeSession("session-1", {
      ...initialSession,
      status: "idle",
      preview: "Recovered assistant chunk.",
      messagesLoaded: true,
      messageCount: 2,
      sessionMutationStamp: 12,
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First message",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Recovered assistant chunk.",
        },
      ],
    });
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const requestUrl = new URL(String(input), "http://localhost");
      if (requestUrl.pathname === "/api/state") {
        return jsonResponse(
          makeStateResponse({
            revision: 5,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-1", {
                ...initialSession,
                status: "idle",
                preview: "Recovered assistant chunk.",
                messageCount: 2,
                sessionMutationStamp: 12,
                messagesLoaded: false,
              }),
            ],
          }),
        );
      }
      if (requestUrl.pathname === "/api/sessions/session-1") {
        return jsonResponse({
          revision: 5,
          serverInstanceId: "test-instance",
          session: recoveredSession,
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
      // INTENTIONALLY do NOT click the session row open. The
      // visible-session hydration effect (`useEffect` keyed on
      // `visibleSessionHydrationTargets`) already triggers hydration for
      // any unhydrated session that becomes the active pane — that would
      // mask the explicit hydration trigger this test is trying to pin.
      // Leaving the session in the sidebar (not active, not in any pane)
      // means the only path to `/api/sessions/session-1` is the
      // handler's `startSessionHydration` call from the
      // `appliedNeedsResync` branch.
      fetchMock.mockClear();

      // Dispatch a textDelta whose target message is unknown to the
      // unhydrated retained transcript. The reducer returns
      // `appliedNeedsResync`: the metadata patch applies (preview /
      // messageCount / status updates immediately) AND the caller fires
      // /api/state resync + per-session hydration so the missing message
      // body eventually appears.
      await act(async () => {
        eventSource.dispatchNamedEvent("delta", {
          type: "textDelta",
          revision: 2,
          sessionId: "session-1",
          messageId: "message-assistant-1",
          messageIndex: 1,
          messageCount: 2,
          delta: " chunk.",
          preview: "Recovered assistant chunk.",
          sessionMutationStamp: 12,
        });
        await flushUiWork();
      });
      await settleAsyncUi();

      // The metadata patch landed immediately: the sidebar's
      // `.session-preview` tooltip (`title` on the inner preview div in
      // `AppControlSurface.tsx`) reflects the delta's `preview` field
      // even though the assistant message body hasn't been hydrated
      // yet. Without `appliedNeedsResync`, the reducer would have
      // returned plain `needsResync` and the metadata patch would have
      // been dropped — the sidebar would still show the OLD preview.
      const sessionRow = within(sessionList)
        .getByText("Codex Session")
        .closest("button");
      const previewDiv = sessionRow?.querySelector(".session-preview");
      expect(previewDiv?.getAttribute("title")).toBe(
        "Recovered assistant chunk.",
      );

      // The targeted hydration fetch fires — distinguishing this branch
      // from a plain `applied` reducer return that would skip hydration.
      // The session is NOT the active pane, so the only way
      // `/api/sessions/session-1` could have been fetched is the
      // handler's explicit `startSessionHydration` call from the
      // `appliedNeedsResync` branch.
      expect(
        fetchMock.mock.calls.some(
          ([input]) =>
            new URL(String(input), "http://localhost").pathname ===
            "/api/sessions/session-1",
        ),
      ).toBe(true);
    } finally {
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

});
