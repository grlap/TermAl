// App.control-panel.test.tsx
//
// Owns: integration tests for the control-panel layer of App —
// workspace-root resolution, workspace-tab → control-panel
// section mapping, control-surface session list entry building
// (including the orchestrator mapping selection), project
// selector filtering, project deletion (context-menu trigger,
// stale-resolution guard for unmounted components, and the
// mirrored rejection path), standalone project tab isolation
// from the docked scope, pane-local session / project scoping
// for the control-panel selection (project-scoped tabs,
// persisted nearest-session context, Files / Git re-scoping,
// projectless-session re-scoping with filter reset), Canvas
// opening with the pane-local session context (including
// moving an existing shared canvas into a new launch context),
// and Files/Git status opening behaviour. Also owns the
// control-panel divider resizability test.
//
// Does not own: control-panel drag/drop tests (those live in
// App.control-panel-dnd.test.tsx), layout clamp tests (see
// App.scroll-behavior.test.tsx planned slice), scroll-
// restoration tests.
//
// Split out of: ui/src/App.test.tsx (Slice 7 of the App-split
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

describe("App control panel", () => {
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

  it("derives control panel workspace roots only from the active workspace or a local project", () => {
    expect(resolveControlPanelWorkspaceRoot(null, null)).toBeNull();
    expect(resolveControlPanelWorkspaceRoot(null, "")).toBeNull();
    expect(resolveControlPanelWorkspaceRoot(null, "   ")).toBeNull();
    expect(
      resolveControlPanelWorkspaceRoot(null, "  /workspace/current  "),
    ).toBe("/workspace/current");
    expect(
      resolveControlPanelWorkspaceRoot(
        {
          id: "project-api",
          name: "API",
          rootPath: "/projects/api",
        },
        null,
      ),
    ).toBe("/projects/api");
    expect(
      resolveControlPanelWorkspaceRoot(
        {
          id: "project-remote",
          name: "Remote",
          rootPath: "/remote/repo",
          remoteId: "ssh-lab",
        },
        "/workspace/current",
      ),
    ).toBeNull();
  });

  it("maps workspace tabs to control panel sections", () => {
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-files",
        kind: "filesystem",
        rootPath: "/repo",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("files");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-git",
        kind: "gitStatus",
        workdir: "/repo",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("git");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-orchestrators",
        kind: "orchestratorList",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("orchestrators");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-projects",
        kind: "projectList",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("projects");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-sessions",
        kind: "sessionList",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBe("sessions");
    expect(
      resolveControlSurfaceSectionIdForWorkspaceTab({
        id: "tab-source",
        kind: "source",
        path: "/repo/src/main.rs",
        originSessionId: null,
      } satisfies WorkspaceTab),
    ).toBeNull();
  });

  it("builds control-surface entries with standalone sessions and the newest orchestrator mapping", () => {
    const templateSnapshot = makeOrchestrator().templateSnapshot;
    const standaloneSession = makeSession("session-standalone", {
      name: "Standalone",
    });
    const sharedSession = makeSession("session-shared", {
      name: "Shared",
    });
    const latestSession = makeSession("session-latest", {
      name: "Latest",
    });
    const olderOrchestrator = makeOrchestrator({
      id: "orchestrator-older",
      templateId: "older-flow",
      createdAt: "2026-03-30 09:00:00",
      templateSnapshot: {
        ...templateSnapshot,
        id: "older-flow",
        name: "Older Flow",
      },
      sessionInstances: [
        {
          templateSessionId: "builder",
          sessionId: "session-shared",
          lastCompletionRevision: null,
          lastDeliveredCompletionRevision: null,
        },
      ],
    });
    const newestOrchestrator = makeOrchestrator({
      id: "orchestrator-newest",
      templateId: "newest-flow",
      createdAt: "2026-03-30 10:00:00",
      templateSnapshot: {
        ...templateSnapshot,
        id: "newest-flow",
        name: "   ",
      },
      sessionInstances: [
        {
          templateSessionId: "builder",
          sessionId: "session-shared",
          lastCompletionRevision: null,
          lastDeliveredCompletionRevision: null,
        },
        {
          templateSessionId: "reviewer",
          sessionId: "session-latest",
          lastCompletionRevision: null,
          lastDeliveredCompletionRevision: null,
        },
      ],
    });

    const entries = buildControlSurfaceSessionListEntries(
      [standaloneSession, sharedSession, latestSession],
      [olderOrchestrator, newestOrchestrator],
    );

    expect(entries).toHaveLength(2);
    expect(entries[0]).toEqual({
      kind: "session",
      session: standaloneSession,
    });

    const groupedEntry = entries[1];
    if (!groupedEntry || groupedEntry.kind !== "orchestratorGroup") {
      throw new Error("Expected an orchestrator group entry");
    }
    expect(groupedEntry.orchestrator.id).toBe("orchestrator-newest");
    expect(groupedEntry.sessions.map((session) => session.id)).toEqual([
      "session-shared",
      "session-latest",
    ]);
    expect(formatSessionOrchestratorGroupName(groupedEntry.orchestrator)).toBe(
      "newest-flow",
    );
  });

  it("returns no control-surface entries for an empty session list", () => {
    expect(buildControlSurfaceSessionListEntries([], [makeOrchestrator()])).toEqual(
      [],
    );
  });

  it("filters sessions from the control panel project selector", async () => {
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
        await screen.findByText("API");
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );

        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("All projects");

        await selectComboboxOption("Project", /^API$/i);

        await waitFor(() => {
          expect(screen.getByText("No sessions in API.")).toBeInTheDocument();
        });

        await clickAndSettle(
          await screen.findByRole("button", { name: "Files" }),
        );
        expect(
          screen.getByRole("combobox", { name: "Project" }),
        ).toHaveTextContent("API");
        await settleAsyncUi();
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
        // still pending. Unmount before resolving, then resolve — the
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
        // several setState functions — none of which should fire after
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
  it("keeps the control panel divider resizable when a session pane is open", async () => {
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
              name: "Session 1",
              preview: "Ready for a prompt.",
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
        await screen.findByRole("button", { name: "Sessions" }),
      );

      const sessionList = document.querySelector(".session-list");
      if (!(sessionList instanceof HTMLDivElement)) {
        throw new Error("Session list not found");
      }

      const sessionRowLabel = await within(sessionList).findByText("Session 1");
      const sessionRowButton = sessionRowLabel.closest("button");
      if (!sessionRowButton) {
        throw new Error("Session row button not found");
      }

      await clickAndSettle(sessionRowButton);

      expect(document.querySelector(".tile-divider-row")).not.toBeNull();
      expect(document.querySelector(".tile-divider-row.fixed")).toBeNull();
    } finally {
      scrollIntoViewSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
});

