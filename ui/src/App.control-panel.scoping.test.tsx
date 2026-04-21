// App.control-panel.scoping.test.tsx
//
// Owns: pane-local control-panel scoping and project/session
// context tests: project deletion/reset behavior, standalone
// project isolation, project-scoped tab selection, persisted
// nearest-session context, and standalone Files/Git re-scoping.
//
// Does not own:
//   - compact helper coverage, which stays in
//     App.control-panel.test.tsx
//   - opener / Canvas flows, which live in
//     App.control-panel.openers.test.tsx
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

describe("App control panel - scoping", () => {
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

  it("removes projects from the context menu and resets project scopes", async () => {
    await withSuppressedActWarnings(async () => {
      const initialState = makeStateResponse({
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
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-api", {
            name: "API Session",
            projectId: "project-api",
            workdir: "/projects/api",
          }),
          makeSession("session-web", {
            name: "Web Session",
            projectId: "project-web",
            workdir: "/projects/web",
          }),
        ],
      });
      const deletedState = makeStateResponse({
        revision: 2,
        projects: [
          {
            id: "project-web",
            name: "Web",
            rootPath: "/projects/web",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-api", {
            name: "API Session",
            projectId: null,
            workdir: "/projects/api",
          }),
          makeSession("session-web", {
            name: "Web Session",
            projectId: "project-web",
            workdir: "/projects/web",
          }),
        ],
      });
      const workspaceWithApiOrigin: WorkspaceState = {
        root: { type: "pane", paneId: "pane-api-origin" },
        panes: [
          {
            id: "pane-api-origin",
            tabs: [
              {
                id: "terminal-api-origin",
                kind: "terminal",
                workdir: "/projects/api",
                originSessionId: null,
                originProjectId: "project-api",
              },
            ],
            activeTabId: "terminal-api-origin",
            activeSessionId: null,
            viewMode: "terminal",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-api-origin",
      };
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      vi.mocked(api.fetchWorkspaceLayout).mockResolvedValue(
        makeWorkspaceLayoutResponse({ workspace: workspaceWithApiOrigin }),
      );
      const saveWorkspaceLayoutSpy = vi.mocked(api.saveWorkspaceLayout);
      const deleteProjectSpy = vi
        .spyOn(api, "deleteProject")
        .mockResolvedValue(deletedState);
      const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      function projectRow(surface: HTMLElement, name: string) {
        const label = within(surface).getByText(name);
        const row = label.closest("button");
        if (!(row instanceof HTMLButtonElement)) {
          throw new Error(`Project row not found for ${name}`);
        }

        return row;
      }

      try {
        await renderApp();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );

        const projectSurfaces = Array.from(
          document.querySelectorAll(".project-controls"),
        ).filter(
          (surface): surface is HTMLElement => surface instanceof HTMLElement,
        );
        const dockedProjectSurface = projectSurfaces[0];
        const standaloneProjectSurface =
          projectSurfaces[projectSurfaces.length - 1];
        if (!dockedProjectSurface || !standaloneProjectSurface) {
          throw new Error("Project surfaces not found");
        }

        await clickAndSettle(projectRow(dockedProjectSurface, "API"));
        await clickAndSettle(projectRow(standaloneProjectSurface, "API"));
        expect(projectRow(dockedProjectSurface, "API")).toHaveClass("selected");
        expect(projectRow(standaloneProjectSurface, "API")).toHaveClass(
          "selected",
        );

        await act(async () => {
          fireEvent.contextMenu(projectRow(dockedProjectSurface, "API"), {
            clientX: 160,
            clientY: 120,
          });
        });
        await settleAsyncUi();

        const menu = await screen.findByRole("menu", {
          name: "API project actions",
        });
        await clickAndSettle(
          within(menu).getByRole("menuitem", { name: "Remove project" }),
        );

        await waitFor(() => {
          expect(deleteProjectSpy).toHaveBeenCalledWith("project-api");
        });
        expect(confirmSpy).toHaveBeenCalledWith(
          'Remove "API" from TermAl? Existing sessions stay in All projects. Files on disk are not deleted.',
        );

        await waitFor(() => {
          const currentSurfaces = Array.from(
            document.querySelectorAll(".project-controls"),
          ).filter(
            (surface): surface is HTMLElement => surface instanceof HTMLElement,
          );
          expect(currentSurfaces.length).toBeGreaterThanOrEqual(2);
          for (const surface of currentSurfaces) {
            expect(within(surface).queryByText("API")).not.toBeInTheDocument();
            expect(projectRow(surface, "All projects")).toHaveClass("selected");
          }
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("All projects");
        expect(screen.getByText("API Session")).toBeInTheDocument();

        await waitFor(() => {
          const clearedWorkspaceSave = saveWorkspaceLayoutSpy.mock.calls.find(
            ([, payload]) => {
              const workspace = payload.workspace as WorkspaceState;
              return workspace.panes.some((pane) =>
                pane.tabs.some(
                  (tab) =>
                    tab.id === "terminal-api-origin" &&
                    "originProjectId" in tab &&
                    tab.originProjectId === null,
                ),
              );
            },
          );
          expect(clearedWorkspaceSave).toBeTruthy();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
      }
    });
  });

  it("swallows a late deleteProject resolution after the app unmounts without running post-unmount state updates", async () => {
    // Regression for the `if (!isMountedRef.current) return;` guard in the
    // try-branch of `handleProjectMenuRemoveProject` (round 7). The
    // existing `removes projects from the context menu...` test resolves
    // `deleteProject` synchronously while the component is still mounted,
    // so the guard never fires. This test uses a deferred to drive the
    // opposite timing: click Remove, unmount mid-request, then resolve.
    // Without the guard, the unmounted component would run
    // `adoptState(state)`, `resetRemovedProjectSelection(project.id)`, and
    // `setRequestError(null)`. In React 18 these setState calls are
    // silent no-ops. Pin the guard directly with a test-only hook placed
    // after the `isMountedRef` check but before the post-await state
    // update path.
    await withSuppressedActWarnings(async () => {
      let deleteResolve!: (state: AppTestStateResponse) => void;
      const deletePromise = new Promise<AppTestStateResponse>((resolve) => {
        deleteResolve = resolve;
      });
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-api",
            name: "API",
            rootPath: "/projects/api",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      });
      const deletedState = makeStateResponse({
        revision: 2,
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      const deleteProjectSpy = vi
        .spyOn(api, "deleteProject")
        .mockReturnValue(deletePromise);
      const postAwaitPathSpy = vi.fn();
      setAppTestHooksForTests({
        onDeleteProjectPostAwaitPath: postAwaitPathSpy,
      });
      const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();
      const consoleErrorMessages: string[] = [];
      const originalConsoleError = console.error;
      const consoleErrorSpy = vi
        .spyOn(console, "error")
        .mockImplementation((message?: unknown, ...args: unknown[]) => {
          if (
            typeof message === "string" &&
            message.includes("not wrapped in act")
          ) {
            return;
          }
          if (typeof message === "string") {
            consoleErrorMessages.push(message);
          }
          originalConsoleError.call(console, message, ...args);
        });

      try {
        let renderResult!: ReturnType<typeof render>;
        await act(async () => {
          renderResult = render(<App />);
        });
        await settleAsyncUi();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        const apiRow = (
          await screen.findByText("API")
        ).closest("button") as HTMLButtonElement | null;
        if (!(apiRow instanceof HTMLButtonElement)) {
          throw new Error("API project row button not found");
        }

        await act(async () => {
          fireEvent.contextMenu(apiRow, { clientX: 160, clientY: 120 });
        });
        await settleAsyncUi();
        const menu = await screen.findByRole("menu", {
          name: "API project actions",
        });
        await clickAndSettle(
          within(menu).getByRole("menuitem", { name: "Remove project" }),
        );

        // At this point `deleteProject` was called but the deferred is
        // still pending. Unmount before resolving, then resolve â€” the
        // guard must prevent the post-unmount try-branch from running.
        expect(deleteProjectSpy).toHaveBeenCalledWith("project-api");
        expect(confirmSpy).toHaveBeenCalledTimes(1);

        await act(async () => {
          renderResult.unmount();
          await flushUiWork();
        });

        await act(async () => {
          deleteResolve(deletedState);
          await flushUiWork();
        });

        // No error messages should have been logged by the post-unmount
        // resolution. A missing guard would land on `adoptState(...)`
        // which calls `syncPreferencesFromState`, `adoptSessions`, and
        // several setState functions â€” none of which should fire after
        // unmount.
        expect(postAwaitPathSpy).not.toHaveBeenCalled();
        expect(consoleErrorMessages).toEqual([]);
      } finally {
        consoleErrorSpy.mockRestore();
        scrollIntoViewSpy.mockRestore();
      }
    });
  });

  it("swallows a late deleteProject rejection after the app unmounts without running post-unmount reportRequestError", async () => {
    // Regression for the `if (!isMountedRef.current) return;` guard in
    // the catch-branch of `handleProjectMenuRemoveProject`. Mirror of the
    // try-branch test above, but with a rejecting deferred: unmount
    // before rejection, then reject. Without the catch-branch guard,
    // `reportRequestError` would call `setRequestError` on the unmounted
    // component. React 18 makes that a silent no-op, so pin the guard
    // directly with a test-only hook placed after the mounted check and
    // before `reportRequestError`.
    await withSuppressedActWarnings(async () => {
      let deleteReject!: (error: unknown) => void;
      const deletePromise = new Promise<AppTestStateResponse>((_, reject) => {
        deleteReject = reject;
      });
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-api",
            name: "API",
            rootPath: "/projects/api",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      const deleteProjectSpy = vi
        .spyOn(api, "deleteProject")
        .mockReturnValue(deletePromise);
      const postAwaitPathSpy = vi.fn();
      setAppTestHooksForTests({
        onDeleteProjectPostAwaitPath: postAwaitPathSpy,
      });
      const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();
      const consoleErrorMessages: string[] = [];
      const originalConsoleError = console.error;
      const consoleErrorSpy = vi
        .spyOn(console, "error")
        .mockImplementation((message?: unknown, ...args: unknown[]) => {
          if (
            typeof message === "string" &&
            message.includes("not wrapped in act")
          ) {
            return;
          }
          if (typeof message === "string") {
            consoleErrorMessages.push(message);
          }
          originalConsoleError.call(console, message, ...args);
        });

      try {
        let renderResult!: ReturnType<typeof render>;
        await act(async () => {
          renderResult = render(<App />);
        });
        await settleAsyncUi();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        const apiRow = (
          await screen.findByText("API")
        ).closest("button") as HTMLButtonElement | null;
        if (!(apiRow instanceof HTMLButtonElement)) {
          throw new Error("API project row button not found");
        }

        await act(async () => {
          fireEvent.contextMenu(apiRow, { clientX: 160, clientY: 120 });
        });
        await settleAsyncUi();
        const menu = await screen.findByRole("menu", {
          name: "API project actions",
        });
        await clickAndSettle(
          within(menu).getByRole("menuitem", { name: "Remove project" }),
        );

        expect(deleteProjectSpy).toHaveBeenCalledWith("project-api");
        expect(confirmSpy).toHaveBeenCalledTimes(1);

        await act(async () => {
          renderResult.unmount();
          await flushUiWork();
        });

        await act(async () => {
          deleteReject(new Error("backend rejected the delete"));
          await flushUiWork();
        });

        // Same invariant as the resolve test: no console errors from the
        // post-unmount rejection path. The guard prevents
        // `reportRequestError` from firing after the unmount.
        expect(postAwaitPathSpy).not.toHaveBeenCalled();
        expect(consoleErrorMessages).toEqual([]);
      } finally {
        consoleErrorSpy.mockRestore();
        scrollIntoViewSpy.mockRestore();
      }
    });
  });

  it("keeps standalone project tabs independent from the docked control panel scope", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
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
              makeSession("session-api", {
                name: "API Session",
                projectId: "project-api",
                workdir: "/projects/api",
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
          eventSource.dispatchError();
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Projects" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );

        const projectSurfaces = Array.from(
          document.querySelectorAll(".project-controls"),
        );
        const standaloneProjectsSurface =
          projectSurfaces[projectSurfaces.length - 1] ?? null;
        if (!(standaloneProjectsSurface instanceof HTMLElement)) {
          throw new Error("Standalone projects surface not found");
        }

        const apiRowLabel = within(standaloneProjectsSurface).getByText("API");
        const apiRowButton = apiRowLabel.closest("button");
        if (!apiRowButton) {
          throw new Error("Standalone API project row not found");
        }

        await clickAndSettle(apiRowButton);
        expect(apiRowButton).toHaveClass("selected");

        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("All projects");
        expect(screen.getByText("Web Session")).toBeInTheDocument();
        expect(screen.getByText("API Session")).toBeInTheDocument();
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

  it("keeps the control panel project aligned with the session when selecting a project-scoped tab", async () => {
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
                  name: "Session 1",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                }),
              ],
            });
          }

          if (requestUrl.pathname === "/api/git/status") {
            const repoPath = requestUrl.searchParams.get("path") ?? "";
            const repoSegments = repoPath.split("/").filter(Boolean);
            const repoName =
              repoSegments[repoSegments.length - 1] ?? "workspace";
            return jsonResponse({
              ahead: 0,
              behind: 0,
              branch: "main",
              files: [],
              isClean: true,
              repoRoot: repoPath,
              upstream: "origin/main",
              workdir: repoPath,
              statusMessage: `${repoName} ready`,
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

      function getSessionTablist() {
        const sessionTablist = screen
          .getAllByRole("tablist")
          .find((candidate) => within(candidate).queryByText("Session 1"));

        if (!sessionTablist) {
          throw new Error("Session tablist not found");
        }

        return sessionTablist;
      }

      function getControlPanelShell() {
        const controlPanelShell = document.querySelector(
          ".control-panel-shell",
        );
        if (!(controlPanelShell instanceof HTMLDivElement)) {
          throw new Error("Control panel shell not found");
        }

        return controlPanelShell;
      }

      async function selectControlPanelProject(optionName: string | RegExp) {
        const combobox = within(getControlPanelShell()).getByRole("combobox", {
          name: "Project",
        });
        await clickAndSettle(combobox);

        const listbox = await screen.findByRole("listbox");
        const option = within(listbox)
          .getAllByRole("option")
          .find((candidate) => {
            const label =
              candidate
                .querySelector(".combo-option-label")
                ?.textContent?.trim() ??
              candidate.textContent?.trim() ??
              "";

            return typeof optionName === "string"
              ? label === optionName
              : optionName.test(label);
          });

        if (!option) {
          throw new Error(
            `Control panel project option not found for ${String(optionName)}`,
          );
        }

        await clickAndSettle(option);
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
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
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
        await clickAndSettle(
          await screen.findByRole("button", { name: "Git status" }),
        );

        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");

        await selectControlPanelProject(/^API$/i);
        await clickAndSettle(
          within(getControlPanelShell()).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );

        const sessionTablist = getSessionTablist();
        expect(
          within(sessionTablist).getByText(/^api$/i),
        ).toBeInTheDocument();

        await selectControlPanelProject(/^TermAl$/i);
        await clickAndSettle(
          within(getControlPanelShell()).getByTitle(
            "Open tab or drag it into the workspace",
          ),
        );
        expect(
          within(sessionTablist).getByText(/^termal$/i),
        ).toBeInTheDocument();
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");

        await clickAndSettle(
          within(sessionTablist).getByRole("tab", { name: /Git status: api/i }),
        );
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("TermAl");
      } finally {
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("persists the nearest session context when selecting the control panel tab", async () => {
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

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-control-panel-tab-context-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-control-panel-tab-context-sync",
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
                paneId: "pane-review",
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
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

        await clickAndSettle(
          screen.getByRole("tab", { name: /Control panel/i }),
        );

        await waitFor(() => {
          const persistedLayoutRaw = window.localStorage.getItem(
            "termal-workspace-layout:test-control-panel-tab-context-sync",
          );
          expect(persistedLayoutRaw).not.toBeNull();
          const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
            workspace: {
              activePaneId: string | null;
              panes: Array<{
                id: string;
                tabs: Array<Record<string, unknown>>;
              }>;
            };
          };
          const persistedControlPane = persistedLayout.workspace.panes.find(
            (pane) => pane.id === "pane-control",
          );

          expect(persistedLayout.workspace.activePaneId).toBe("pane-control");
          expect(persistedControlPane?.tabs).toContainEqual({
            id: "tab-control",
            kind: "controlPanel",
            originSessionId: "session-2",
            originProjectId: "project-api",
          });
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
  it("re-scopes standalone Files and Git panes to the nearest session when selected", async () => {
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

      function getPaneByTabName(name: string | RegExp) {
        const tab = screen.getByRole("tab", { name });
        const pane = tab.closest(".workspace-pane");
        if (!(pane instanceof HTMLElement)) {
          throw new Error(`Workspace pane not found for ${String(name)}`);
        }

        return pane;
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
        "/?workspace=test-standalone-control-surface-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-standalone-control-surface-sync",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.18,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-right-1",
                type: "split",
                direction: "row",
                ratio: 0.33,
                first: {
                  type: "pane",
                  paneId: "pane-files",
                },
                second: {
                  id: "split-right-2",
                  type: "split",
                  direction: "row",
                  ratio: 0.5,
                  first: {
                    type: "pane",
                    paneId: "pane-git",
                  },
                  second: {
                    type: "pane",
                    paneId: "pane-review",
                  },
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
                id: "pane-files",
                tabs: [
                  {
                    id: "tab-files",
                    kind: "filesystem",
                    rootPath: "/projects/termal",
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-files",
                activeSessionId: "session-1",
                viewMode: "filesystem",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-git",
                tabs: [
                  {
                    id: "tab-git",
                    kind: "gitStatus",
                    workdir: "/projects/termal",
                    originSessionId: "session-1",
                    originProjectId: "project-termal",
                  },
                ],
                activeTabId: "tab-git",
                activeSessionId: "session-1",
                viewMode: "gitStatus",
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

        await clickAndSettle(
          screen.getByRole("tab", { name: /Files: termal/i }),
        );

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Files: api/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        expect(
          within(getPaneByTabName(/Files: api/i)).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        await waitFor(() => {
          const request = latestRequestTo("/api/fs");
          expect(request.searchParams.get("path")).toBe("/projects/api");
          expect(request.searchParams.get("sessionId")).toBe("session-2");
          expect(request.searchParams.get("projectId")).toBe("project-api");
        });

        await clickAndSettle(screen.getByRole("tab", { name: /Git status: termal/i }));

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Git status: api/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        expect(
          within(getPaneByTabName(/Git status: api/i)).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");
        await waitFor(() => {
          const request = latestRequestTo("/api/git/status");
          expect(request.searchParams.get("path")).toBe("/projects/api");
          expect(request.searchParams.get("sessionId")).toBe("session-2");
          expect(request.searchParams.get("projectId")).toBe("project-api");
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
  it("re-scopes a standalone Files pane to a projectless nearest session and resets the control panel filter", async () => {
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
                  projectId: "project-api",
                  workdir: "/projects/api",
                }),
                makeSession("session-2", {
                  name: "Workspace Only",
                  projectId: null,
                  workdir: "/workspace/review-only",
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

      function getPaneByTabName(name: string | RegExp) {
        const tab = screen.getByRole("tab", { name });
        const pane = tab.closest(".workspace-pane");
        if (!(pane instanceof HTMLElement)) {
          throw new Error(`Workspace pane not found for ${String(name)}`);
        }

        return pane;
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
        "/?workspace=test-projectless-standalone-control-surface-sync",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        "termal-workspace-layout:test-projectless-standalone-control-surface-sync",
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.18,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                id: "split-right",
                type: "split",
                direction: "row",
                ratio: 0.45,
                first: {
                  type: "pane",
                  paneId: "pane-files",
                },
                second: {
                  type: "pane",
                  paneId: "pane-workspace",
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
                id: "pane-files",
                tabs: [
                  {
                    id: "tab-files",
                    kind: "filesystem",
                    rootPath: "/projects/api",
                    originSessionId: "session-1",
                    originProjectId: "project-api",
                  },
                ],
                activeTabId: "tab-files",
                activeSessionId: "session-1",
                viewMode: "filesystem",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-workspace",
                tabs: [
                  {
                    id: "tab-workspace",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-workspace",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-workspace",
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

        await clickAndSettle(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        );
        const projectListbox = await screen.findByRole("listbox");
        const apiOption = within(projectListbox)
          .getAllByRole("option")
          .find((candidate) => {
            const label =
              candidate
                .querySelector(".combo-option-label")
                ?.textContent?.trim() ??
              candidate.textContent?.trim() ??
              "";
            return /^API$/i.test(label);
          });
        if (!apiOption) {
          throw new Error("Project option not found for API");
        }
        await clickAndSettle(apiOption);
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("API");

        await clickAndSettle(screen.getByRole("tab", { name: /Files: api/i }));

        await waitFor(() => {
          expect(
            screen.getByRole("tab", { name: /Files: review-only/i }),
          ).toBeInTheDocument();
        });
        expect(
          within(getControlPanelShell()).getByRole("combobox", {
            name: "Project",
          }),
        ).toHaveTextContent("All projects");
        expect(
          within(getPaneByTabName(/Files: review-only/i)).getByDisplayValue(
            "/workspace/review-only",
          ),
        ).toBeInTheDocument();
        await waitFor(() => {
          const request = latestRequestTo("/api/fs");
          expect(request.searchParams.get("path")).toBe(
            "/workspace/review-only",
          );
          expect(request.searchParams.get("sessionId")).toBe("session-2");
          expect(request.searchParams.has("projectId")).toBe(false);
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

