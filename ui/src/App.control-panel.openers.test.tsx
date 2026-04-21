// App.control-panel.openers.test.tsx
//
// Owns: control-panel opener flows: loading project files without
// a session, opening standalone tabs, opening Canvas from pane-local
// context, moving shared canvas state, and pane-local Files/Git openers.
//
// Does not own:
//   - compact helper coverage, which stays in
//     App.control-panel.test.tsx
//   - pane-local scoping / nearest-session tests, which live in
//     App.control-panel.scoping.test.tsx
//
// Split out of: ui/src/App.control-panel.test.tsx during
// Slice 7R of the App-split plan.

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

describe("App control panel - openers and canvas", () => {
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

  it("loads project files in the control panel without requiring a session", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const requestUrl = new URL(String(input), "http://localhost");
      if (requestUrl.pathname === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [
            {
              id: "project-api",
              name: "API",
              rootPath: "/projects/api",
            },
            {
              id: "project-web",
              name: "Web",
              rootPath: "/projects/web",
            },
          ],
          sessions: [
            makeSession("session-web", {
              name: "Web Session",
              projectId: "project-web",
              workdir: "/projects/web",
            }),
          ],
        });
      }

      if (requestUrl.pathname === "/api/fs") {
        expect(requestUrl.searchParams.get("path")).toBe("/projects/api");
        expect(requestUrl.searchParams.get("sessionId")).toBeNull();
        expect(requestUrl.searchParams.get("projectId")).toBe("project-api");
        return jsonResponse({
          entries: [
            {
              kind: "file",
              name: "README.md",
              path: "/projects/api/README.md",
            },
          ],
          name: "api",
          path: "/projects/api",
        });
      }

      if (requestUrl.pathname === "/api/git/status") {
        expect(requestUrl.searchParams.get("path")).toBe("/projects/api");
        expect(requestUrl.searchParams.get("sessionId")).toBeNull();
        expect(requestUrl.searchParams.get("projectId")).toBe("project-api");
        return jsonResponse({
          ahead: 0,
          behind: 0,
          branch: "main",
          files: [],
          isClean: true,
          repoRoot: "/projects/api",
          upstream: "origin/main",
          workdir: "/projects/api",
        });
      }

      throw new Error(
        `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
      );
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

      await selectComboboxOption("Project", /^API$/i);
      await clickAndSettle(
        await screen.findByRole("button", { name: "Files" }),
      );

      expect(
        await screen.findByRole("button", { name: /^README\.md/i }),
      ).toBeInTheDocument();
      expect(
        screen.queryByText(
          "This file browser is no longer associated with a live session or project.",
        ),
      ).not.toBeInTheDocument();
    } finally {
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("opens standalone tabs for sessions, projects, and git from the control panel", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
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
              ],
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: "/projects/termal",
              upstream: "origin/main",
              workdir: "/projects/termal",
            });
          }

          if (requestUrl.pathname === "/api/orchestrators/templates") {
            return jsonResponse({
              templates: [
                {
                  id: "delivery-flow",
                  name: "Delivery Flow",
                  description: "Implement and review a change.",
                  createdAt: "2026-03-26 10:00:00",
                  updatedAt: "2026-03-26 10:15:00",
                  sessions: [
                    {
                      id: "builder",
                      name: "Builder",
                      agent: "Codex",
                      model: null,
                      instructions: "Implement the change.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 220, y: 420 },
                    },
                  ],
                  transitions: [],
                },
              ],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      function getSessionTablist() {
        const sessionTablist = screen
          .getAllByRole("tablist")
          .find((candidate) => within(candidate).queryByText("Session 1"));

        if (!sessionTablist) {
          throw new Error("Session tablist not found");
        }

        return sessionTablist;
      }

      window.localStorage.clear();
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

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );
        expect(
          within(getSessionTablist()).getByText("Sessions"),
        ).toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );
        expect(
          within(getSessionTablist()).getByText("Projects"),
        ).toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Git status" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );
        expect(
          within(getSessionTablist()).getByText(/^termal$/i),
        ).toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("TermAl");
        expect(
          screen.queryByRole("button", { name: /Load repo/i }),
        ).not.toBeInTheDocument();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Orchestrators" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Edit canvas" }),
        );
        expect(
          within(getSessionTablist()).getByText("Orchestration: delivery-flow"),
        ).toBeInTheDocument();
        expect(
          await screen.findByRole("heading", {
            level: 3,
            name: "Edit template",
          }),
        ).toBeInTheDocument();
      } finally {
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("opens canvas from the control panel using the pane-local session context", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
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
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: "/projects/termal",
              upstream: "origin/main",
              workdir: "/projects/termal",
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
      function getTablistForSession(name: string) {
        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) => within(candidate).queryByText(name));

        if (!tablist) {
          throw new Error(`Tablist not found for session ${name}`);
        }

        return tablist;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-pane-local-control-panel",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-pane-local-control-panel",
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
                id: "split-content",
                type: "split",
                direction: "row",
                ratio: 0.5,
                first: {
                  type: "pane",
                  paneId: "pane-main",
                },
                second: {
                  type: "pane",
                  paneId: "pane-review",
                },
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
                id: "pane-main",
                tabs: [
                  {
                    id: "tab-main",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-canvas",
                    kind: "canvas",
                    cards: [],
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-main",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        expect(
          within(getTablistForSession("Main")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Review")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Canvas" }),
        );

        expect(
          within(getTablistForSession("Main")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Review")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("moves an existing shared canvas into the new launch context and syncs its pane state", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/fs") {
            const path = requestUrl.searchParams.get("path") ?? "";
            const segments = path.split("/").filter(Boolean);
            const name = segments[segments.length - 1] ?? "workspace";
            return jsonResponse({
              entries: [],
              name,
              path,
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            const path = requestUrl.searchParams.get("path") ?? "/projects/api";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: path,
              upstream: "origin/main",
              workdir: path,
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
      function getTablistForSession(name: string) {
        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) => within(candidate).queryByText(name));

        if (!tablist) {
          throw new Error(`Tablist not found for session ${name}`);
        }

        return tablist;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-canvas-relocation-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-canvas-relocation-sync",
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
                id: "split-content",
                type: "split",
                direction: "row",
                ratio: 0.45,
                first: {
                  type: "pane",
                  paneId: "pane-review",
                },
                second: {
                  type: "pane",
                  paneId: "pane-main",
                },
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
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-main",
                tabs: [
                  {
                    id: "tab-main",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-canvas",
                    kind: "canvas",
                    cards: [],
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-main",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        expect(
          within(getTablistForSession("Main")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Review")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Canvas" }),
        );

        expect(
          within(getTablistForSession("Review")).getByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeInTheDocument();
        expect(
          within(getTablistForSession("Main")).queryByRole("tab", {
            name: /Canvas/i,
          }),
        ).toBeNull();

        const persistedLayoutRaw = window.localStorage.getItem(
          "termal-workspace-layout:test-canvas-relocation-sync",
        );
        expect(persistedLayoutRaw).not.toBeNull();
        const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
          workspace: {
            panes: Array<{
              id: string;
              activeTabId: string | null;
              activeSessionId: string | null;
              tabs: Array<Record<string, unknown>>;
            }>;
          };
        };
        const persistedReviewPane = persistedLayout.workspace.panes.find(
          (pane) => pane.id === "pane-review",
        );
        const persistedMainPane = persistedLayout.workspace.panes.find(
          (pane) => pane.id === "pane-main",
        );

        expect(persistedReviewPane?.activeTabId).toBe("tab-canvas");
        expect(persistedReviewPane?.activeSessionId).toBe("session-2");
        expect(persistedReviewPane?.tabs).toContainEqual({
          id: "tab-canvas",
          kind: "canvas",
          cards: [],
          originSessionId: "session-2",
          originProjectId: "project-api",
        });
        expect(persistedMainPane?.tabs).toEqual([
          {
            id: "tab-main",
            kind: "session",
            sessionId: "session-1",
          },
        ]);
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("opens Files and Git status from the control panel using the pane-local session context", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
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
                {
                  id: "project-api",
                  name: "API",
                  rootPath: "/projects/api",
                },
              ],
              sessions: [
                makeSession("session-1", {
                  name: "Main",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
                makeSession("session-2", {
                  name: "Review",
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/fs") {
            const path = requestUrl.searchParams.get("path") ?? "";
            const segments = path.split("/").filter(Boolean);
            const name = segments[segments.length - 1] ?? "workspace";
            return jsonResponse({
              entries: [],
              name,
              path,
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            const path = requestUrl.searchParams.get("path") ?? "";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: path,
              upstream: "origin/main",
              workdir: path,
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

      function getControlPanelShell() {
        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        return controlPanelShell;
      }

      function latestRequestTo(pathname: string) {
        const requests = fetchMock.mock.calls
          .map(([input]) => new URL(String(input), "http://localhost"))
          .filter((requestUrl) => requestUrl.pathname === pathname);
        const request = requests[requests.length - 1];
        if (!request) {
          throw new Error(`No request captured for ${pathname}`);
        }

        return request;
      }

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-pane-local-control-panel-files-git",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-pane-local-control-panel-files-git",
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
                id: "split-content",
                type: "split",
                direction: "row",
                ratio: 0.5,
                first: {
                  type: "pane",
                  paneId: "pane-main",
                },
                second: {
                  type: "pane",
                  paneId: "pane-review",
                },
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
              {
                id: "pane-review",
                tabs: [
                  {
                    id: "tab-review",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-review",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-review",
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
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        const controlPanelShell = getControlPanelShell();

        expect(
          screen.queryByRole("tab", { name: /Files: termal/i }),
        ).toBeNull();
        expect(screen.queryByRole("tab", { name: /Files: api/i })).toBeNull();

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Files" }),
        );
        await clickAndSettle(
          within(controlPanelShell).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Files: termal/i }),
          ).toBeInTheDocument();
        });
        expect(screen.queryByRole("tab", { name: /Files: api/i })).toBeNull();
        await waitFor(() => {
          const request = latestRequestTo("/api/fs");
          expect(request.searchParams.get("path")).toBe("/projects/termal");
          expect(request.searchParams.get("sessionId")).toBe("session-1");
          expect(request.searchParams.get("projectId")).toBe("project-termal");
        });

        await clickAndSettle(
          within(controlPanelShell).getByRole("button", { name: "Git status" }),
        );
        await clickAndSettle(
          within(controlPanelShell).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Git status: termal/i }),
          ).toBeInTheDocument();
        });
        expect(screen.queryByRole("tab", { name: /Git status: api/i })).toBeNull();
        await waitFor(() => {
          const request = latestRequestTo("/api/git/status");
          expect(request.searchParams.get("path")).toBe("/projects/termal");
          expect(request.searchParams.get("sessionId")).toBe("session-1");
          expect(request.searchParams.get("projectId")).toBe("project-termal");
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
});

