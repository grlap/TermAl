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

describe("App", () => {
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

  it("restores helper setup globals when renderAppWithProjectAndSession fails", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const originalQuerySelector = Document.prototype.querySelector;
      const querySelectorSpy = vi
        .spyOn(Document.prototype, "querySelector")
        .mockImplementation(function (this: Document, selectors: string) {
          if (selectors === ".session-list") {
            return null;
          }

          return originalQuerySelector.call(this, selectors);
        });

      const originalScrollIntoViewBeforeFailure =
        HTMLElement.prototype.scrollIntoView;

      try {
        await expect(renderAppWithProjectAndSession()).rejects.toThrow(
          "Session list not found",
        );
        expect(globalThis.fetch).toBe(originalFetch);
        expect(globalThis.EventSource).toBe(originalEventSource);
        expect(globalThis.ResizeObserver).toBe(originalResizeObserver);
        expect(HTMLElement.prototype.scrollIntoView).toBe(
          originalScrollIntoViewBeforeFailure,
        );
      } finally {
        querySelectorSpy.mockRestore();
      }
    });
  });

  it("uses the freshly rendered EventSource when prior mock instances exist", async () => {
    await withSuppressedActWarnings(async () => {
      const staleDispatchError = vi.fn(() => {
        throw new Error("stale EventSource should not be reused");
      });
      const priorEventSourceCount = EventSourceMock.instances.length;
      const seededEventSourceCount = (EventSourceMock.instances = [
        ...EventSourceMock.instances,
        {
          dispatchError: staleDispatchError,
        } as unknown as EventSourceMock,
      ]).length;

      let context:
        | Awaited<ReturnType<typeof renderAppWithProjectAndSession>>
        | null = null;
      try {
        context = await renderAppWithProjectAndSession();
        const freshEventSources = EventSourceMock.instances.slice(
          seededEventSourceCount,
        );
        expect(EventSourceMock.instances.length).toBeGreaterThan(
          seededEventSourceCount,
        );
        expect(
          freshEventSources.some(
            (eventSource) => eventSource.url?.includes("/api/events") ?? false,
          ),
        ).toBe(true);
        expect(staleDispatchError).not.toHaveBeenCalled();
      } finally {
        EventSourceMock.instances.splice(priorEventSourceCount);
        context?.cleanup();
      }
    });
  });

  it("opens session find on Ctrl+F even when focused session controls stop propagation", async () => {
    await withSuppressedActWarnings(async () => {
      const context = await renderAppWithProjectAndSession();
      const composer = await screen.findByLabelText("Message Session 1");
      const stopPropagation = (event: KeyboardEvent) => {
        event.stopPropagation();
      };
      composer.addEventListener("keydown", stopPropagation);

      try {
        await act(async () => {
          fireEvent.keyDown(composer, {
            key: "f",
            code: "KeyF",
            ctrlKey: true,
          });
        });
        await settleAsyncUi();

        expect(
          screen.getByRole("search", { name: "Find in session" }),
        ).toBeInTheDocument();
        expect(screen.getByPlaceholderText("Find in session")).toHaveFocus();
      } finally {
        composer.removeEventListener("keydown", stopPropagation);
        context.cleanup();
      }
    });
  });

  it("cancels a pending settle-to-bottom frame when Ctrl+PageUp jumps to the top", async () => {
    await withSuppressedActWarnings(async () => {
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
        const messageStack = document.querySelector(".message-stack");
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
      }
    });
  });

  it("applies the active combobox option on space without closing the menu", () => {
    const onChange = vi.fn();
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(() => {});

    try {
      render(
        <ThemedCombobox
          id="test-combobox"
          value="gpt-5"
          options={[
            { label: "GPT-5", value: "gpt-5" },
            { label: "GPT-5 mini", value: "gpt-5-mini" },
          ]}
          onChange={onChange}
        />,
      );

      fireEvent.click(screen.getByRole("combobox"));
      fireEvent.keyDown(window, { key: "ArrowDown" });
      fireEvent.keyDown(window, { key: " " });

      expect(onChange).toHaveBeenCalledTimes(1);
      expect(onChange).toHaveBeenCalledWith("gpt-5-mini");
      expect(screen.getByRole("listbox")).toBeInTheDocument();

      fireEvent.keyDown(window, { key: "Enter" });

      expect(onChange).toHaveBeenCalledTimes(2);
      expect(onChange).toHaveBeenLastCalledWith("gpt-5-mini");
      expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    } finally {
      scrollIntoViewSpy.mockRestore();
    }
  });

  it("scrolls an off-screen combobox selection into view when the menu opens", async () => {
    const originalGetBoundingClientRect =
      HTMLElement.prototype.getBoundingClientRect;
    HTMLElement.prototype.getBoundingClientRect = function () {
      if (this.classList.contains("combo-menu")) {
        return {
          bottom: 90,
          height: 90,
          left: 0,
          right: 240,
          top: 0,
          width: 240,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }

      const optionIndex = this.getAttribute("data-option-index");
      if (optionIndex !== null) {
        const index = Number(optionIndex);
        const listbox = this.parentElement as HTMLElement | null;
        const top = index * 30 - (listbox?.scrollTop ?? 0);

        return {
          bottom: top + 30,
          height: 30,
          left: 0,
          right: 240,
          top,
          width: 240,
          x: 0,
          y: top,
          toJSON: () => ({}),
        } as DOMRect;
      }

      return originalGetBoundingClientRect.call(this);
    };

    try {
      render(
        <ThemedCombobox
          id="overflow-combobox"
          value="model-7"
          options={Array.from({ length: 8 }, (_, index) => ({
            label: `Model ${index}`,
            value: `model-${index}`,
          }))}
          onChange={vi.fn()}
        />,
      );

      fireEvent.click(screen.getByRole("combobox"));

      const listbox = await screen.findByRole("listbox");
      await waitFor(() => {
        expect(listbox.scrollTop).toBe(150);
      });
    } finally {
      HTMLElement.prototype.getBoundingClientRect =
        originalGetBoundingClientRect;
    }
  });

  it("describes when a Codex model switch resets reasoning effort", () => {
    expect(
      describeCodexModelAdjustmentNotice(
        makeSession("before", {
          model: "gpt-5",
          reasoningEffort: "minimal",
          modelOptions: [
            {
              label: "GPT-5",
              value: "gpt-5",
              supportedReasoningEfforts: ["minimal", "low", "medium", "high"],
            },
          ],
        }),
        makeSession("after", {
          model: "gpt-5-codex-mini",
          reasoningEffort: "medium",
          modelOptions: [
            {
              label: "GPT-5 Codex Mini",
              value: "gpt-5-codex-mini",
              supportedReasoningEfforts: ["medium", "high"],
            },
          ],
        }),
      ),
    ).toBe(
      "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
    );
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

  it("keeps omitted adoptState slices unchanged", () => {
    const preservedCodex = {
      notices: [
        {
          kind: "runtimeNotice" as const,
          level: "warning" as const,
          title: "Existing notice",
          detail: "Keep this codex state when omitted.",
          timestamp: "2026-04-06T00:00:00Z",
        },
      ],
    };
    const preservedReadiness = [
      makeReadiness({
        agent: "Codex",
        detail: "Keep this readiness state when omitted.",
      }),
    ];
    const preservedProjects = [
      {
        id: "project-local",
        name: "Local",
        rootPath: "/repo",
      },
    ];
    const preservedOrchestrators = [
      makeOrchestrator({
        id: "orchestrator-existing",
      }),
    ];
    const preservedWorkspaces = [
      {
        id: "workspace-existing",
        revision: 3,
        updatedAt: "2026-04-06 00:00:00",
        controlPanelSide: "left" as const,
      },
    ];

    const adopted = resolveAdoptedStateSlices(
      {
        codex: preservedCodex,
        agentReadiness: preservedReadiness,
        projects: preservedProjects,
        orchestrators: preservedOrchestrators,
        workspaces: preservedWorkspaces,
      },
      {},
    );

    expect(adopted.codex).toBe(preservedCodex);
    expect(adopted.agentReadiness).toBe(preservedReadiness);
    expect(adopted.projects).toBe(preservedProjects);
    expect(adopted.orchestrators).toBe(preservedOrchestrators);
    expect(adopted.workspaces).toBe(preservedWorkspaces);
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

  it("adopts explicitly empty adoptState slices", () => {
    const currentCodex = {
      notices: [
        {
          kind: "runtimeNotice" as const,
          level: "warning" as const,
          title: "Existing notice",
          detail: "This should be replaced by the next state.",
          timestamp: "2026-04-06T00:00:00Z",
        },
      ],
    };
    const currentReadiness = [
      makeReadiness({
        agent: "Codex",
        detail: "This should be replaced by the next state.",
      }),
    ];
    const currentProjects = [
      {
        id: "project-local",
        name: "Local",
        rootPath: "/repo",
      },
    ];
    const currentOrchestrators = [
      makeOrchestrator({
        id: "orchestrator-existing",
      }),
    ];
    const currentWorkspaces = [
      {
        id: "workspace-existing",
        revision: 3,
        updatedAt: "2026-04-06 00:00:00",
        controlPanelSide: "left" as const,
      },
    ];
    const emptyCodex = { notices: [] };
    const emptyReadiness: typeof currentReadiness = [];
    const emptyProjects: typeof currentProjects = [];
    const emptyOrchestrators: typeof currentOrchestrators = [];
    const emptyWorkspaces: typeof currentWorkspaces = [];

    const adopted = resolveAdoptedStateSlices(
      {
        codex: currentCodex,
        agentReadiness: currentReadiness,
        projects: currentProjects,
        orchestrators: currentOrchestrators,
        workspaces: currentWorkspaces,
      },
      {
        codex: emptyCodex,
        agentReadiness: emptyReadiness,
        projects: emptyProjects,
        orchestrators: emptyOrchestrators,
        workspaces: emptyWorkspaces,
      },
    );

    expect(adopted.codex).toBe(emptyCodex);
    expect(adopted.agentReadiness).toBe(emptyReadiness);
    expect(adopted.projects).toBe(emptyProjects);
    expect(adopted.orchestrators).toBe(emptyOrchestrators);
    expect(adopted.workspaces).toBe(emptyWorkspaces);
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

  it("keeps the new-response button scroll correction alive for the explicit minAttempts floor", async () => {
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

          const scrollToLatestButton = await screen.findByRole("button", {
            name: "New response",
          });
          scrollToMock.mockClear();
          await clickAndSettle(scrollToLatestButton);
          for (let iteration = 0; iteration < 10; iteration += 1) {
            await settleAsyncUi();
          }

          const callsAtBottom = filterScrollToCallsAt(
            scrollToMock,
            800,
            "auto",
          );
          expect(callsAtBottom.length).toBeGreaterThanOrEqual(8);
          expect(callsAtBottom.length).toBeLessThan(60);
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
  it("drags control panel dock sections into the workspace", async () => {
    await withSuppressedActWarnings(async () => {
      const context = await renderAppWithProjectAndSession({
        includeGitStatus: true,
        includeWorkspacePersistence: true,
      });

      try {
        async function dragDockSectionToWorkspace(
          buttonName: string,
          expectedTabName: RegExp,
        ) {
          const dock = await screen.findByRole("navigation", {
            name: "Control panel dock",
          });
          const sectionButton = await within(dock).findByRole("button", {
            name: buttonName,
          });
          expect(sectionButton).toHaveAttribute("draggable", "true");

          const dataTransfer = createDragDataTransfer();
          await act(async () => {
            fireEvent.dragStart(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          const rightDropZone = document.querySelector(".pane-drop-zone-right");
          if (!(rightDropZone instanceof HTMLDivElement)) {
            throw new Error("Right drop zone not found");
          }

          await act(async () => {
            fireEvent.dragEnter(rightDropZone, { dataTransfer });
            fireEvent.dragOver(rightDropZone, { dataTransfer });
            fireEvent.drop(rightDropZone, { dataTransfer });
            fireEvent.dragEnd(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          await waitFor(() => {
            expect(
              screen
                .getAllByRole("tab")
                .some((tab) =>
                  expectedTabName.test(
                    ((tab.getAttribute("aria-label") ?? tab.textContent ?? "").replace(/\u00d7/g, "").trim()),
                  ),
                ),
            ).toBe(true);
          });
        }

        await dragDockSectionToWorkspace("Sessions", /^Sessions$/i);
        await dragDockSectionToWorkspace("Orchestrators", /^Orchestrators$/i);
        await dragDockSectionToWorkspace("Files", /Files: termal/i);
        await dragDockSectionToWorkspace("Git status", /Git status: termal/i);
      } finally {
        context.cleanup();
      }
    });
  });
  it("accepts control panel launcher drags in the pane body when dragover only exposes text/plain", async () => {
    await withSuppressedActWarnings(async () => {
      const context = await renderAppWithProjectAndSession({
        includeWorkspacePersistence: true,
      });

      try {
        const dock = await screen.findByRole("navigation", {
          name: "Control panel dock",
        });
        const sectionButton = await within(dock).findByRole("button", {
          name: "Sessions",
        });
        const dataTransfer = createDragDataTransfer();

        await act(async () => {
          fireEvent.dragStart(sectionButton, { dataTransfer });
        });

        const reducedMimeDataTransfer =
          createReducedMimeDragDataTransfer(dataTransfer);
        const workspaceTabList = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((tabList) =>
            within(tabList).queryByRole("tab", { name: /Session 1/i }),
          );
        if (!(workspaceTabList instanceof HTMLDivElement)) {
          throw new Error("Workspace tab list not found");
        }
        const workspacePane = workspaceTabList.closest(".workspace-pane");
        if (!(workspacePane instanceof HTMLElement)) {
          throw new Error("Workspace pane not found");
        }

        await act(async () => {
          fireEvent.dragEnter(workspacePane, {
            clientX: 240,
            clientY: 220,
            dataTransfer: reducedMimeDataTransfer,
          });
          fireEvent.dragOver(workspacePane, {
            clientX: 240,
            clientY: 220,
            dataTransfer: reducedMimeDataTransfer,
          });
          fireEvent.drop(workspacePane, {
            clientX: 240,
            clientY: 220,
            dataTransfer: reducedMimeDataTransfer,
          });
          fireEvent.dragEnd(sectionButton, { dataTransfer });
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            screen
              .getAllByRole("tab")
              .some((tab) => /Sessions/i.test(tab.textContent ?? "")),
          ).toBe(true);
        });
      } finally {
        context.cleanup();
      }
    });
  });
  it("drops control panel dock sections into the tab rail without splitting the pane", async () => {
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

        const initialTabLists = screen.getAllByRole("tablist", {
          name: "Tile tabs",
        });
        expect(initialTabLists).toHaveLength(2);

        const workspaceTabList = initialTabLists.find((tabList) =>
          /Session 1/i.test(tabList.textContent ?? ""),
        );
        if (!(workspaceTabList instanceof HTMLDivElement)) {
          throw new Error("Workspace tab list not found");
        }
        const workspaceTabRail = workspaceTabList;

        async function dragDockSectionToTabRail(
          buttonName: string,
          expectedTabLabel: RegExp,
        ) {
          const dock = await screen.findByRole("navigation", {
            name: "Control panel dock",
          });
          const sectionButton = await within(dock).findByRole("button", {
            name: buttonName,
          });
          const dataTransfer = createDragDataTransfer();

          await act(async () => {
            fireEvent.dragStart(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          await act(async () => {
            fireEvent.dragEnter(workspaceTabRail, {
              clientX: 200,
              dataTransfer,
            });
            fireEvent.dragOver(workspaceTabRail, {
              clientX: 200,
              dataTransfer,
            });
            fireEvent.drop(workspaceTabRail, { clientX: 200, dataTransfer });
            fireEvent.dragEnd(sectionButton, { dataTransfer });
          });
          await settleAsyncUi();

          await waitFor(() => {
            const tabLists = screen.getAllByRole("tablist", {
              name: "Tile tabs",
            });
            expect(tabLists).toHaveLength(2);
            const updatedWorkspaceTabList = tabLists.find((tabList) =>
              /Session 1/i.test(tabList.textContent ?? ""),
            );
            expect(updatedWorkspaceTabList).toBeTruthy();
            expect(
              within(updatedWorkspaceTabList as HTMLElement)
                .getAllByRole("tab")
                .some((tab) =>
                  expectedTabLabel.test(
                    ((tab.getAttribute("aria-label") ?? tab.textContent ?? "").replace(/\u00d7/g, "").trim()),
                  ),
                ),
            ).toBe(true);
          });
        }

        await dragDockSectionToTabRail("Sessions", /^.*Sessions.*$/i);
        await dragDockSectionToTabRail("Orchestrators", /^.*Orchestrators.*$/i);
        await dragDockSectionToTabRail("Files", /Files:\s*termal/i);
        await dragDockSectionToTabRail("Git status", /Git status:\s*termal/i);
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
  it("separates theme selection from editor and UI appearance controls in preferences", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
    const fetchStateSpy = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => fetchStateDeferred.promise);
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
        fetchStateDeferred.resolve(makeStateResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        }));
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );

      expect(
        screen.getByRole("radiogroup", { name: "UI theme" }),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("radiogroup", { name: "UI style" }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("heading", { level: 3, name: "Font sizes" }),
      ).not.toBeInTheDocument();

      await clickAndSettle(
        screen.getByRole("tab", { name: "Editor & UI" }),
      );

      expect(
        screen.getByRole("heading", { level: 3, name: "Font sizes" }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("radiogroup", { name: "UI theme" }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("radiogroup", { name: "UI style" }),
      ).not.toBeInTheDocument();
    } finally {
      scrollIntoViewSpy.mockRestore();
      fetchStateSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("persists UI density changes from the appearance preferences", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
    const fetchStateSpy = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => fetchStateDeferred.promise);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    const scrollIntoViewSpy = stubScrollIntoView();
    window.localStorage.clear();
    document.documentElement.style.removeProperty("--density-scale");

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve(makeStateResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        }));
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );
      await clickAndSettle(
        screen.getByRole("tab", { name: "Editor & UI" }),
      );

      const densitySlider = screen.getByRole("slider", { name: "UI density" });
      expect((densitySlider as HTMLInputElement).value).toBe("100");

      await act(async () => {
        fireEvent.change(densitySlider, { target: { value: "85" } });
      });
      await settleAsyncUi();

      expect(
        document.documentElement.style.getPropertyValue("--density-scale"),
      ).toBe("0.85");
      expect(window.localStorage.getItem("termal-ui-density")).toBe("85");
    } finally {
      scrollIntoViewSpy.mockRestore();
      fetchStateSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("persists UI style changes from the themes preferences", async () => {
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const fetchStateDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
    const fetchStateSpy = vi
      .spyOn(api, "fetchState")
      .mockImplementation(() => fetchStateDeferred.promise);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    const scrollIntoViewSpy = stubScrollIntoView();
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-ui-style");

    try {
      await renderApp();
      await act(async () => {
        fetchStateDeferred.resolve(makeStateResponse({
          revision: 1,
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [],
        }));
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );
      const styleGroup = screen.getByRole("radiogroup", { name: "UI style" });
      await clickAndSettle(
        within(styleGroup).getByRole("radio", { name: /Blueprint/i }),
      );

      expect(document.documentElement.dataset.uiStyle).toBe("blueprint-style");
      expect(window.localStorage.getItem("termal-ui-style")).toBe(
        "blueprint-style",
      );
    } finally {
      scrollIntoViewSpy.mockRestore();
      fetchStateSpy.mockRestore();
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });
});
