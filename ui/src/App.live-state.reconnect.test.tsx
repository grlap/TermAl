// App.live-state.reconnect.test.tsx
//
// Owns: integration tests for the App-level SSE reconnect /
// fallback-state-resync flows — reconnect snapshot adoption,
// stale reconnect rejection, fallback /api/state resyncs,
// post-hydration stream-error recovery, and same-revision backend
// restart handling. Verifies ordering invariants between
// reconnect resync, delta-gap resync, and transient /api/state
// failures without touching the watchdog or wake-gap paths.
//
// Does not own: delta-gap / watchdog / wake-gap / visibility
// recovery (those live in App.live-state.deltas.test.tsx),
// workspace-layout tests, session-lifecycle tests, or any other
// App.*.test.tsx domain split.
//
// Split out of: ui/src/App.test.tsx (Slice 3 of the App-split
// plan, see docs/app-split-plan.md).
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
import { RECONNECT_STATE_RESYNC_DELAY_MS } from "./app-shell-internals";
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

const RECONNECT_STATE_RESYNC_TEST_BUFFER_MS = Math.max(
  1,
  Math.min(100, Math.floor(RECONNECT_STATE_RESYNC_DELAY_MS / 4)),
);

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

describe("App live state — reconnect", () => {
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
          messageCount: 2,
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
          sessionId: "session-1",
          messageId: "message-gap-1",
          messageIndex: 1,
          messageCount: 2,
          message: {
            id: "message-gap-1",
            type: "text",
            timestamp: "10:02",
            author: "assistant",
            text: "Gap output",
          },
          preview: "Gap output",
          status: "active",
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
          messageCount: 2,
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

      await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS - 101);
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
          sessionId: "session-1",
          messageId: "message-gap-1",
          messageIndex: 1,
          messageCount: 2,
          message: {
            id: "message-gap-1",
            type: "text",
            timestamp: "10:02",
            author: "assistant",
            text: "Gap output",
          },
          preview: "Gap output",
          status: "active",
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
          sessionId: "session-1",
          messageId: "message-gap-2",
          messageIndex: 1,
          messageCount: 2,
          message: {
            id: "message-gap-2",
            type: "text",
            timestamp: "10:03",
            author: "assistant",
            text: "Later gap output",
          },
          preview: "Later gap output",
          status: "active",
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

  it("adopts a newer replacement-instance snapshot from reconnect fallback recovery", async () => {
    await withSuppressedActWarnings(async () => {
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(
        makeStateResponse({
          revision: 6,
          serverInstanceId: "replacement-instance",
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [
            makeSession("session-recovered", {
              name: "Replacement Session",
              preview: "Recovered from replacement instance",
            }),
          ],
        }),
      );

      try {
        await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
          await dispatchOpenedStateEvent(
            eventSource,
            makeStateResponse({
              revision: 5,
              serverInstanceId: "current-instance",
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-current", {
                  name: "Current Session",
                  preview: "Current preview",
                }),
              ],
            }),
          );
          await within(sessionList).findByText("Current Session");

          await dispatchStateEvent(eventSource, {
            _sseFallback: true,
            revision: 6,
            serverInstanceId: "replacement-instance",
            projects: [],
            sessions: [],
          });
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Replacement Session"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).getByText("Recovered from replacement instance"),
          ).toBeInTheDocument();
          expect(
            within(sessionList).queryByText("Current Session"),
          ).not.toBeInTheDocument();
        });
      } finally {
        fetchStateSpy.mockRestore();
      }
    });
  });

  it("keeps reconnect fallback polling armed after replacement-instance fallback adoption until SSE reopens", async () => {
    const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(
      makeStateResponse({
        revision: 6,
        serverInstanceId: "replacement-instance",
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-recovered", {
            name: "Replacement Session",
            preview: "Recovered before SSE reopened",
          }),
        ],
      }),
    );

    try {
      await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
        let fakeTimersActive = false;
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 5,
            serverInstanceId: "current-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-current", {
                name: "Current Session",
                preview: "Current preview",
              }),
            ],
          }),
        );
        await within(sessionList).findByText("Current Session");

        vi.useFakeTimers();
        fakeTimersActive = true;
        try {
          act(() => {
            eventSource.dispatchError();
          });

          await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS);
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Replacement Session"),
          ).toBeInTheDocument();

          await advanceTimers(
            RECONNECT_STATE_RESYNC_DELAY_MS * 2 -
              RECONNECT_STATE_RESYNC_TEST_BUFFER_MS,
          );
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);

          await advanceTimers(RECONNECT_STATE_RESYNC_TEST_BUFFER_MS * 2);
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(2);
        } finally {
          if (fakeTimersActive) {
            vi.useRealTimers();
          }
        }
      });
    } finally {
      if (vi.isFakeTimers()) {
        vi.useRealTimers();
      }
      fetchStateSpy.mockRestore();
    }
  });

  it("stops automatic same-instance polling after catch-up advances the revision", async () => {
    const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(
      makeStateResponse({
        revision: 6,
        serverInstanceId: "current-instance",
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-current", {
            name: "Current Session",
            preview: "Recovered before SSE delivered data",
          }),
        ],
      }),
    );

    try {
      await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
        let fakeTimersActive = false;
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 5,
            serverInstanceId: "current-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-current", {
                name: "Current Session",
                preview: "Current preview",
              }),
            ],
          }),
        );
        await within(sessionList).findByText("Current Session");

        vi.useFakeTimers();
        fakeTimersActive = true;
        try {
          act(() => {
            eventSource.dispatchError();
          });

          await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS);
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Recovered before SSE delivered data"),
          ).toBeInTheDocument();

          await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS * 4);
          await settleAsyncUi();
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        } finally {
          if (fakeTimersActive) {
            vi.useRealTimers();
          }
        }
      });
    } finally {
      if (vi.isFakeTimers()) {
        vi.useRealTimers();
      }
      fetchStateSpy.mockRestore();
    }
  });

  it("disarms replacement-instance fallback polling after SSE state confirms recovery", async () => {
    const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(
      makeStateResponse({
        revision: 6,
        serverInstanceId: "replacement-instance",
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-recovered", {
            name: "Replacement Session",
            preview: "Recovered before SSE reopened",
          }),
        ],
      }),
    );

    try {
      await withFallbackStateHarness(async ({ eventSource, sessionList }) => {
        let fakeTimersActive = false;
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 5,
            serverInstanceId: "current-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-current", {
                name: "Current Session",
                preview: "Current preview",
              }),
            ],
          }),
        );
        await within(sessionList).findByText("Current Session");

        vi.useFakeTimers();
        fakeTimersActive = true;
        try {
          act(() => {
            eventSource.dispatchError();
          });

          await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS);
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
          expect(
            within(sessionList).getByText("Replacement Session"),
          ).toBeInTheDocument();

          await advanceTimers(
            RECONNECT_STATE_RESYNC_DELAY_MS * 2 -
              RECONNECT_STATE_RESYNC_TEST_BUFFER_MS,
          );
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);

          await advanceTimers(RECONNECT_STATE_RESYNC_TEST_BUFFER_MS * 2);
          await settleAsyncUi();
          expect(fetchStateSpy).toHaveBeenCalledTimes(2);

          act(() => {
            eventSource.dispatchOpen();
          });
          await dispatchStateEvent(
            eventSource,
            makeStateResponse({
              revision: 7,
              serverInstanceId: "replacement-instance",
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-recovered", {
                  name: "Replacement Session",
                  preview: "Recovered after SSE delivered data",
                }),
              ],
            }),
          );
          await settleAsyncUi();

          await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS * 4);
          await settleAsyncUi();

          expect(fetchStateSpy).toHaveBeenCalledTimes(2);
        } finally {
          if (fakeTimersActive) {
            vi.useRealTimers();
          }
        }
      });
    } finally {
      if (vi.isFakeTimers()) {
        vi.useRealTimers();
      }
      fetchStateSpy.mockRestore();
    }
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
          messageCount: 2,
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

  it("adopts a Lagged-recovery state snapshot at the same revision when the backend signals lagged", async () => {
    // Scenario: SSE delta channel fell past broadcast capacity, so the backend
    // emits a `lagged` marker followed by a recovery state snapshot. The
    // recovery snapshot may carry the same revision the client already saw
    // (the client read some events from the burst before falling behind).
    // Without the `lagged` marker arming force-adopt, the client would treat
    // the recovery snapshot as a redundant catch-up and silently drop it,
    // leaving the latest assistant message hidden until the user takes
    // another action. See bugs.md "SSE Lagged-recovery snapshot can be
    // silently ignored".
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        throw new Error("backend should not be probed in this scenario");
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
      expect(screen.queryByText("Lagged recovery body.")).not.toBeInTheDocument();

      // No reconnect — the SSE stream stays open. The backend emits the
      // `lagged` marker (because the delta channel overflowed) and immediately
      // follows with a recovery snapshot at the same revision the client
      // already adopted.
      act(() => {
        eventSource.dispatchNamedEvent("lagged", "");
        eventSource.dispatchNamedEvent("state", {
          revision: 2,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Lagged recovery body.",
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
                  text: "Lagged recovery body.",
                },
              ],
            }),
          ],
        });
      });

      // The recovery snapshot was force-adopted: the new assistant message
      // body is visible without any /api/state probe and without a reconnect
      // open. Without the lagged-marker fix, this assertion would fail
      // because the same-revision snapshot would be rejected as redundant.
      await waitFor(() => {
        expect(
          screen.getAllByText("Lagged recovery body.").length,
        ).toBeGreaterThan(0);
      });
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

  it("recreates the EventSource when the browser closes it permanently after a non-200 response", async () => {
    // Scenario: the dev-mode Vite proxy returns 502 during the brief
    // backend-restart gap, OR the browser otherwise marks the EventSource
    // permanently CLOSED (readyState === 2). The WHATWG spec prohibits
    // auto-reconnect after a non-200 response, so the EventSource is dead
    // and the user has to hard-refresh — unless the client detects the
    // CLOSED state and constructs a fresh EventSource itself. See bugs.md
    // "Browser auto-reconnect gives up after a non-200 SSE response and
    // the client gets stuck".
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    vi.useFakeTimers();
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        // Simulate the dev-server gap: every /api/state probe also fails
        // until the backend is back. The reconnect-fallback timer would
        // therefore not be the recovery path here — only EventSource
        // recreation can succeed once the new backend is up.
        throw new Error("backend unavailable during restart");
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });
    EventSourceMock.instances = [];

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
      const initialEventSource = latestEventSource();

      act(() => {
        initialEventSource.dispatchOpen();
        initialEventSource.dispatchNamedEvent("state", {
          revision: 1,
          projects: [],
          sessions: [
            makeSession("session-1", {
              name: "Codex Session",
              status: "idle",
              preview: "Original preview.",
              messages: [],
            }),
          ],
        });
      });

      // Mark the EventSource as permanently closed (mirrors the browser
      // behavior after a non-200 reconnect attempt) and dispatch the
      // matching error.
      initialEventSource.readyState = 2;
      act(() => {
        initialEventSource.dispatchError();
      });

      // The recovery timer fires after a short backoff and re-runs the
      // transport effect, which constructs a fresh EventSource. Without
      // this fix, the EventSourceMock count would stay at 1 forever and
      // the user would have to hard-refresh.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(750);
      });

      expect(EventSourceMock.instances.length).toBeGreaterThanOrEqual(2);
      const recoveredEventSource =
        EventSourceMock.instances[EventSourceMock.instances.length - 1];
      expect(recoveredEventSource).not.toBe(initialEventSource);
    } finally {
      vi.useRealTimers();
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
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

});
