// App.live-state.deltas.test.tsx
//
// Owns: integration tests for the App-level delta-gap handling
// that is driven by ignored or partial live traffic rather than reconnect
// snapshots: broad render coalescing, applied-no-op revisions, missing-target
// hydration repair, and same-revision unknown-session repair.
//
// Does not own:
//   - reconnect-focused tests, which live in
//     App.live-state.reconnect.test.tsx
//   - visibility / wake-gap recovery tests, which live in
//     App.live-state.visibility.test.tsx
//   - queued-follow-up and watchdog cooldown tests, which
//     live in App.live-state.watchdog.test.tsx
//   - hydration-focused delta-gap tests, which live in
//     App.live-state.hydration.test.tsx and
//     App.live-state.hydration-recovery.test.tsx
//   - stale-live-transport watchdog delta tests, which live in
//     App.live-state.delta-watchdog.test.tsx
// Split out of: ui/src/App.test.tsx, then reduced in Slice 3R and this file split.
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

  it("coalesces broad live session renders while publishing active session slices eagerly", async () => {
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
        expect(pendingFrames.size).toBe(1);
        expect(screen.getAllByText("Live output 4").length).toBeGreaterThan(0);

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

  it("advances revision for forward appliedNoOp deltas without resyncing", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Stable answer",
        messagesLoaded: true,
        messageCount: 2,
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
            text: "Stable answer",
          },
        ],
      });
      let stateRequestCount = 0;
      let sessionFetchCount = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateRequestCount += 1;
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
          sessionFetchCount += 1;
          return jsonResponse({
            revision: 1,
            serverInstanceId: "test-instance",
            session: initialSession,
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
        await openSessionByName("Codex Session");
        expect(screen.getAllByText("Stable answer").length).toBeGreaterThan(0);

        stateRequestCount = 0;
        sessionFetchCount = 0;
        fetchMock.mockClear();

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: 2,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 1,
            messageCount: 2,
            text: "Stable answer",
            preview: "Stable answer",
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(stateRequestCount).toBe(0);
        expect(sessionFetchCount).toBe(0);

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 3,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 1,
            messageCount: 2,
            delta: " continued",
            preview: "Stable answer continued",
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(stateRequestCount).toBe(0);
        expect(sessionFetchCount).toBe(0);
        expect(
          screen.getAllByText("Stable answer continued").length,
        ).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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

  it("waits for authoritative state after a same-revision delta references an unknown session", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "idle",
        preview: "Known session",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [
          {
            id: "message-known",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Known session",
          },
        ],
      });
      const newSession = makeSession("session-x", {
        name: "New Session",
        status: "idle",
        preview: "New session arrived authoritatively.",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 1,
        messages: [
          {
            id: "message-new-session",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "New session arrived authoritatively.",
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
      const stateFetchCount = () =>
        fetchMock.mock.calls.filter(
          ([input]) =>
            new URL(String(input), "http://localhost").pathname === "/api/state",
        ).length;

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
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        expect(screen.getByText("Codex Session")).toBeInTheDocument();
        fetchMock.mockClear();

        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageCreated",
            revision: 5,
            sessionId: newSession.id,
            messageId: "message-new-session",
            messageIndex: 0,
            messageCount: 1,
            message: newSession.messages[0],
            preview: "New session arrived authoritatively.",
            status: "idle",
            sessionMutationStamp: 1,
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(stateFetchCount()).toBe(0);
        expect(screen.queryByText("New Session")).not.toBeInTheDocument();

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 6,
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession, newSession],
          }),
        );
        await settleAsyncUi();

        await waitFor(() => {
          expect(screen.getByText("New Session")).toBeInTheDocument();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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
