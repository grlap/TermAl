// App.live-state.hydration-recovery.test.tsx
//
// Owns: App live-state tests for hydration responses that must trigger state
// resync, reject stale server-instance data, or rehydrate metadata-only panes.
//
// Does not own:
//   - reconnect-focused tests, which live in
//     App.live-state.reconnect.test.tsx
//   - visibility / wake-gap recovery tests, which live in
//     App.live-state.visibility.test.tsx
//   - queued-follow-up and watchdog cooldown tests, which
//     live in App.live-state.watchdog.test.tsx
//   - transcript-preservation hydration tests, which live in
//     App.live-state.hydration.test.tsx
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
});
