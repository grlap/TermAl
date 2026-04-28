// App.live-state.visibility.test.tsx
//
// Owns: integration tests for visibility / focus / wake-gap
// recovery: resync after hidden output, focus suppression while
// hidden, focused wake without visibility events, stale hidden
// transport, and post-wake gap recovery.
//
// Does not own:
//   - reconnect-focused tests, which live in
//     App.live-state.reconnect.test.tsx
//   - ignored-delta / orchestrator-delta core watchdog tests,
//     which live in App.live-state.deltas.test.tsx
//   - queued-follow-up and watchdog cooldown tests, which live in
//     App.live-state.watchdog.test.tsx
//
// Split out of: ui/src/App.live-state.deltas.test.tsx during Slice 3R.
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

describe("App live state - visibility and wake recovery", () => {
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

  it("resyncs when the page becomes visible again after a live reply finishes while hidden", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalVisibilityState = document.visibilityState;
      const hiddenState = {
        revision: 2,
        serverInstanceId: "replacement-instance",
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
      };
      const stateResponseDeferred = createDeferred<Response>();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return stateResponseDeferred.promise;
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
            serverInstanceId: "current-instance",
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
        await act(async () => {
          stateResponseDeferred.resolve(jsonResponse(hiddenState));
          await flushUiWork();
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
  });

  it("recovers a replacement-instance snapshot returned by an approval action", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const approvalEndpoint =
        "/api/sessions/session-1/approvals/message-approval-1";
      const recoveredState = {
        revision: 2,
        serverInstanceId: "replacement-instance",
        projects: [],
        sessions: [
          makeSession("session-1", {
            name: "Codex Session",
            status: "idle",
            preview: "Recovered after approval action.",
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
                text: "Recovered after approval action.",
              },
            ],
          }),
        ],
      };
      let stateRequestCount = 0;
      const stateResponseDeferred = createDeferred<Response>();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === approvalEndpoint) {
          return jsonResponse(recoveredState);
        }
        if (url === "/api/state") {
          stateRequestCount += 1;
          return stateResponseDeferred.promise;
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
            serverInstanceId: "current-instance",
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

        const sessionRowButton = within(sessionList)
          .getByText("Codex Session")
          .closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }

        await clickAndSettle(sessionRowButton);
        await clickAndSettle(screen.getByRole("button", { name: "Approve" }));

        await waitFor(() => {
          expect(stateRequestCount).toBe(1);
        });
        await act(async () => {
          stateResponseDeferred.resolve(jsonResponse(recoveredState));
          await flushUiWork();
        });
        await waitFor(() => {
          expect(
            screen.getAllByText("Recovered after approval action."),
          ).toHaveLength(2);
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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
});
