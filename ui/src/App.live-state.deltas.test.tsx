// App.live-state.deltas.test.tsx
//
// Owns: integration tests for the App-level delta-gap handling,
// stale transport watchdog, wake-gap recovery, and visibility /
// focus gating of transport recovery. Covers the paths where
// missing or ignored deltas, clock drift, or hidden-then-visible
// transitions drive a resync without relying on the SSE
// reconnect-specific flows in the sibling file.
//
// Does not own: reconnect-focused tests — reconnect snapshot
// adoption, stale reconnect rejection, fallback /api/state
// resyncs, and same-revision backend restart handling live in
// App.live-state.reconnect.test.tsx.
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

describe("App live state — deltas and watchdog", () => {
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

});
