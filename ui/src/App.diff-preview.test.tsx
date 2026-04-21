// App.diff-preview.test.tsx
//
// Owns: App-level tests for the Git diff preview feature —
// restored-diff-document refresh collection (deduplicating
// transient loading placeholders), hydration of stripped
// restored diff tabs after workspace layout readiness, save
// option forwarding through the App save adapter, restore
// error display on stripped diff tabs when document refresh
// fails, and the stale-response guards that keep late-arriving
// diff responses from overwriting newer state (both the
// post-unmount late-resolve case and the re-opened
// same-request-key manual-refresh case).
//
// Does not own: the workspace-layout tests that set up the
// restart-required recovery notices (see
// App.workspace-layout.test.tsx).
//
// Split out of: ui/src/App.test.tsx (Slice 10 of the App-split
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

describe("App diff preview", () => {
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

  it("collects restored Git diff document refreshes without duplicating transient loading placeholders", () => {
    const durableRequest = {
      path: "docs/README.md",
      sectionId: "unstaged" as const,
      workdir: "/repo",
    };
    const workspace: WorkspaceState = {
      root: {
        type: "pane",
        paneId: "pane-a",
      },
      panes: [
        {
          id: "pane-a",
          activeTabId: "restored",
          activeSessionId: null,
          viewMode: "diffPreview",
          lastSessionViewMode: "session",
          sourcePath: null,
          tabs: [
            {
              id: "transient",
              kind: "diffPreview",
              changeType: "edit",
              diff: "",
              diffMessageId: "git-preview:/repo:unstaged::loading.md",
              filePath: "/repo/loading.md",
              gitDiffRequest: {
                path: "loading.md",
                sectionId: "unstaged",
                workdir: "/repo",
              },
              gitDiffRequestKey: "git-preview:/repo:unstaged::loading.md",
              gitSectionId: "unstaged",
              isLoading: true,
              language: "markdown",
              originSessionId: null,
              summary: "Loading",
            },
            {
              id: "restored",
              kind: "diffPreview",
              changeType: "edit",
              diff: "-before\n+after",
              diffMessageId: "git-preview:/repo:unstaged::docs/README.md",
              filePath: "/repo/docs/README.md",
              gitDiffRequest: durableRequest,
              gitDiffRequestKey: "git-preview:/repo:unstaged::docs/README.md",
              gitSectionId: "unstaged",
              isLoading: true,
              language: "markdown",
              originSessionId: null,
              summary: "Updated README",
            },
            {
              id: "attempted",
              kind: "diffPreview",
              changeType: "edit",
              diff: "-old\n+new",
              diffMessageId: "git-preview:/repo:unstaged::attempted.md",
              filePath: "/repo/attempted.md",
              gitDiffRequest: {
                path: "attempted.md",
                sectionId: "unstaged",
                workdir: "/repo",
              },
              gitDiffRequestKey: "git-preview:/repo:unstaged::attempted.md",
              gitSectionId: "unstaged",
              language: "markdown",
              originSessionId: null,
              summary: "Attempted",
            },
          ],
        },
      ],
      activePaneId: "pane-a",
    };

    expect(
      collectRestoredGitDiffDocumentContentRefreshes(
        workspace,
        new Set(),
        new Set(["git-preview:/repo:unstaged::attempted.md"]),
      ),
    ).toEqual([
      {
        request: durableRequest,
        requestKey: "git-preview:/repo:unstaged::docs/README.md",
        sectionId: "unstaged",
      },
    ]);
  });

  type DiffPreviewWorkspaceTab = Extract<
    WorkspaceTab,
    { kind: "diffPreview" }
  >;

  const restoredGitDiffRequest = {
    path: "docs/README.md",
    sectionId: "unstaged",
    workdir: "/repo",
  } satisfies api.GitDiffRequestPayload;
  const restoredGitDiffRequestKey =
    "git-preview:/repo:unstaged::docs/README.md";

  function makeRestoredGitDiffWorkspace(
    tabOverrides: Partial<DiffPreviewWorkspaceTab> = {},
  ): WorkspaceState {
    const restoredTab: DiffPreviewWorkspaceTab = {
      id: restoredGitDiffRequestKey,
      kind: "diffPreview",
      changeType: "edit",
      diff: "-# Before restored\n+# After restored\n",
      diffMessageId: restoredGitDiffRequestKey,
      filePath: "/repo/docs/README.md",
      gitDiffRequest: restoredGitDiffRequest,
      gitDiffRequestKey: restoredGitDiffRequestKey,
      gitSectionId: "unstaged",
      isLoading: true,
      language: "markdown",
      originSessionId: null,
      originProjectId: null,
      summary: "Updated README",
      ...tabOverrides,
    };

    return {
      root: {
        type: "pane",
        paneId: "pane-restored",
      },
      panes: [
        {
          id: "pane-restored",
          activeTabId: restoredGitDiffRequestKey,
          activeSessionId: null,
          viewMode: "diffPreview",
          lastSessionViewMode: "session",
          sourcePath: null,
          tabs: [restoredTab],
        },
      ],
      activePaneId: "pane-restored",
    };
  }

  function makeRestoredGitDiffResponse(
    overrides: Partial<api.GitDiffResponse> = {},
  ): api.GitDiffResponse {
    return {
      changeType: "edit",
      changeSetId: "restored-change-set",
      diff: "-# Before restored\n+# After restored\n",
      diffId: restoredGitDiffRequestKey,
      documentEnrichmentNote: "Loaded full Markdown document.",
      documentContent: {
        before: {
          content: "# Before restored\n\nBefore body.\n",
          source: "index",
        },
        after: {
          content: "# After restored\n\nAfter restored body.\n",
          source: "worktree",
        },
        canEdit: true,
        isCompleteDocument: true,
      },
      filePath: "/repo/docs/README.md",
      language: "markdown",
      summary: "Restored README",
      ...overrides,
    };
  }

  it("hydrates stripped restored Git diff tabs after workspace layout readiness", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-15 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: makeRestoredGitDiffWorkspace(),
          }),
        );
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockResolvedValue(makeRestoredGitDiffResponse());
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();

        await waitFor(() => {
          expect(fetchWorkspaceLayoutSpy).toHaveBeenCalledWith(
            "workspace-test",
          );
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });
        expect(fetchGitDiffSpy).toHaveBeenCalledWith(restoredGitDiffRequest);

        expect(
          await screen.findByText("After restored body."),
        ).toBeInTheDocument();
        await clickAndSettle(screen.getByRole("button", { name: "Raw patch" }));
        expect(
          await screen.findByText("Loaded full Markdown document."),
        ).toBeInTheDocument();

        await settleAsyncUi();
        expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("forwards diff preview save options through the App save adapter", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const diffWorkspace: WorkspaceState = {
        root: {
          type: "pane",
          paneId: "pane-diff",
        },
        panes: [
          {
            id: "pane-diff",
            activeTabId: "diff-options",
            activeSessionId: null,
            viewMode: "diffPreview",
            lastSessionViewMode: "session",
            sourcePath: null,
            tabs: [
              {
                id: "diff-options",
                kind: "diffPreview",
                changeType: "edit",
                diff: [
                  "@@ -1,3 +1,3 @@",
                  " # Title",
                  " ",
                  "-Original body.",
                  "+Saved body.",
                ].join("\n"),
                diffMessageId: "diff-options",
                filePath: "/repo/docs/README.md",
                gitSectionId: "unstaged",
                language: "markdown",
                originProjectId: "project-termal",
                originSessionId: null,
                summary: "Updated README",
                documentContent: {
                  before: {
                    content: "# Title\n\nOriginal body.\n",
                    source: "index",
                  },
                  after: {
                    content: "# Title\n\nSaved body.\n",
                    source: "worktree",
                  },
                  canEdit: true,
                  isCompleteDocument: true,
                },
              },
            ],
          },
        ],
        activePaneId: "pane-diff",
      };
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/repo",
          },
        ],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-16 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: diffWorkspace,
          }),
        );
      const fetchFileSpy = vi.spyOn(api, "fetchFile").mockResolvedValue({
        path: "/repo/docs/README.md",
        content: "# Title\n\nSaved body.\n",
        contentHash: "sha256:base",
        language: "markdown",
      });
      const saveFileSpy = vi
        .spyOn(api, "saveFile")
        .mockRejectedValueOnce(new Error("file changed on disk before save"))
        .mockResolvedValueOnce({
          path: "/repo/docs/README.md",
          content: "# Title\n\nSaved body refined.\n",
          contentHash: "sha256:saved",
          language: "markdown",
        });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();
        await waitFor(() => {
          expect(fetchFileSpy).toHaveBeenCalledWith("/repo/docs/README.md", {
            projectId: "project-termal",
            sessionId: null,
          });
        });
        const editor = await screen.findByTestId("monaco-diff-editor-modified");
        await act(async () => {
          fireEvent.change(editor, {
            target: { value: "# Title\n\nSaved body refined.\n" },
          });
        });

        await clickAndSettle(screen.getByRole("button", { name: "Mock diff save" }));

        await waitFor(() => {
          expect(saveFileSpy).toHaveBeenNthCalledWith(
            1,
            "/repo/docs/README.md",
            "# Title\n\nSaved body refined.\n",
            {
              baseHash: "sha256:base",
              overwrite: undefined,
              projectId: "project-termal",
              sessionId: null,
            },
          );
        });

        expect(await screen.findByText("Save failed")).toBeInTheDocument();
        await clickAndSettle(screen.getByRole("button", { name: "Save anyway" }));

        await waitFor(() => {
          expect(saveFileSpy).toHaveBeenNthCalledWith(
            2,
            "/repo/docs/README.md",
            "# Title\n\nSaved body refined.\n",
            {
              baseHash: "sha256:base",
              overwrite: true,
              projectId: "project-termal",
              sessionId: null,
            },
          );
        });
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchFileSpy.mockRestore();
        saveFileSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows a restore error on stripped Git diff tabs when document refresh fails", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-15 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: makeRestoredGitDiffWorkspace(),
          }),
        );
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockRejectedValue(new Error("restore failed"));
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();

        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });
        const alert = await screen.findByRole("alert");
        expect(within(alert).getByText("Unable to load diff")).toBeInTheDocument();
        expect(within(alert).getByText("restore failed")).toBeInTheDocument();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("ignores late restored Git diff document responses after App unmount", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const restoreUpdateSpy = vi.fn();
      const restoreDeferred = createDeferred<api.GitDiffResponse>();
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-15 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: makeRestoredGitDiffWorkspace(),
          }),
        );
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockImplementation(() => restoreDeferred.promise);
      setAppTestHooksForTests({
        onRestoredGitDiffDocumentContentUpdate: restoreUpdateSpy,
      });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();
        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });

        await act(async () => {
          cleanup();
          await flushUiWork();
        });
        await act(async () => {
          restoreDeferred.resolve(makeRestoredGitDiffResponse());
          await flushUiWork();
        });

        expect(restoreUpdateSpy).not.toHaveBeenCalled();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        setAppTestHooksForTests(null);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("ignores stale manual Git diff responses after reopening the same request key", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const staleDiffDeferred = createDeferred<api.GitDiffResponse>();
      const currentDiffDeferred = createDeferred<api.GitDiffResponse>();
      const gitWorkspace: WorkspaceState = {
        root: {
          type: "pane",
          paneId: "pane-git",
        },
        panes: [
          {
            id: "pane-git",
            activeSessionId: null,
            activeTabId: "git-status",
            lastSessionViewMode: "session",
            sourcePath: null,
            tabs: [
              {
                id: "git-status",
                kind: "gitStatus",
                originProjectId: null,
                originSessionId: null,
                workdir: "/repo",
              },
            ],
            viewMode: "gitStatus",
          },
        ],
        activePaneId: "pane-git",
      };
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-04-16 09:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-test",
            workspace: gitWorkspace,
          }),
        );
      const fetchGitStatusSpy = vi.spyOn(api, "fetchGitStatus").mockResolvedValue({
        ahead: 0,
        behind: 0,
        branch: "main",
        files: [
          {
            path: "src/example.ts",
            worktreeStatus: "M",
          },
        ],
        isClean: false,
        repoRoot: "/repo",
        upstream: "origin/main",
        workdir: "/repo",
      });
      const fetchGitDiffSpy = vi
        .spyOn(api, "fetchGitDiff")
        .mockImplementationOnce(() => staleDiffDeferred.promise)
        .mockImplementationOnce(() => currentDiffDeferred.promise);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=workspace-test",
      );

      try {
        await renderApp();

        await clickAndSettle(await screen.findByRole("button", { name: /^example\.ts$/i }));
        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(1);
        });

        await clickAndSettle(screen.getByRole("tab", { name: /Git status: repo/i }));
        await clickAndSettle(await screen.findByRole("button", { name: /^example\.ts$/i }));
        await waitFor(() => {
          expect(fetchGitDiffSpy).toHaveBeenCalledTimes(2);
        });

        await act(async () => {
          currentDiffDeferred.resolve({
            changeType: "edit",
            diff: ["@@ -1 +1 @@", "-const value = 1;", "+const value = 2;"].join("\n"),
            diffId: "current-diff",
            filePath: "src/example.ts",
            language: "typescript",
            summary: "Current diff",
          });
          await flushUiWork();
        });
        expect(await screen.findByTestId("monaco-diff-editor")).toHaveTextContent(
          "const value = 2;",
        );

        await act(async () => {
          staleDiffDeferred.resolve({
            changeType: "edit",
            diff: ["@@ -1 +1 @@", "-const value = 1;", "+const value = 999;"].join("\n"),
            diffId: "stale-diff",
            filePath: "src/example.ts",
            language: "typescript",
            summary: "Stale diff",
          });
          await flushUiWork();
        });

        expect(screen.getByTestId("monaco-diff-editor")).toHaveTextContent(
          "const value = 2;",
        );
        expect(screen.getByTestId("monaco-diff-editor")).not.toHaveTextContent(
          "const value = 999;",
        );
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchGitStatusSpy.mockRestore();
        fetchGitDiffSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
});
