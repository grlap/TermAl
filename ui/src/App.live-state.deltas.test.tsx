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


});
