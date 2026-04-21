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
