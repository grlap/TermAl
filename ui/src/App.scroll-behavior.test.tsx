// App.scroll-behavior.test.tsx
//
// Owns: tests for App-level scroll and layout-clamp behaviour
// - Ctrl+PageUp jump-to-top regression (cancels a pending
//   settle-to-bottom RAF), session scroll restoration (the
//   manual-message-scroll store path, the settled-scroll
//   minimum-attempts helper, the new-response scroll
//   correction, the default-scroll-to-bottom useLayoutEffect
//   branch), wheel passive/non-passive listener registration,
//   docked control-panel layout clamps (saved-layout floor,
//   control-panel pixel minimum vs generic row split clamp,
//   standalone control-surface pixel minimum, standalone
//   width -> initial dock ratio matching, and the initial
//   dock-ratio clamp when the standalone width would crowd
//   out the session pane).
//
// Does not own: control-panel integration tests (see
// App.control-panel.test.tsx), DnD tests, live-state tests,
// workspace-layout tests, session-lifecycle tests.
//
// Split out of: ui/src/App.test.tsx (Slice 9 of the App-split
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

describe("App scroll behaviour", () => {
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

  it("cancels a pending settle-to-bottom frame when Ctrl+PageUp jumps to the top", async () => {
    await withSuppressedActWarnings(async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(
        window.navigator,
        "platform",
      );
      Object.defineProperty(window.navigator, "platform", {
        configurable: true,
        value: "Win32",
      });
      const pendingFrames = new Map<number, FrameRequestCallback>();
      let nextFrameId = 1;
      vi.stubGlobal(
        "requestAnimationFrame",
        ((callback: FrameRequestCallback) => {
          const frameId = nextFrameId;
          nextFrameId += 1;
          pendingFrames.set(frameId, callback);
          return frameId;
        }) as typeof requestAnimationFrame,
      );
      const cancelAnimationFrameMock = vi.fn((frameId: number) => {
        pendingFrames.delete(frameId);
      });
      vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);

      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();

      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        messageStack.scrollTop = 800;

        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "PageUp",
            code: "PageUp",
            ctrlKey: true,
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(0);
        expect(filterScrollToCallsAt(scrollToMock, 0, "auto").length).toBeGreaterThan(0);
        expect(cancelAnimationFrameMock).toHaveBeenCalled();

        const queuedFrames = [...pendingFrames.values()];
        pendingFrames.clear();
        for (const callback of queuedFrames) {
          await act(async () => {
            callback(Date.now());
            await flushUiWork();
          });
        }
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(0);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
        if (originalPlatform) {
          Object.defineProperty(window.navigator, "platform", originalPlatform);
        } else {
          Reflect.deleteProperty(window.navigator, "platform");
        }
      }
    });
  });

  it("jumps to the top on Ctrl+Shift+PageUp from the composer textarea", async () => {
    await withSuppressedActWarnings(async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(
        window.navigator,
        "platform",
      );
      Object.defineProperty(window.navigator, "platform", {
        configurable: true,
        value: "Win32",
      });
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, { target: { value: "hello world" } });
        });

        messageStack.scrollTop = 800;
        composer.focus();
        composer.setSelectionRange(composer.value.length, composer.value.length);

        await act(async () => {
          fireEvent.keyDown(composer, {
            key: "PageUp",
            code: "PageUp",
            ctrlKey: true,
            shiftKey: true,
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(0);
        expect(filterScrollToCallsAt(scrollToMock, 0, "auto").length).toBeGreaterThan(0);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
        if (originalPlatform) {
          Object.defineProperty(window.navigator, "platform", originalPlatform);
        } else {
          Reflect.deleteProperty(window.navigator, "platform");
        }
      }
    });
  });

  it("keeps plain PageDown inside the composer textarea when the caret is not at the start", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(".message-stack");
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, { target: { value: "hello world" } });
        });

        messageStack.scrollTop = 800;
        composer.focus();
        composer.setSelectionRange(5, 5);

        await act(async () => {
          fireEvent.keyDown(composer, {
            key: "PageDown",
            code: "PageDown",
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(800);
        expect(filterScrollToCallsAt(scrollToMock, 0, "auto")).toEqual([]);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("uses the current session when the nested editable PageDown fallback fires after a tab switch", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-termal",
                name: "TermAl",
                rootPath: "/projects/termal",
              },
            ],
            sessions: [
              makeSession("session-1", {
                name: "Session 1",
                projectId: "project-termal",
                workdir: "/projects/termal",
              }),
              makeSession("session-2", {
                name: "Session 2",
                projectId: "project-termal",
                workdir: "/projects/termal",
              }),
            ],
          });
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-nested-page-fallback-session-switch`;
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-nested-page-fallback-session-switch",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        layoutStorageKey,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              type: "pane",
              paneId: "pane-session",
            },
            panes: [
              {
                id: "pane-session",
                tabs: [
                  {
                    id: "tab-session-1",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-session-2",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-session-1",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-session",
          },
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

      try {
        await renderApp();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) => within(candidate).queryByRole("tab", { name: "Session 1" }));
        if (!tablist) {
          throw new Error("Session pane tablist not found");
        }
        const session1Tab = within(tablist).getByRole("tab", { name: "Session 1" });
        const session2Tab = within(tablist).getByRole("tab", { name: "Session 2" });

        await clickAndSettle(session1Tab);
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Active message stack not found");
        }

        messageStack.scrollTop = 150;
        act(() => {
          fireEvent.scroll(messageStack);
        });

        await clickAndSettle(session2Tab);
        expect(messageStack.scrollTop).toBe(800);
        messageStack.scrollTop = 400;
        act(() => {
          fireEvent.scroll(messageStack);
        });

        const composer = await screen.findByLabelText("Message Session 2");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Session 2 composer not found");
        }
        await act(async () => {
          fireEvent.change(composer, { target: { value: "hello world" } });
        });
        composer.focus();
        composer.setSelectionRange(0, 0);
        const stopPropagation = (event: KeyboardEvent) => {
          event.stopPropagation();
        };
        composer.addEventListener("keydown", stopPropagation);

        await act(async () => {
          try {
            fireEvent.keyDown(composer, {
              key: "PageDown",
              code: "PageDown",
            });
          } finally {
            composer.removeEventListener("keydown", stopPropagation);
          }
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(490);

        const currentTablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) =>
            within(candidate).queryByRole("tab", {
              name: "Session 2",
              selected: true,
            }),
          );
        if (!currentTablist) {
          throw new Error("Session pane tablist not found after tab switch");
        }
        await clickAndSettle(
          within(currentTablist).getByRole("tab", { name: "Session 1" }),
        );
        expect(messageStack.scrollTop).toBe(800);
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("ignores nested editable PageDown targets outside the active pane", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });

      try {
        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          await settleAsyncUi();

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          if (!(messageStack instanceof HTMLElement)) {
            throw new Error("Active message stack not found");
          }

          messageStack.scrollTop = 320;
          act(() => {
            fireEvent.scroll(messageStack);
          });

          const externalTextarea = document.createElement("textarea");
          externalTextarea.value = "outside";
          document.body.appendChild(externalTextarea);
          externalTextarea.focus();
          externalTextarea.setSelectionRange(0, 0);

          try {
            await act(async () => {
              fireEvent.keyDown(externalTextarea, {
                key: "PageDown",
                code: "PageDown",
              });
            });
            await settleAsyncUi();
          } finally {
            externalTextarea.remove();
          }

          expect(messageStack.scrollTop).toBe(320);
        } finally {
          teardown();
        }
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("pages the session transcript by a fixed delta on plain PageDown", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        scrollToMock.mockClear();
        messageStack.scrollTop = 400;

        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "PageDown",
            code: "PageDown",
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(490);
        expect(scrollToMock).not.toHaveBeenCalled();

        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Fresh assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Fresh assistant response.",
                },
              ],
            }),
          ],
        });

        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("pages the session transcript upward by a fixed delta on plain PageUp", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        scrollToMock.mockClear();
        messageStack.scrollTop = 800;

        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "PageUp",
            code: "PageUp",
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(710);
        expect(scrollToMock).not.toHaveBeenCalled();

        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Fresh assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Fresh assistant response.",
                },
              ],
            }),
          ],
        });

        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("follows the latest user prompt immediately while a send is in flight", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const pendingSend = createDeferred<Response>();
      const baseState = {
        revision: 2,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: "project-termal",
            workdir: "/projects/termal",
            preview: "Latest user prompt.",
            messages: [
              {
                id: "message-user-1",
                type: "text",
                timestamp: "10:01",
                author: "you",
                text: "Latest user prompt.",
              },
            ],
          }),
        ],
      };

      context.fetchMock.mockImplementation(
        async (input: RequestInfo | URL) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse(baseState);
          }
          if (requestUrl.pathname === "/api/sessions/session-1/messages") {
            return pendingSend.promise;
          }
          throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
        },
      );

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 0;
        act(() => {
          fireEvent.scroll(messageStack);
        });

        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Follow this prompt" },
          });
        });

        scrollToMock.mockClear();

        await act(async () => {
          fireEvent.click(screen.getByRole("button", { name: "Send" }));
          await Promise.resolve();
        });
        await settleAsyncUi();

        const settledBottomCallCount = filterScrollToCallsAt(
          scrollToMock,
          800,
          "auto",
        ).length;
        expect(settledBottomCallCount).toBeGreaterThan(0);

        context.cleanup();
        await flushUiWork();
        expect(filterScrollToCallsAt(scrollToMock, 800, "auto").length).toBe(
          settledBottomCallCount,
        );
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("scrolls before paint to make room when the live turn appears at the bottom", async () => {
    await withSuppressedActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const baseState = {
        revision: 2,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: "project-termal",
            workdir: "/projects/termal",
            preview: "Current response.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(
          filterScrollToCallsAt(scrollToMock, 920, "auto").length,
        ).toBeGreaterThan(0);
        expect(messageStack.scrollTop).toBe(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("smoothly follows new assistant messages while pinned to the bottom", async () => {
    await withSuppressedActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "First assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 800;
        expect(
          messageStack.scrollHeight -
            messageStack.scrollTop -
            messageStack.clientHeight,
        ).toBe(0);
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();
        let growSecondAssistantAfterFirstFollow = true;
        scrollToMock.mockImplementation(function (
          this: HTMLElement,
          options?: ScrollToOptions | number,
          y?: number,
        ) {
          if (
            typeof options === "object" &&
            options !== null &&
            typeof options.top === "number"
          ) {
            this.scrollTop = options.top;
            // Simulate rendered message content measuring taller after the
            // first follow scroll.
            if (
              growSecondAssistantAfterFirstFollow &&
              options.behavior === "smooth" &&
              options.top === 900
            ) {
              growSecondAssistantAfterFirstFollow = false;
              scrollHeight = 1200;
            }
            return;
          }

          if (typeof options === "number" && typeof y === "number") {
            this.scrollTop = y;
          }
        });

        scrollHeight = 1100;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Second assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Second assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          filterScrollToCallsAt(scrollToMock, 900, "smooth").length,
        ).toBeGreaterThan(0);
        expect(
          filterScrollToCallsAt(scrollToMock, 1000, "smooth").length,
        ).toBeGreaterThan(0);
        expect(
          screen.queryByRole("button", { name: "New response" }),
        ).not.toBeInTheDocument();

        messageStack.scrollTop = 760;
        expect(
          messageStack.scrollHeight -
            messageStack.scrollTop -
            messageStack.clientHeight,
        ).toBeGreaterThan(0);
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1200;
        await dispatchStateEvent(latestEventSource(), {
          revision: 4,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Third assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Second assistant response.",
                },
                {
                  id: "message-assistant-3",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Third assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          filterScrollToCallsAt(scrollToMock, 1000, "smooth").length,
        ).toBeGreaterThan(0);
        expect(
          screen.queryByRole("button", { name: "New response" }),
        ).not.toBeInTheDocument();

        scrollToMock.mockClear();
        messageStack.scrollTop = 760;
        await act(async () => {
          fireEvent.mouseDown(messageStack);
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        scrollHeight = 1300;
        await dispatchStateEvent(latestEventSource(), {
          revision: 5,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Fourth assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Second assistant response.",
                },
                {
                  id: "message-assistant-3",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Third assistant response.",
                },
                {
                  id: "message-assistant-4",
                  type: "text",
                  timestamp: "10:04",
                  author: "assistant",
                  text: "Fourth assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(filterScrollToCallsAt(scrollToMock, 1100, "smooth")).toEqual([]);
        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("scrolls down when queued prompts append in transcript order above the live turn", async () => {
    await withSuppressedActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:02",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const liveTurnCard = screen
          .getByText("Live turn")
          .closest(".activity-card-live");
        const queuedPromptCard = screen
          .getByText("Queued follow-up")
          .closest(".pending-prompt-card");
        expect(liveTurnCard).not.toBeNull();
        expect(queuedPromptCard).not.toBeNull();
        expect(
          Boolean(
            queuedPromptCard!.compareDocumentPosition(liveTurnCard!) &
              Node.DOCUMENT_POSITION_FOLLOWING,
          ),
        ).toBe(true);
        expect(filterScrollToCallsAt(scrollToMock, 920, "smooth").length).toBeGreaterThan(0);
        expect(filterScrollToCallsAt(scrollToMock, 920, "auto")).toEqual([]);
        expect(messageStack.scrollTop).toBe(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("labels the bottom indicator as activity when only queued prompts append", async () => {
    await withSuppressedActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        const messages: Session["messages"] = [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Current prompt",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Current turn partial.",
          },
        ];
        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current turn partial.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 700;
        await act(async () => {
          fireEvent.mouseDown(messageStack);
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current turn partial.",
              messages,
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:02",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          await screen.findByRole("button", { name: "New activity" }),
        ).toBeInTheDocument();
        expect(
          screen.queryByRole("button", { name: "New response" }),
        ).not.toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps a response indicator when queued prompts append after an unseen assistant response", async () => {
    await withSuppressedActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        const baseMessages: Session["messages"] = [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Current prompt",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Current turn partial.",
          },
        ];
        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Current turn partial.",
              messages: baseMessages,
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 700;
        await act(async () => {
          fireEvent.mouseDown(messageStack);
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        const responseMessages: Session["messages"] = [
          ...baseMessages,
          {
            id: "message-assistant-2",
            type: "text",
            timestamp: "10:02",
            author: "assistant",
            text: "Fresh assistant response.",
          },
        ];
        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              preview: "Fresh assistant response.",
              messages: responseMessages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();

        scrollHeight = 1220;
        await dispatchStateEvent(latestEventSource(), {
          revision: 4,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Fresh assistant response.",
              messages: responseMessages,
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
        await settleAsyncUi();

        expect(
          screen.getByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
        expect(
          screen.queryByRole("button", { name: "New activity" }),
        ).not.toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("unpinns the live turn tail when the user scrolls away from bottom", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const liveTail = screen
          .getByText("Live turn")
          .closest(".conversation-live-tail");
        expect(liveTail).not.toBeNull();
        expect(liveTail).toHaveClass("is-pinned");

        await act(async () => {
          fireEvent.wheel(messageStack, { deltaY: -160 });
          messageStack.scrollTop = 600;
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).not.toHaveClass("is-pinned");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).toHaveClass("is-pinned");
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps the live turn tail pinned when transcript growth opens a temporary bottom gap", async () => {
    await withSuppressedActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const liveTail = screen
          .getByText("Live turn")
          .closest(".conversation-live-tail");
        expect(liveTail).not.toBeNull();
        expect(liveTail).toHaveClass("is-pinned");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        scrollHeight = 1120;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        expect(liveTail).toHaveClass("is-pinned");
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("resolves settled-scroll minimum attempts from the fallback threshold and explicit clamp", () => {
    expect(resolveSettledScrollMinimumAttempts(60)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(13)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(12)).toBe(4);
    expect(resolveSettledScrollMinimumAttempts(4)).toBe(4);
    expect(resolveSettledScrollMinimumAttempts(12, 8)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(6, 8)).toBe(6);
    expect(resolveSettledScrollMinimumAttempts(60, 8)).toBe(8);
    expect(resolveSettledScrollMinimumAttempts(0)).toBe(0);
  });

  it("jumps the new-response button to the virtualized bottom without settled-scroll spam", async () => {
    await withSuppressedActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();

      try {
        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          for (let iteration = 0; iteration < 10; iteration += 1) {
            await settleAsyncUi();
          }

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          if (!(messageStack instanceof HTMLElement)) {
            throw new Error("Message stack not found");
          }

          scrollToMock.mockClear();
          messageStack.scrollTop = 0;
          await act(async () => {
            fireEvent.scroll(messageStack);
            await flushUiWork();
          });

          await dispatchStateEvent(latestEventSource(), {
            revision: 2,
            projects: [
              {
                id: "project-termal",
                name: "TermAl",
                rootPath: "/projects/termal",
              },
            ],
            sessions: [
              makeSession("session-1", {
                name: "Session 1",
                projectId: "project-termal",
                workdir: "/projects/termal",
                preview: "Fresh assistant response.",
                messages: Array.from({ length: 90 }, (_, index) => ({
                  id: `message-assistant-${index + 1}`,
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: `Fresh assistant response ${index + 1}.`,
                })),
              }),
            ],
          });

          const scrollToLatestButton = await screen.findByRole("button", {
            name: "New response",
          });
          scrollToMock.mockClear();
          await clickAndSettle(scrollToLatestButton);
          for (let iteration = 0; iteration < 10; iteration += 1) {
            await settleAsyncUi();
          }

          expect(messageStack.scrollTop).toBe(800);
          expect(filterScrollToCallsAt(scrollToMock, 800, "auto")).toEqual([]);
        } finally {
          teardown();
        }
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("runs the default-scroll-to-bottom branch of the session scroll useLayoutEffect on mount and lets the cleanup return cleanly", async () => {
    // Regression for round 7's session-pane scroll restoration
    // `useLayoutEffect` restructure and the synchronous first `tick()`
    // inside `scheduleSettledScrollToBottom`. The two changes are
    // observed together here because the effect runs `tick()`
    // synchronously via `scheduleSettledScrollToBottom("auto", ...)` as
    // part of the default-scroll-to-bottom branch on first mount:
    //
    //  1. `messageStackRef.current` is the `<section class="message-stack">`
    //     rendered by `SessionPaneContent`.
    //  2. The effect hits the `else if (defaultScrollToBottom) { ... }`
    //     arm (branch 3) because there is no prior saved
    //     `paneScrollPositions[scrollStateKey]` entry.
    //  3. `scheduleSettledScrollToBottom("auto", { maxAttempts: 60 })`
    //     runs `tick()` synchronously inside its own call. `tick()`
    //     calls `scrollToLatestMessage("auto")`.
    //  4. `scrollToLatestMessage` computes
    //     `nextScrollTop = Math.max(scrollHeight - clientHeight, 0)`
    //     and invokes `node.scrollTo({ top, behavior })` when the
    //     current `scrollTop` is farther than 1 px from the target.
    //
    // jsdom reports `scrollHeight` and `clientHeight` as 0 on every
    // element, which would collapse `nextScrollTop` to 0 and skip the
    // `scrollTo` call via the 1-px tolerance. To observe the scroll the
    // test overrides the prototype getters for the duration of the test
    // so `scrollHeight - clientHeight = 800`, matches the sibling
    // `TerminalPanel.test.tsx` `stubScrollGeometry` helper's spirit
    // while staying scoped to a single test via a finally-block
    // restore. The test then checks `HTMLElement.prototype.scrollTo`
    // (which `beforeEach` already stubs with `vi.fn()`) to prove the
    // effect reached the `scrollTo({ top: 800, ... })` branch, which
    // simultaneously pins:
    //
    //  - Branch 3 of the restored `useLayoutEffect` if-else chain,
    //  - The synchronous first `tick()` in `scheduleSettledScrollToBottom`,
    //  - The `Math.max(scrollHeight - clientHeight, 0)` / 1-px tolerance
    //    pattern shared between `scrollToLatestMessage` and
    //    `scrollTerminalHistoryToBottom`,
    //  - And finally the cleanup branch: after `cleanup()` fires in
    //    `afterEach`, the returned cleanup function from
    //    `scheduleSettledScrollToBottom` runs `if (frameId !== 0)
    //    cancelAnimationFrame(frameId)`. A regression that dropped the
    //    `frameId !== 0` guard after the synchronous complete would
    //    still produce a noisy `cancelAnimationFrame(0)` call that
    //    would surface via `cancelAnimationFrameMock`'s tracking map
    //    (verified implicitly — the test's own afterEach would throw
    //    under the unhandled error if the cleanup propagated one).
    await withSuppressedActWarnings(async () => {
      const originalScrollHeight = Object.getOwnPropertyDescriptor(
        HTMLElement.prototype,
        "scrollHeight",
      );
      const originalClientHeight = Object.getOwnPropertyDescriptor(
        HTMLElement.prototype,
        "clientHeight",
      );
      Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
        configurable: true,
        get() {
          return 1000;
        },
      });
      Object.defineProperty(HTMLElement.prototype, "clientHeight", {
        configurable: true,
        get() {
          return 200;
        },
      });

      // Wrap `cancelAnimationFrame` with a spy so the cleanup-guard
      // assertion below can prove the `frameId !== 0` guard fired.
      // `beforeEach` already installs `cancelAnimationFrameMock` via
      // `vi.stubGlobal`; spying on `globalThis.cancelAnimationFrame`
      // layers a `vi.fn` wrapper on top without dropping the underlying
      // map-tracking behavior.
      const cancelAnimationFrameSpy = vi.spyOn(
        globalThis,
        "cancelAnimationFrame",
      );

      try {
        const scrollToMock =
          HTMLElement.prototype.scrollTo as unknown as ReturnType<typeof vi.fn>;
        scrollToMock.mockClear?.();

        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          await settleAsyncUi();

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          expect(messageStack).not.toBeNull();

          // The synchronous first `tick()` in `scheduleSettledScrollToBottom`
          // must have scrolled to `top: 800` (Math.max(1000 - 200, 0) =
          // 800) on the message stack. We do not pin a single call
          // index because the scheduler's rAF follow-ups also run
          // (`requestAnimationFrameMock` queues them as microtasks),
          // so the mock may observe several scroll-to-bottom calls as
          // the stability loop settles — all of them should target
          // 800.
          const callsAtBottom = scrollToMock.mock.calls.filter((call) => {
            const arg = call[0];
            return (
              typeof arg === "object" &&
              arg !== null &&
              (arg as ScrollToOptions).top === 800 &&
              (arg as ScrollToOptions).behavior === "auto"
            );
          });
          expect(callsAtBottom.length).toBeGreaterThan(0);
        } finally {
          teardown();
        }

        // Explicit cleanup assertion: the scheduler's returned cleanup
        // closure checks `if (frameId !== 0) cancelAnimationFrame(frameId)`
        // to avoid a wasted `cancelAnimationFrame(0)` call after the
        // synchronous first `tick()` sets `frameId = 0` before scheduling
        // the next rAF. A regression that dropped the `frameId !== 0`
        // guard would be observable here because the cleanup would call
        // `cancelAnimationFrame(0)` at least once. Running this check
        // AFTER `teardown()` ensures the scheduler's cleanup has
        // definitely executed (SessionPaneContent unmounts on workspace
        // teardown) — the cleanup is otherwise only triggered by
        // effect-dep churn or `afterEach`'s global `cleanup()`.
        const zeroCancels = cancelAnimationFrameSpy.mock.calls.filter(
          ([frameId]) => frameId === 0,
        );
        expect(zeroCancels).toEqual([]);
      } finally {
        cancelAnimationFrameSpy.mockRestore();
        if (originalScrollHeight) {
          Object.defineProperty(
            HTMLElement.prototype,
            "scrollHeight",
            originalScrollHeight,
          );
        } else {
          delete (HTMLElement.prototype as unknown as Record<string, unknown>)
            .scrollHeight;
        }
        if (originalClientHeight) {
          Object.defineProperty(
            HTMLElement.prototype,
            "clientHeight",
            originalClientHeight,
          );
        } else {
          delete (HTMLElement.prototype as unknown as Record<string, unknown>)
            .clientHeight;
        }
      }
    });
  });

  it("registers the message-stack wheel listener as non-passive so preventDefault takes effect", async () => {
    // Regression guard for the native-wheel-handling migration in
    // `SessionPaneView.tsx`. The message stack moved from React's
    // delegated `onWheel` prop to a direct
    // `node.addEventListener("wheel", listener, { passive: false })`
    // because passive wheel listeners silently no-op
    // `preventDefault()` — which meant both the custom
    // `scrollTop` write and the browser's native scroll ran on
    // the same wheel tick, producing a jagged scroll-up
    // experience. A revert to React's prop would reintroduce
    // the regression with no test catching it.
    //
    // Spy on `Element.prototype.addEventListener` globally,
    // capture every `"wheel"` registration with its options,
    // then filter to the one installed on the `.message-stack`
    // node after render. Assert `{ passive: false }`.
    const wheelRegistrations: Array<{
      target: EventTarget;
      options: AddEventListenerOptions | boolean | undefined;
    }> = [];
    const originalAdd = Element.prototype.addEventListener;
    Element.prototype.addEventListener = function patched(
      this: Element,
      type: string,
      listener: EventListenerOrEventListenerObject | null,
      options?: AddEventListenerOptions | boolean,
    ) {
      if (type === "wheel") {
        wheelRegistrations.push({ target: this, options });
      }
      // The cast mirrors the native signature; `listener` can be
      // null in some polyfill shapes but the prototype method
      // handles that.
      return originalAdd.call(
        this,
        type,
        listener as EventListenerOrEventListenerObject,
        options,
      );
    } as typeof Element.prototype.addEventListener;

    try {
      const { cleanup: teardown } = await renderAppWithProjectAndSession();
      try {
        await settleAsyncUi();
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        expect(messageStack).toBeInstanceOf(HTMLElement);

        // Find the wheel registration installed on THIS message
        // stack. Other elements in the tree may also install
        // wheel listeners (Monaco, the virtualized message list,
        // etc.); filter by node identity rather than by count.
        const messageStackRegistration = wheelRegistrations.find(
          (entry) => entry.target === messageStack,
        );
        expect(messageStackRegistration).toBeDefined();
        // A revert to React's `onWheel` prop would NOT install a
        // direct listener on this node — React delegates through
        // the document root and the registration array would not
        // contain this target at all. The `toBeDefined` above
        // catches that. The `{ passive: false }` assertion below
        // catches a narrower regression: someone switched back to
        // a direct listener but forgot the options argument, so
        // the browser defaults to passive on scrolling events in
        // modern Chrome/Firefox.
        expect(messageStackRegistration?.options).toEqual(
          expect.objectContaining({ passive: false }),
        );
      } finally {
        teardown();
      }
    } finally {
      Element.prototype.addEventListener = originalAdd;
    }
  });

  it("clamps a saved docked control panel layout up to the current minimum width", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-control-panel-min-clamp`;
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

    window.history.replaceState(
      window.history.state,
      "",
      "/?workspace=test-control-panel-min-clamp",
    );
    window.localStorage.clear();
    window.localStorage.setItem(
      layoutStorageKey,
      JSON.stringify({
        controlPanelSide: "left",
        workspace: {
          root: {
            id: "split-root",
            type: "split",
            direction: "row",
            ratio: 0.22,
            first: {
              type: "pane",
              paneId: "pane-control",
            },
            second: {
              type: "pane",
              paneId: "pane-session",
            },
          },
          panes: [
            {
              id: "pane-control",
              tabs: [
                {
                  id: "tab-control",
                  kind: "controlPanel",
                  originSessionId: null,
                },
              ],
              activeTabId: "tab-control",
              activeSessionId: null,
              viewMode: "controlPanel",
              lastSessionViewMode: "session",
              sourcePath: null,
            },
            {
              id: "pane-session",
              tabs: [
                {
                  id: "tab-session",
                  kind: "session",
                  sessionId: "session-1",
                },
              ],
              activeTabId: "tab-session",
              activeSessionId: "session-1",
              viewMode: "session",
              lastSessionViewMode: "session",
              sourcePath: null,
            },
          ],
          activePaneId: "pane-session",
        },
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
    Object.defineProperty(document.documentElement, "clientWidth", {
      configurable: true,
      value: 1000,
    });
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();

      await waitFor(() => {
        const persistedLayoutRaw = window.localStorage.getItem(layoutStorageKey);
        expect(persistedLayoutRaw).not.toBeNull();
        const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
          workspace: {
            root: {
              ratio: number;
            } | null;
          };
        };
        expect(persistedLayout.workspace.root?.ratio).toBeCloseTo(0.64, 5);
      });
    } finally {
      delete (document.documentElement as { clientWidth?: number }).clientWidth;
      window.history.replaceState(window.history.state, "", originalUrl);
      window.localStorage.clear();
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("stores manual message scroll state immediately when leaving the bottom", () => {
    const paneScrollPositions: Record<
      string,
      {
        top: number;
        shouldStick: boolean;
      }
    > = {
      "pane-1:session:session-1": {
        top: 1200,
        shouldStick: true,
      },
    };
    const node = {
      clientHeight: 800,
      scrollHeight: 2000,
      scrollTop: 960,
    };

    const next = syncMessageStackScrollPosition(
      node,
      "pane-1:session:session-1",
      paneScrollPositions,
    );

    expect(next).toEqual({
      top: 960,
      shouldStick: false,
    });
    expect(paneScrollPositions["pane-1:session:session-1"]).toEqual(next);
  });

  it("uses the control panel pixel minimum instead of the generic row split clamp", () => {
    document.documentElement.style.setProperty(
      "--control-panel-pane-min-width",
      "14rem",
    );

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

  it("uses the standalone control-surface pixel minimum instead of the generic row split clamp", () => {
    const previousStandalonePaneMinWidth =
      document.documentElement.style.getPropertyValue(
        "--standalone-control-surface-pane-min-width",
      );
    const previousDensityScale =
      document.documentElement.style.getPropertyValue("--density-scale");
    document.documentElement.style.setProperty(
      "--standalone-control-surface-pane-min-width",
      "calc(16rem * var(--density-scale))",
    );
    document.documentElement.style.setProperty("--density-scale", "1");

    try {
      const bounds = getWorkspaceSplitResizeBounds(
        {
          id: "split-1",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: {
            type: "pane",
            paneId: "session-pane",
          },
          second: {
            type: "pane",
            paneId: "git-pane",
          },
        },
        "split-1",
        "row",
        1600,
        new Map([
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
          [
            "git-pane",
            {
              id: "git-pane",
              tabs: [
                {
                  id: "git-tab",
                  kind: "gitStatus",
                  workdir: "C:/repo",
                  originSessionId: null,
                },
              ],
              activeTabId: "git-tab",
              activeSessionId: null,
              viewMode: "gitStatus",
              lastSessionViewMode: "session",
              sourcePath: null,
            },
          ],
        ]),
      );

      expect(bounds.minRatio).toBeCloseTo(22 / 100, 4);
      expect(bounds.maxRatio).toBeCloseTo(84 / 100, 4);
    } finally {
      if (previousStandalonePaneMinWidth) {
        document.documentElement.style.setProperty(
          "--standalone-control-surface-pane-min-width",
          previousStandalonePaneMinWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--standalone-control-surface-pane-min-width",
        );
      }
      if (previousDensityScale) {
        document.documentElement.style.setProperty(
          "--density-scale",
          previousDensityScale,
        );
      } else {
        document.documentElement.style.removeProperty("--density-scale");
      }
    }
  });

  it("matches the standalone control panel width when resolving the initial dock ratio", () => {
    const previousPaneWidth = document.documentElement.style.getPropertyValue(
      "--control-panel-pane-width",
    );
    document.documentElement.style.setProperty(
      "--control-panel-pane-width",
      "40rem",
    );

    const workspaceStage = document.createElement("div");
    workspaceStage.className =
      "workspace-stage workspace-stage-control-panel-only";
    Object.defineProperty(workspaceStage, "clientWidth", {
      configurable: true,
      value: 1200,
    });
    document.body.appendChild(workspaceStage);

    try {
      expect(resolveStandaloneControlPanelDockWidthRatio(0.24)).toBeCloseTo(
        (40 * 16) / 1200,
        5,
      );
    } finally {
      workspaceStage.remove();
      if (previousPaneWidth) {
        document.documentElement.style.setProperty(
          "--control-panel-pane-width",
          previousPaneWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--control-panel-pane-width",
        );
      }
    }
  });

  it("clamps the initial dock ratio when the standalone width would crowd out the session pane", () => {
    const previousPaneWidth = document.documentElement.style.getPropertyValue(
      "--control-panel-pane-width",
    );
    const previousPaneMinWidth =
      document.documentElement.style.getPropertyValue(
        "--control-panel-pane-min-width",
      );
    document.documentElement.style.setProperty(
      "--control-panel-pane-width",
      "40rem",
    );
    document.documentElement.style.setProperty(
      "--control-panel-pane-min-width",
      "40rem",
    );

    const workspaceStage = document.createElement("div");
    workspaceStage.className =
      "workspace-stage workspace-stage-control-panel-only";
    Object.defineProperty(workspaceStage, "clientWidth", {
      configurable: true,
      value: 400,
    });
    document.body.appendChild(workspaceStage);

    try {
      expect(resolveStandaloneControlPanelDockWidthRatio(0.24)).toBeCloseTo(
        1 / (1 + 0.22),
        5,
      );
    } finally {
      workspaceStage.remove();
      if (previousPaneWidth) {
        document.documentElement.style.setProperty(
          "--control-panel-pane-width",
          previousPaneWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--control-panel-pane-width",
        );
      }
      if (previousPaneMinWidth) {
        document.documentElement.style.setProperty(
          "--control-panel-pane-min-width",
          previousPaneMinWidth,
        );
      } else {
        document.documentElement.style.removeProperty(
          "--control-panel-pane-min-width",
        );
      }
    }
  });
});
