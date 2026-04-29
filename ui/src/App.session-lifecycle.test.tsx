// App.session-lifecycle.test.tsx
//
// Owns: integration tests for the session-lifecycle layer of
// App — session creation (Codex / Claude agent-specific paths,
// remote-project routing, interactive-shell warnings for
// Gemini), live model-refresh behaviour and the Codex
// reasoning-effort-reset notice, unknown-model send
// confirmation (UI path + pure resolveUnknownSessionModelSendAttempt
// flow), backend-unavailable create-session recovery, the
// active-prompt-poll arming for stale send responses, and the
// hydration wake-gap resync for newly created sessions that
// haven't yet seen SSE traffic.
//
// Does not own: reconnect / watchdog live-state tests (see
// App.live-state.*.test.tsx), workspace-layout tests (see
// App.workspace-layout.test.tsx), orchestrator tests, control
// panel tests, or scroll / layout clamp tests.
//
// Split out of: ui/src/App.test.tsx (Slice 5 of the App-split
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

describe("App session lifecycle", () => {
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

  it("arms the active-prompt poll and adopts a replacement-instance recovery state when a successful send response is stale", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalFetch = globalThis.fetch;
      const project = {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      };
      const session = makeSession("session-1", {
        name: "Session 1",
        projectId: project.id,
        workdir: project.rootPath,
      });
      const initialState = makeStateResponse({
        revision: 1,
        serverInstanceId: "current-instance",
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [session],
      });
      const pollState = makeStateResponse({
        revision: 3,
        serverInstanceId: "replacement-instance",
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "idle",
            preview: "Recovered assistant response",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Recover this prompt",
              },
              {
                id: "message-2",
                timestamp: "2026-04-19T10:00:01Z",
                author: "assistant",
                type: "text",
                text: "Recovered assistant response",
              },
            ],
          }),
        ],
      });
      const sendMessageDeferred = createDeferred<Response>();
      let stateResponse = initialState;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(stateResponse);
          }

          if (
            requestUrl.pathname === "/api/sessions/session-1/messages" &&
            (init?.method ?? "GET").toUpperCase() === "POST"
          ) {
            return sendMessageDeferred.promise;
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchOpenedStateEvent(eventSource, initialState);
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
        const composer = await screen.findByLabelText("Message Session 1");
        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Recover this prompt" },
          });
        });
        await settleAsyncUi();
        vi.useFakeTimers();
        await clickAndSettle(screen.getByRole("button", { name: "Send" }));

        expect(
          fetchMock.mock.calls.some(([input, init]) => {
            const requestUrl = new URL(String(input), "http://localhost");
            return (
              requestUrl.pathname === "/api/sessions/session-1/messages" &&
              (init?.method ?? "GET").toUpperCase() === "POST" &&
              init?.body ===
                JSON.stringify({
                  text: "Recover this prompt",
                  attachments: [],
                  expandedText: null,
                })
            );
          }),
        ).toBe(true);

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 2,
            serverInstanceId: "current-instance",
            projects: [project],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-1", {
                name: "Session 1",
                projectId: project.id,
                workdir: project.rootPath,
                status: "active",
                preview: "Recover this prompt",
                messages: [
                  {
                    id: "message-1",
                    timestamp: "2026-04-19T10:00:00Z",
                    author: "you",
                    type: "text",
                    text: "Recover this prompt",
                  },
                ],
              }),
            ],
          }),
        );
        expect(
          screen.getAllByText("Recover this prompt").length,
        ).toBeGreaterThan(0);

        stateResponse = pollState;
        fetchMock.mockClear();

        await act(async () => {
          sendMessageDeferred.resolve(
            jsonResponse(
              makeStateResponse({
                revision: 1,
                serverInstanceId: "current-instance",
                projects: [project],
                orchestrators: [],
                workspaces: [],
                sessions: [session],
              }),
            ),
          );
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(composer).toHaveValue("");
        expect(fetchMock).not.toHaveBeenCalled();

        await advanceTimers(ACTIVE_PROMPT_POLL_INTERVAL_MS);
        await settleAsyncUi();

        expect(fetchMock).toHaveBeenCalledTimes(1);
        expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/state");
        expect(
          screen.getAllByText("Recovered assistant response").length,
        ).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("immediately probes /api/state when a send response carries an unseen serverInstanceId", async () => {
    // The user restarts the backend while keeping the browser tab open, then
    // sends a prompt. The POST returns a response with the NEW server's
    // `serverInstanceId`. `adoptState` rejects it (an unseen mismatched
    // instance is conservatively dropped, not silently accepted), and
    // without an immediate `/api/state` probe the only recovery path is
    // the 30 s `startStaleSendResponseRecoveryPoll` interval — during which
    // the sidebar tooltip (`session.preview`) stays on the PREVIOUS prompt
    // and the new prompt's metadata is invisible. The fix fires
    // `requestActionRecoveryResync({ allowUnknownServerInstance: true })`
    // immediately when the rejected response was from a different instance,
    // so adoption flips within a single round trip. See bugs.md
    // "Send-after-restart leaves session preview tooltip stale for 30 s".
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalFetch = globalThis.fetch;
      const project = {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      };
      const session = makeSession("session-1", {
        name: "Session 1",
        projectId: project.id,
        workdir: project.rootPath,
      });
      const initialState = makeStateResponse({
        revision: 5,
        serverInstanceId: "current-instance",
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [session],
      });
      // After the restart, /api/state observed by `requestActionRecoveryResync`
      // also returns the new instance — the POST response is corroborated.
      const recoveredState = makeStateResponse({
        revision: 1,
        serverInstanceId: "replacement-instance",
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "active",
            preview: "Restart-spanning prompt",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Restart-spanning prompt",
              },
            ],
          }),
        ],
      });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(recoveredState);
          }
          if (
            requestUrl.pathname === "/api/sessions/session-1/messages" &&
            (init?.method ?? "GET").toUpperCase() === "POST"
          ) {
            // The POST goes to the new server, which returns its fresh
            // (lower-revision) state with the new instance id.
            return jsonResponse(recoveredState);
          }

          throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchOpenedStateEvent(eventSource, initialState);
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
        const composer = await screen.findByLabelText("Message Session 1");
        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Restart-spanning prompt" },
          });
        });
        await settleAsyncUi();

        fetchMock.mockClear();
        await clickAndSettle(screen.getByRole("button", { name: "Send" }));
        await settleAsyncUi();

        // Two fetches expected: the POST itself, AND an immediate /api/state
        // probe driven by the unseen-instance recovery path. Without the
        // fix, only the POST would be made within the test window — the
        // /api/state probe wouldn't fire for 30 s.
        const stateProbeCalls = fetchMock.mock.calls.filter(
          ([input]) =>
            new URL(String(input), "http://localhost").pathname ===
            "/api/state",
        );
        expect(stateProbeCalls.length).toBeGreaterThanOrEqual(1);
        // The recovered state's new prompt is visible.
        await waitFor(() => {
          expect(
            screen.getAllByText("Restart-spanning prompt").length,
          ).toBeGreaterThan(0);
        });
        // The EventSource is recreated as part of the same recovery
        // flow so future streaming chunks (the assistant's reply) have
        // a live connection to the new backend — without this, the
        // user would see their prompt land but the assistant response
        // would silently fail to stream until they hard-refreshed.
        // `forceSseReconnect` set the pending-flag and `adoptState`'s
        // success path with `fullStateServerInstanceChanged` consumed
        // it, so a fresh `EventSourceMock` instance was constructed.
        expect(EventSourceMock.instances.length).toBeGreaterThanOrEqual(2);
        const newestEventSource =
          EventSourceMock.instances[EventSourceMock.instances.length - 1];
        expect(newestEventSource).not.toBe(eventSource);

        // Live-stream proof: dispatch a post-restart assistant delta on
        // the recreated EventSource and assert it lands in the active
        // transcript. Without this, the regression would still pass on
        // a future change that recreates the EventSource correctly but
        // leaves it stuck (e.g., listeners not re-attached, force-adopt
        // arming dropped on `onopen`, delta application broken on the
        // new transport). See bugs.md "Send-after-restart live-stream
        // recovery is not proven by the regression".
        await act(async () => {
          newestEventSource.dispatchOpen();
          newestEventSource.dispatchNamedEvent("state", {
            revision: 2,
            serverInstanceId: "replacement-instance",
            projects: [project],
            sessions: [
              makeSession("session-1", {
                name: "Session 1",
                projectId: project.id,
                workdir: project.rootPath,
                status: "active",
                preview: "Restart-spanning prompt",
                messageCount: 1,
                sessionMutationStamp: 1,
                messages: [
                  {
                    id: "message-1",
                    timestamp: "2026-04-19T10:00:00Z",
                    author: "you",
                    type: "text",
                    text: "Restart-spanning prompt",
                  },
                ],
              }),
            ],
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        await act(async () => {
          newestEventSource.dispatchNamedEvent("delta", {
            type: "messageCreated",
            revision: 3,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 1,
            messageCount: 2,
            message: {
              id: "message-assistant-1",
              timestamp: "2026-04-19T10:00:01Z",
              author: "assistant",
              type: "text",
              text: "Recovered through the new EventSource.",
            },
            preview: "Recovered through the new EventSource.",
            status: "active",
            sessionMutationStamp: 2,
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        // The assistant reply streamed through the new transport must
        // appear in the visible transcript. This is the user-facing
        // contract: the response renders without Ctrl+Shift+R.
        await waitFor(() => {
          expect(
            screen.getAllByText("Recovered through the new EventSource.")
              .length,
          ).toBeGreaterThan(0);
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("cancels the active-prompt poll once live updates resume for that session", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalFetch = globalThis.fetch;
      const project = {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      };
      const session = makeSession("session-1", {
        name: "Session 1",
        projectId: project.id,
        workdir: project.rootPath,
      });
      const initialState = makeStateResponse({
        revision: 1,
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [session],
      });
      const resumedLiveState = makeStateResponse({
        revision: 3,
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "active",
            preview: "Recovered via live event",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Recover this prompt",
              },
              {
                id: "message-2",
                timestamp: "2026-04-19T10:00:01Z",
                author: "assistant",
                type: "text",
                text: "Recovered via live event",
              },
            ],
          }),
        ],
      });
      const pollState = makeStateResponse({
        revision: 4,
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "idle",
            preview: "Poll should not fire",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Recover this prompt",
              },
              {
                id: "message-2",
                timestamp: "2026-04-19T10:00:01Z",
                author: "assistant",
                type: "text",
                text: "Poll should not fire",
              },
            ],
          }),
        ],
      });
      const sendMessageDeferred = createDeferred<Response>();
      let stateResponse = pollState;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(stateResponse);
          }

          if (
            requestUrl.pathname === "/api/sessions/session-1/messages" &&
            (init?.method ?? "GET").toUpperCase() === "POST"
          ) {
            return sendMessageDeferred.promise;
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchOpenedStateEvent(eventSource, initialState);
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
        const composer = await screen.findByLabelText("Message Session 1");
        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Recover this prompt" },
          });
        });
        await settleAsyncUi();
        vi.useFakeTimers();
        await clickAndSettle(screen.getByRole("button", { name: "Send" }));

        await dispatchStateEvent(
          eventSource,
          makeStateResponse({
            revision: 2,
            projects: [project],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-1", {
                name: "Session 1",
                projectId: project.id,
                workdir: project.rootPath,
                status: "active",
                preview: "Recover this prompt",
                messages: [
                  {
                    id: "message-1",
                    timestamp: "2026-04-19T10:00:00Z",
                    author: "you",
                    type: "text",
                    text: "Recover this prompt",
                  },
                ],
              }),
            ],
          }),
        );

        await act(async () => {
          sendMessageDeferred.resolve(jsonResponse(initialState));
          await flushUiWork();
        });
        await settleAsyncUi();

        fetchMock.mockClear();
        await dispatchStateEvent(eventSource, resumedLiveState);
        await settleAsyncUi();

        await advanceTimers(ACTIVE_PROMPT_POLL_INTERVAL_MS);
        await settleAsyncUi();

        expect(fetchMock).not.toHaveBeenCalled();
        expect(
          screen.getAllByText("Recovered via live event").length,
        ).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("arms the active-prompt poll when an adopted send response is still active", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalFetch = globalThis.fetch;
      const project = {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      };
      const session = makeSession("session-1", {
        name: "Session 1",
        projectId: project.id,
        workdir: project.rootPath,
      });
      const sendResponseState = makeStateResponse({
        revision: 2,
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "active",
            preview: "Keep working",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Keep working",
              },
            ],
          }),
        ],
      });
      const pollState = makeStateResponse({
        revision: 3,
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "idle",
            preview: "Recovered after adopted response",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Keep working",
              },
              {
                id: "message-2",
                timestamp: "2026-04-19T10:00:01Z",
                author: "assistant",
                type: "text",
                text: "Recovered after adopted response",
              },
            ],
          }),
        ],
      });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(pollState);
          }

          if (
            requestUrl.pathname === "/api/sessions/session-1/messages" &&
            (init?.method ?? "GET").toUpperCase() === "POST"
          ) {
            return jsonResponse(sendResponseState);
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [project],
            orchestrators: [],
            workspaces: [],
            sessions: [session],
          }),
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
        const composer = await screen.findByLabelText("Message Session 1");
        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Keep working" },
          });
        });
        await settleAsyncUi();
        vi.useFakeTimers();
        fetchMock.mockClear();
        await clickAndSettle(screen.getByRole("button", { name: "Send" }));
        await settleAsyncUi();

        expect(composer).toHaveValue("");
        expect(
          fetchMock.mock.calls.filter(([input]) => {
            const requestUrl = new URL(String(input), "http://localhost");
            return requestUrl.pathname === "/api/state";
          }),
        ).toHaveLength(0);
        fetchMock.mockClear();

        await advanceTimers(ACTIVE_PROMPT_POLL_INTERVAL_MS);
        await settleAsyncUi();

        expect(fetchMock).toHaveBeenCalledTimes(1);
        expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/state");
        expect(
          screen.getAllByText("Recovered after adopted response").length,
        ).toBeGreaterThan(0);
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("does not arm the active-prompt poll when an adopted send response is idle", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalFetch = globalThis.fetch;
      const project = {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      };
      const session = makeSession("session-1", {
        name: "Session 1",
        projectId: project.id,
        workdir: project.rootPath,
      });
      const sendResponseState = makeStateResponse({
        revision: 2,
        projects: [project],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: project.id,
            workdir: project.rootPath,
            status: "idle",
            preview: "Finished from send response",
            messages: [
              {
                id: "message-1",
                timestamp: "2026-04-19T10:00:00Z",
                author: "you",
                type: "text",
                text: "Finish immediately",
              },
              {
                id: "message-2",
                timestamp: "2026-04-19T10:00:01Z",
                author: "assistant",
                type: "text",
                text: "Finished from send response",
              },
            ],
          }),
        ],
      });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(
              makeStateResponse({
                revision: 3,
                projects: [project],
                orchestrators: [],
                workspaces: [],
                sessions: [
                  makeSession("session-1", {
                    name: "Session 1",
                    projectId: project.id,
                    workdir: project.rootPath,
                    status: "idle",
                    preview: "Poll should not fire",
                  }),
                ],
              }),
            );
          }

          if (
            requestUrl.pathname === "/api/sessions/session-1/messages" &&
            (init?.method ?? "GET").toUpperCase() === "POST"
          ) {
            return jsonResponse(sendResponseState);
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 1,
            projects: [project],
            orchestrators: [],
            workspaces: [],
            sessions: [session],
          }),
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
        const composer = await screen.findByLabelText("Message Session 1");
        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Finish immediately" },
          });
        });
        await settleAsyncUi();
        vi.useFakeTimers();
        fetchMock.mockClear();
        await clickAndSettle(screen.getByRole("button", { name: "Send" }));
        await settleAsyncUi();

        expect(composer).toHaveValue("");
        expect(
          screen.getAllByText("Finished from send response").length,
        ).toBeGreaterThan(0);
        expect(
          fetchMock.mock.calls.filter(([input]) => {
            const requestUrl = new URL(String(input), "http://localhost");
            return requestUrl.pathname === "/api/state";
          }),
        ).toHaveLength(0);
        fetchMock.mockClear();

        await advanceTimers(ACTIVE_PROMPT_POLL_INTERVAL_MS);
        await settleAsyncUi();

        expect(fetchMock).not.toHaveBeenCalled();
        expect(screen.queryByText("Poll should not fire")).toBeNull();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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

  it("keeps the Create Session assistant pick after changing away from the active session agent", async () => {
    await withSuppressedActWarnings(async () => {
      const setup = await renderAppWithProjectAndSession();
      try {
        await openCreateSessionDialog();

        expect(
          await screen.findByRole("combobox", { name: "Assistant" }),
        ).toHaveTextContent("Codex");

        await selectComboboxOption("Assistant", "Claude");
        await waitFor(() => {
          expect(
            screen.getByRole("combobox", { name: "Assistant" }),
          ).toHaveTextContent("Claude");
        });
        await settleAsyncUi();
        expect(
          screen.getByRole("combobox", { name: "Assistant" }),
        ).toHaveTextContent("Claude");
      } finally {
        setup.cleanup();
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
          refreshSessionModelOptionsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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
          refreshSessionModelOptionsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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
          fetchStateDeferred.resolve(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
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
          refreshSessionModelOptionsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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
          fetchStateDeferred.resolve(
            makeStateResponse({
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
            }),
          );
          await flushUiWork();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(screen.getByRole("tab", { name: "Codex" }));
        await selectComboboxOption("Default reasoning effort", /high/i);
        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultCodexReasoningEffort: "high",
          });
        });
        await act(async () => {
          updateSettingsDeferred.resolve(
            makeStateResponse({
              revision: 2,
              preferences: {
                defaultCodexReasoningEffort: "high",
                defaultClaudeEffort: "default",
              },
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
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
          refreshSessionModelOptionsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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
          fetchStateDeferred.resolve(
            makeStateResponse({
              revision: 1,
              preferences: {
                defaultCodexReasoningEffort: "medium",
                defaultClaudeEffort: "default",
              },
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(screen.getByRole("tab", { name: "Claude" }));
        await selectComboboxOption("Default Claude effort", /max/i);
        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultClaudeEffort: "max",
          });
        });
        await act(async () => {
          updateSettingsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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
          refreshSessionModelOptionsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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
          refreshSessionModelOptionsDeferred.resolve(
            makeStateResponse({
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
            }),
          );
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

  it("does not open a phantom pane for a stale create response whose session is still absent", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 3,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-phantom",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-phantom", {
              name: "Phantom Session",
            }),
          });
          await flushUiWork();
        });

        expect(screen.queryByText("Phantom Session")).not.toBeInTheDocument();
        expect(
          screen.queryByLabelText("Message Phantom Session"),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        expect(screen.queryByText("Phantom Session")).not.toBeInTheDocument();
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

  it("recovers instead of directly adopting an unknown cross-instance create response", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 5,
              serverInstanceId: "current-instance",
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-cross-instance",
            revision: 4,
            serverInstanceId: "unknown-old-instance",
            session: makeSession("session-cross-instance", {
              name: "Cross Instance Session",
            }),
          });
          await flushUiWork();
        });

        expect(
          screen.queryByText("Cross Instance Session"),
        ).not.toBeInTheDocument();
        expect(
          screen.queryByLabelText("Message Cross Instance Session"),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 6,
              serverInstanceId: "replacement-instance",
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-cross-instance", {
                  name: "Cross Instance Session",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(
          await screen.findByLabelText("Message Cross Instance Session"),
        ).toBeInTheDocument();
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

  it("opens the created session after action-recovery resync adopts a stale create response", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 3,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-recovered",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-recovered", {
              name: "Recovered Session",
            }),
          });
          await flushUiWork();
        });

        expect(screen.queryByText("Recovered Session")).not.toBeInTheDocument();
        expect(
          screen.queryByLabelText("Message Recovered Session"),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-recovered", {
                  name: "Recovered Session",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(
          await screen.findByLabelText("Message Recovered Session"),
        ).toBeInTheDocument();
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

  it("opens the created session only after a later state event when the first recovery snapshot still omits it", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const createSessionDeferred =
        createDeferred<Awaited<ReturnType<typeof api.createSession>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const createSessionSpy = vi
        .spyOn(api, "createSession")
        .mockImplementation(() => createSessionDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await openCreateSessionDialog();
        await settleAsyncUi();
        await submitButtonAndSettle(
          screen.getByRole("button", { name: "Create session" }),
        );

        await waitFor(() => {
          expect(createSessionSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 3,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        await act(async () => {
          createSessionDeferred.resolve({
            sessionId: "session-recovered-late",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-recovered-late", {
              name: "Recovered Later",
            }),
          });
          await flushUiWork();
        });

        expect(screen.queryByText("Recovered Later")).not.toBeInTheDocument();
        expect(
          screen.queryByLabelText("Message Recovered Later"),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
          await flushUiWork();
        });

        expect(screen.queryByText("Recovered Later")).not.toBeInTheDocument();
        expect(
          screen.queryByLabelText("Message Recovered Later"),
        ).not.toBeInTheDocument();

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 5,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-recovered-late", {
                  name: "Recovered Later",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(
          await screen.findByLabelText("Message Recovered Later"),
        ).toBeInTheDocument();
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

  it("does not open a phantom pane for a stale fork response whose session is still absent", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const forkCodexThreadDeferred =
        createDeferred<Awaited<ReturnType<typeof api.forkCodexThread>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const forkCodexThreadSpy = vi
        .spyOn(api, "forkCodexThread")
        .mockImplementation(() => forkCodexThreadDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowLabel =
          await within(sessionList).findByText("Codex Session");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!(sessionRowButton instanceof HTMLButtonElement)) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);

        await clickAndSettle(
          await screen.findByRole("button", { name: "Prompt" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Fork thread" }),
        );

        await waitFor(() => {
          expect(forkCodexThreadSpy).toHaveBeenCalledWith("session-1");
        });

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 3,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        await act(async () => {
          forkCodexThreadDeferred.resolve({
            sessionId: "session-fork-phantom",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-fork-phantom", {
              agent: "Codex",
              name: "Phantom Fork",
              externalSessionId: "thread-fork",
              codexThreadState: "active",
            }),
          });
          await flushUiWork();
        });

        expect(screen.queryByText("Phantom Fork")).not.toBeInTheDocument();
        expect(
          screen.queryByRole("tab", { name: "Phantom Fork" }),
        ).not.toBeInTheDocument();
        expect(
          screen.queryByText(
            "Forked the live Codex thread into a new session.",
          ),
        ).not.toBeInTheDocument();
        expect(
          screen.queryByText(/attached to a forked Codex thread/i),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(screen.queryByText("Phantom Fork")).not.toBeInTheDocument();
        expect(
          screen.queryByText(
            "Forked the live Codex thread into a new session.",
          ),
        ).not.toBeInTheDocument();
        expect(
          screen.queryByText(/attached to a forked Codex thread/i),
        ).not.toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        forkCodexThreadSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows fork notice when the fork response materializes the new session", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const forkCodexThreadDeferred =
        createDeferred<Awaited<ReturnType<typeof api.forkCodexThread>>>();
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(
        makeStateResponse({
          revision: 1,
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [
            makeSession("session-1", {
              agent: "Codex",
              name: "Codex Session",
              externalSessionId: "thread-live",
              codexThreadState: "active",
            }),
          ],
        }),
      );
      const forkCodexThreadSpy = vi
        .spyOn(api, "forkCodexThread")
        .mockImplementation(() => forkCodexThreadDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowLabel =
          await within(sessionList).findByText("Codex Session");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!(sessionRowButton instanceof HTMLButtonElement)) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);

        await clickAndSettle(
          await screen.findByRole("button", { name: "Prompt" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Fork thread" }),
        );

        await waitFor(() => {
          expect(forkCodexThreadSpy).toHaveBeenCalledWith("session-1");
        });

        await act(async () => {
          forkCodexThreadDeferred.resolve({
            sessionId: "session-fork-direct",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-fork-direct", {
              agent: "Codex",
              name: "Direct Fork",
              externalSessionId: "thread-fork",
              codexThreadState: "active",
            }),
          });
          await flushUiWork();
        });

        const directForkTab = await screen.findByRole("tab", {
          name: "Direct Fork",
        });
        await clickAndSettle(directForkTab);
        expect(
          await screen.findByText(/attached to a forked Codex thread/i),
        ).toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        forkCodexThreadSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("opens the forked session after action-recovery resync adopts a stale fork response", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const forkCodexThreadDeferred =
        createDeferred<Awaited<ReturnType<typeof api.forkCodexThread>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const forkCodexThreadSpy = vi
        .spyOn(api, "forkCodexThread")
        .mockImplementation(() => forkCodexThreadDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowLabel =
          await within(sessionList).findByText("Codex Session");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!(sessionRowButton instanceof HTMLButtonElement)) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);

        await clickAndSettle(
          await screen.findByRole("button", { name: "Prompt" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Fork thread" }),
        );

        await waitFor(() => {
          expect(forkCodexThreadSpy).toHaveBeenCalledWith("session-1");
        });

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 3,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        await act(async () => {
          forkCodexThreadDeferred.resolve({
            sessionId: "session-fork-recovered",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-fork-recovered", {
              agent: "Codex",
              name: "Recovered Fork",
              externalSessionId: "thread-fork",
              codexThreadState: "active",
            }),
          });
          await flushUiWork();
        });

        expect(screen.queryByText("Recovered Fork")).not.toBeInTheDocument();
        expect(
          screen.queryByRole("tab", { name: "Recovered Fork" }),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
                makeSession("session-fork-recovered", {
                  agent: "Codex",
                  name: "Recovered Fork",
                  externalSessionId: "thread-fork",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        const recoveredForkTab = await screen.findByRole("tab", {
          name: "Recovered Fork",
        });
        expect(recoveredForkTab).toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        forkCodexThreadSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("opens the forked session only after a later state event when the first recovery snapshot still omits it", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const forkCodexThreadDeferred =
        createDeferred<Awaited<ReturnType<typeof api.forkCodexThread>>>();
      const actionRecoveryDeferred =
        createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => actionRecoveryDeferred.promise);
      const forkCodexThreadSpy = vi
        .spyOn(api, "forkCodexThread")
        .mockImplementation(() => forkCodexThreadDeferred.promise);
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

        await act(async () => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowLabel =
          await within(sessionList).findByText("Codex Session");
        const sessionRowButton = sessionRowLabel.closest("button");
        if (!(sessionRowButton instanceof HTMLButtonElement)) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);

        await clickAndSettle(
          await screen.findByRole("button", { name: "Prompt" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Fork thread" }),
        );

        await waitFor(() => {
          expect(forkCodexThreadSpy).toHaveBeenCalledWith("session-1");
        });

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 3,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        await act(async () => {
          forkCodexThreadDeferred.resolve({
            sessionId: "session-fork-recovered-late",
            revision: 2,
            serverInstanceId: "test-instance",
            session: makeSession("session-fork-recovered-late", {
              agent: "Codex",
              name: "Recovered Fork Later",
              externalSessionId: "thread-fork",
              codexThreadState: "active",
            }),
          });
          await flushUiWork();
        });

        expect(
          screen.queryByText("Recovered Fork Later"),
        ).not.toBeInTheDocument();
        expect(
          screen.queryByRole("tab", { name: "Recovered Fork Later" }),
        ).not.toBeInTheDocument();

        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          actionRecoveryDeferred.resolve(
            makeStateResponse({
              revision: 4,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(
          screen.queryByText("Recovered Fork Later"),
        ).not.toBeInTheDocument();
        expect(
          screen.queryByRole("tab", { name: "Recovered Fork Later" }),
        ).not.toBeInTheDocument();

        await act(async () => {
          eventSource.dispatchNamedEvent(
            "state",
            makeStateResponse({
              revision: 5,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  agent: "Codex",
                  name: "Codex Session",
                  externalSessionId: "thread-live",
                  codexThreadState: "active",
                }),
                makeSession("session-fork-recovered-late", {
                  agent: "Codex",
                  name: "Recovered Fork Later",
                  externalSessionId: "thread-fork",
                  codexThreadState: "active",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(
          await screen.findByRole("tab", { name: "Recovered Fork Later" }),
        ).toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        forkCodexThreadSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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
