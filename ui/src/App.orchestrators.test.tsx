// App.orchestrators.test.tsx
//
// Owns: integration tests for the orchestrator layer of App —
// orchestrator delta adoption (full orchestrator-start state,
// library updates from live orchestrator deltas, session
// merging from orchestrator-carried deltas), grouped-session
// rendering inside the control panel session list, runtime
// action controls (start / pause / resume / stop), runtime
// action error display, and the busy/disabled transition for
// pending runtime actions. Also owns the pure-function fallback
// test for blank orchestrator group names.
//
// Does not own: control panel tests beyond the orchestrator-
// specific grouped-session view (those live in
// App.control-panel.test.tsx), session-lifecycle / model-
// refresh tests, live-state / watchdog tests.
//
// Split out of: ui/src/App.test.tsx (Slice 6 of the App-split
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

describe("App orchestrators", () => {
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

  it("falls back to the template id for blank orchestrator group names", () => {
    const blankNamedOrchestrator = makeOrchestrator({
      templateId: "review-flow",
      templateSnapshot: {
        ...makeOrchestrator().templateSnapshot,
        id: "review-flow",
        name: "   ",
      },
    });

    expect(formatSessionOrchestratorGroupName(blankNamedOrchestrator)).toBe(
      "review-flow",
    );
  });

  it("adopts the full orchestrator-start state so the next delta does not force a resync", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            updatedAt: "2026-03-30 09:01:00",
          }),
        );
      const fetchTemplatesSpy = vi
        .spyOn(api, "fetchOrchestratorTemplates")
        .mockResolvedValue({
          templates: [
            {
              id: "delivery-flow",
              name: "Delivery Flow",
              description: "Implement and review a change.",
              createdAt: "2026-03-30 09:00:00",
              updatedAt: "2026-03-30 09:05:00",
              projectId: "project-local",
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
      const createOrchestratorInstanceSpy = vi
        .spyOn(api, "createOrchestratorInstance")
        .mockResolvedValue({
          orchestrator: {
            id: "orchestrator-1",
            templateId: "delivery-flow",
            projectId: "project-local",
            templateSnapshot: {
              id: "delivery-flow",
              name: "Delivery Flow",
              description: "Implement and review a change.",
              createdAt: "2026-03-30 09:00:00",
              updatedAt: "2026-03-30 09:05:00",
              projectId: "project-local",
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
            status: "running",
            sessionInstances: [],
            createdAt: "2026-03-30 09:06:00",
            completedAt: null,
          },
          state: makeStateResponse({
            revision: 3,
            projects: [
              {
                id: "project-local",
                name: "Local Project",
                rootPath: "/repo",
                remoteId: "local",
              },
              {
                id: "project-added",
                name: "Added By Start",
                rootPath: "/repo-added",
              },
            ],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-orchestrated", {
                name: "Orchestrated Builder",
                projectId: "project-local",
                preview: "Waiting for work",
                status: "active",
                workdir: "/repo",
              }),
            ],
          }),
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

      try {
        await renderApp();

        const eventSource = latestEventSource();
        act(() => {
          eventSource.dispatchOpen();
          eventSource.dispatchNamedEvent("state", {
            revision: 2,
            projects: [
              {
                id: "project-local",
                name: "Local Project",
                rootPath: "/repo",
                remoteId: "local",
              },
            ],
            sessions: [],
          });
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(
          screen.getByRole("tab", { name: "Orchestrators" }),
        );
        expect(
          await screen.findByDisplayValue("Delivery Flow"),
        ).toBeInTheDocument();

        const templateProjectSelect = screen.getByLabelText("Project", {
          selector: "select#orchestrator-template-project",
        });
        fireEvent.change(templateProjectSelect, {
          target: { value: "project-local" },
        });
        expect(templateProjectSelect).toHaveValue("project-local");
        const runButton = document.querySelector<HTMLButtonElement>(
          ".orchestrator-run-button",
        );
        if (!runButton) {
          throw new Error("Run button not found");
        }
        expect(runButton).toBeEnabled();
        await clickAndSettle(runButton);

        await waitFor(() => {
          expect(createOrchestratorInstanceSpy).toHaveBeenCalledWith(
            "delivery-flow",
            "project-local",
          );
        });
        expect(
          await screen.findByText("Orchestrated Builder"),
        ).toBeInTheDocument();

        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "messageCreated",
            revision: 4,
            sessionId: "session-orchestrated",
            messageId: "message-1",
            messageIndex: 0,
            messageCount: 1,
            message: {
              id: "message-1",
              type: "text",
              timestamp: "09:07",
              author: "assistant",
              text: "Orchestration delta applied.",
            },
            preview: "Orchestration delta applied.",
            status: "active",
          });
        });

        await screen.findByText("Orchestration delta applied.");
        expect(fetchStateSpy).not.toHaveBeenCalled();
      } finally {
        window.history.replaceState(window.history.state, "", originalUrl);
        window.localStorage.clear();
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        fetchTemplatesSpy.mockRestore();
        createOrchestratorInstanceSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("updates the orchestrator library from live orchestrator deltas without forcing a resync", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-local",
            name: "Local Project",
            rootPath: "/repo",
            remoteId: "local",
          },
        ],
        orchestrators: [makeOrchestrator()],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Builder",
            projectId: "project-local",
            workdir: "/repo",
          }),
        ],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            updatedAt: "2026-03-30 09:07:00",
          }),
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
          eventSource.dispatchNamedEvent("delta", {
            type: "orchestratorsUpdated",
            revision: 2,
            orchestrators: [
              makeOrchestrator({
                status: "paused",
              }),
            ],
          });
        });
        await settleAsyncUi();

        expect(fetchStateSpy).toHaveBeenCalledTimes(1);
      } finally {
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("merges sessions carried by live orchestrator deltas without forcing a resync", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-local",
            name: "Local Project",
            rootPath: "/repo",
            remoteId: "local",
          },
        ],
        orchestrators: [makeOrchestrator()],
        workspaces: [],
        sessions: [
          makeSession("session-1", {
            name: "Builder",
            projectId: "project-local",
            workdir: "/repo",
          }),
        ],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            updatedAt: "2026-03-30 09:07:00",
          }),
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
      const reviewerTemplateSession = {
        id: "reviewer",
        name: "Reviewer",
        agent: "Claude" as const,
        model: null,
        instructions: "Review the queued work.",
        autoApprove: false,
        inputMode: "queue" as const,
        position: { x: 520, y: 420 },
      };

      try {
        await renderApp();
        const eventSource = latestEventSource();
        await dispatchStateEvent(eventSource, {
          revision: 1,
          projects: [
            {
              id: "project-local",
              name: "Local Project",
              rootPath: "/repo",
              remoteId: "local",
            },
          ],
          orchestrators: [makeOrchestrator()],
          sessions: [
            makeSession("session-1", {
              name: "Builder",
              projectId: "project-local",
              workdir: "/repo",
            }),
          ],
        });

        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        await screen.findByText("Builder");
        fetchStateSpy.mockClear();
        expect(within(sessionList).queryByText("Reviewer")).not.toBeInTheDocument();

        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "orchestratorsUpdated",
            revision: 2,
            orchestrators: [
              makeOrchestrator({
                status: "paused",
                templateSnapshot: {
                  ...makeOrchestrator().templateSnapshot,
                  sessions: [
                    ...makeOrchestrator().templateSnapshot.sessions,
                    reviewerTemplateSession,
                  ],
                },
                sessionInstances: [
                  ...makeOrchestrator().sessionInstances,
                  {
                    templateSessionId: "reviewer",
                    sessionId: "session-2",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                ],
              }),
            ],
            sessions: [
              makeSession("session-2", {
                name: "Reviewer",
                agent: "Claude",
                model: "claude-sonnet-4-5",
                projectId: "project-local",
                workdir: "/repo",
                preview: "Draft review ready.",
                messages: [
                  {
                    id: "message-user-reviewer-1",
                    type: "text",
                    timestamp: "10:00",
                    author: "you",
                    text: "review the implementation",
                  },
                  {
                    id: "message-assistant-reviewer-1",
                    type: "text",
                    timestamp: "10:01",
                    author: "assistant",
                    text: "Draft review ready.",
                  },
                ],
              }),
            ],
          });
        });
        await settleAsyncUi();

        const reviewerRowLabel = await waitFor(() =>
          within(sessionList).getByText("Reviewer"),
        );
        const reviewerRowButton = reviewerRowLabel.closest("button");
        if (!reviewerRowButton) {
          throw new Error("Reviewer session row button not found");
        }
        await clickAndSettle(reviewerRowButton);
        expect(screen.getAllByText("Draft review ready.").length).toBeGreaterThan(0);

        act(() => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textReplace",
            revision: 3,
            sessionId: "session-2",
            messageId: "message-assistant-reviewer-1",
            messageIndex: 1,
            messageCount: 2,
            text: "Reviewer output updated.",
            preview: "Reviewer output updated.",
          });
        });
        await settleAsyncUi();

        expect(screen.getAllByText("Reviewer output updated.").length).toBeGreaterThan(0);
        expect(fetchStateSpy).not.toHaveBeenCalled();
      } finally {
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("groups orchestrated sessions inside the control panel session list", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          const baseOrchestrator = makeOrchestrator();
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-questly",
                name: "Questly",
                rootPath: "/projects/questly",
              },
            ],
            orchestrators: [
              makeOrchestrator({
                id: "orchestrator-review-flow",
                projectId: "project-questly",
                templateId: "review-flow",
                templateSnapshot: {
                  ...baseOrchestrator.templateSnapshot,
                  id: "review-flow",
                  name: "Review Flow",
                  projectId: "project-questly",
                  sessions: [
                    {
                      id: "entry",
                      name: "Entry",
                      agent: "Codex",
                      model: null,
                      instructions: "Start the review flow.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 160, y: 220 },
                    },
                    {
                      id: "codex-reviewer",
                      name: "Codex Reviewer",
                      agent: "Codex",
                      model: null,
                      instructions: "Review the changes.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 420, y: 220 },
                    },
                    {
                      id: "claude-reviewer",
                      name: "Claude Reviewer",
                      agent: "Claude",
                      model: null,
                      instructions: "Double-check the review.",
                      autoApprove: false,
                      inputMode: "queue",
                      position: { x: 680, y: 220 },
                    },
                  ],
                },
                sessionInstances: [
                  {
                    templateSessionId: "entry",
                    sessionId: "session-entry",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                  {
                    templateSessionId: "codex-reviewer",
                    sessionId: "session-codex-reviewer",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                  {
                    templateSessionId: "claude-reviewer",
                    sessionId: "session-claude-reviewer",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                ],
                createdAt: "2026-04-03 10:05:00",
              }),
            ],
            sessions: [
              makeSession("session-entry", {
                name: "Entry",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: 'Running "C:\\WINDOWS\\system32\\wi..."',
              }),
              makeSession("session-standalone", {
                name: "Questly",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: "Current open tracked bugs in [bugs.md].",
              }),
              makeSession("session-codex-reviewer", {
                name: "Codex Reviewer",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: "Ready for a prompt.",
              }),
              makeSession("session-claude-reviewer", {
                name: "Claude Reviewer",
                agent: "Claude",
                model: "claude-sonnet-4-5",
                projectId: "project-questly",
                workdir: "/projects/questly",
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

        await within(sessionList).findByText("Questly");
        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        expect(
          within(sessionList).getAllByRole("group", {
            name: /Orchestration Review Flow/i,
          }),
        ).toHaveLength(1);
        const orchestratorHeaderCopy = orchestratorGroup.querySelector(
          ".session-orchestrator-group-copy",
        );
        if (!(orchestratorHeaderCopy instanceof HTMLElement)) {
          throw new Error("Expected orchestrator header copy block");
        }
        expect(
          within(orchestratorHeaderCopy).getByText("Orchestration"),
        ).toBeInTheDocument();
        expect(within(orchestratorHeaderCopy).getByText("Review Flow")).toBeInTheDocument();
        expect(within(orchestratorHeaderCopy).getByText("3 sessions")).toBeInTheDocument();
        expect(within(orchestratorGroup).getByText("Entry")).toBeInTheDocument();
        expect(within(orchestratorGroup).getByText("Codex Reviewer")).toBeInTheDocument();
        expect(within(orchestratorGroup).getByText("Claude Reviewer")).toBeInTheDocument();
        expect(within(orchestratorGroup).queryByText("Questly")).not.toBeInTheDocument();

        const collapseButton = within(orchestratorGroup).getByRole("button", {
          name: /Collapse Review Flow sessions/i,
        });
        expect(
          collapseButton.querySelector(".session-orchestrator-group-chevron"),
        ).toHaveClass("expanded");

        await clickAndSettle(collapseButton);

        expect(
          within(orchestratorGroup).queryByText("Codex Reviewer"),
        ).not.toBeInTheDocument();
        const expandButton = within(orchestratorGroup).getByRole("button", {
          name: /Expand Review Flow sessions/i,
        });
        expect(
          expandButton.querySelector(".session-orchestrator-group-chevron"),
        ).not.toHaveClass("expanded");

        await clickAndSettle(expandButton);

        expect(within(orchestratorGroup).getByText("Codex Reviewer")).toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
  it("controls orchestrators from the grouped session view", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const buildGroupedSessionState = (
        status: OrchestratorInstance["status"],
        revision: number,
      ) => {
        const baseOrchestrator = makeOrchestrator();
        return {
          revision,
          projects: [
            {
              id: "project-questly",
              name: "Questly",
              rootPath: "/projects/questly",
            },
          ],
          orchestrators: [
            makeOrchestrator({
              id: "orchestrator-review-flow",
              projectId: "project-questly",
              templateId: "review-flow",
              status,
              completedAt:
                status === "stopped" ? "2026-04-03 10:12:00" : null,
              templateSnapshot: {
                ...baseOrchestrator.templateSnapshot,
                id: "review-flow",
                name: "Review Flow",
                projectId: "project-questly",
                sessions: [
                  {
                    id: "entry",
                    name: "Entry",
                    agent: "Codex",
                    model: null,
                    instructions: "Start the review flow.",
                    autoApprove: true,
                    inputMode: "queue",
                    position: { x: 160, y: 220 },
                  },
                  {
                    id: "tester",
                    name: "Tester",
                    agent: "Codex",
                    model: null,
                    instructions: "Run the checks.",
                    autoApprove: true,
                    inputMode: "queue",
                    position: { x: 420, y: 220 },
                  },
                ],
              },
              sessionInstances: [
                {
                  templateSessionId: "entry",
                  sessionId: "session-entry",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
                {
                  templateSessionId: "tester",
                  sessionId: "session-tester",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
              ],
              createdAt: "2026-04-03 10:05:00",
            }),
          ],
          sessions: [
            makeSession("session-entry", {
              name: "Entry",
              projectId: "project-questly",
              workdir: "/projects/questly",
              preview: "[bugs.md](/C:/github/Personal/questly/docs/status...",
            }),
            makeSession("session-tester", {
              name: "Tester",
              projectId: "project-questly",
              workdir: "/projects/questly",
              preview: "`flutter test` passed on the current tree.",
            }),
          ],
        };
      };
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse(buildGroupedSessionState("running", 1));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/pause") {
          return jsonResponse(buildGroupedSessionState("paused", 2));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/resume") {
          return jsonResponse(buildGroupedSessionState("running", 3));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/stop") {
          return jsonResponse(buildGroupedSessionState("stopped", 4));
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

        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        ).toBeInTheDocument();
        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Stop orchestration/,
          }),
        ).toBeInTheDocument();

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        );

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                String(input) ===
                "/api/orchestrators/orchestrator-review-flow/pause",
            ),
          ).toBe(true);
          expect(
            within(orchestratorGroup).getByRole("button", {
              name: /^Resume orchestration/,
            }),
          ).toBeInTheDocument();
        });

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Resume orchestration/,
          }),
        );

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                String(input) ===
                "/api/orchestrators/orchestrator-review-flow/resume",
            ),
          ).toBe(true);
          expect(
            within(orchestratorGroup).getByRole("button", {
              name: /^Pause orchestration/,
            }),
          ).toBeInTheDocument();
        });

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Stop orchestration/,
          }),
        );

        await waitFor(() => {
          expect(
            fetchMock.mock.calls.some(
              ([input]) =>
                String(input) ===
                "/api/orchestrators/orchestrator-review-flow/stop",
            ),
          ).toBe(true);
          expect(orchestratorGroup).toHaveAttribute("data-status", "stopped");
          expect(
            within(orchestratorGroup).queryByRole("button", {
              name: /^Resume orchestration/,
            }),
          ).not.toBeInTheDocument();
          expect(
            within(orchestratorGroup).queryByRole("button", {
              name: /^Stop orchestration/,
            }),
          ).not.toBeInTheDocument();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows orchestrator action errors from the grouped session view", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const baseOrchestrator = makeOrchestrator();
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse({
            revision: 1,
            projects: [
              {
                id: "project-questly",
                name: "Questly",
                rootPath: "/projects/questly",
              },
            ],
            orchestrators: [
              makeOrchestrator({
                id: "orchestrator-review-flow",
                projectId: "project-questly",
                templateId: "review-flow",
                status: "running",
                templateSnapshot: {
                  ...baseOrchestrator.templateSnapshot,
                  id: "review-flow",
                  name: "Review Flow",
                  projectId: "project-questly",
                  sessions: [
                    {
                      id: "entry",
                      name: "Entry",
                      agent: "Codex",
                      model: null,
                      instructions: "Start the review flow.",
                      autoApprove: true,
                      inputMode: "queue",
                      position: { x: 160, y: 220 },
                    },
                  ],
                },
                sessionInstances: [
                  {
                    templateSessionId: "entry",
                    sessionId: "session-entry",
                    lastCompletionRevision: null,
                    lastDeliveredCompletionRevision: null,
                  },
                ],
                createdAt: "2026-04-03 10:05:00",
              }),
            ],
            sessions: [
              makeSession("session-entry", {
                name: "Entry",
                projectId: "project-questly",
                workdir: "/projects/questly",
                preview: "Ready for a prompt.",
              }),
            ],
          });
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/pause") {
          return new Response(JSON.stringify({ error: "pause failed" }), {
            headers: {
              "Content-Type": "application/json",
            },
            status: 500,
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

        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        await clickAndSettle(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        );

        await waitFor(() => {
          expect(screen.getByText("pause failed")).toBeInTheDocument();
        });
        expect(orchestratorGroup).toHaveAttribute("data-status", "running");
        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Pause orchestration/,
          }),
        ).toBeEnabled();
        expect(
          within(orchestratorGroup).getByRole("button", {
            name: /^Stop orchestration/,
          }),
        ).toBeEnabled();
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows pending orchestrator actions as busy and disabled until the request resolves", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const baseOrchestrator = makeOrchestrator();
      const pauseDeferred = createDeferred<Response>();
      const buildGroupedSessionState = (
        status: "running" | "paused" | "stopped",
        revision: number,
      ) => {
        const sessionInstances =
          status === "stopped"
            ? []
            : [
                {
                  templateSessionId: "entry",
                  sessionId: "session-entry",
                  lastCompletionRevision: null,
                  lastDeliveredCompletionRevision: null,
                },
              ];
        const sessions =
          status === "stopped"
            ? []
            : [
                makeSession("session-entry", {
                  name: "Entry",
                  projectId: "project-questly",
                  workdir: "/projects/questly",
                  preview: "Ready for a prompt.",
                }),
              ];

        return {
          revision,
          projects: [
            {
              id: "project-questly",
              name: "Questly",
              rootPath: "/projects/questly",
            },
          ],
          orchestrators: [
            makeOrchestrator({
              id: "orchestrator-review-flow",
              projectId: "project-questly",
              templateId: "review-flow",
              status,
              templateSnapshot: {
                ...baseOrchestrator.templateSnapshot,
                id: "review-flow",
                name: "Review Flow",
                projectId: "project-questly",
                sessions: [
                  {
                    id: "entry",
                    name: "Entry",
                    agent: "Codex",
                    model: null,
                    instructions: "Start the review flow.",
                    autoApprove: true,
                    inputMode: "queue",
                    position: { x: 160, y: 220 },
                  },
                ],
              },
              sessionInstances,
              createdAt: "2026-04-03 10:05:00",
            }),
          ],
          sessions,
        };
      };
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          return jsonResponse(buildGroupedSessionState("running", 1));
        }

        if (url === "/api/orchestrators/orchestrator-review-flow/pause") {
          return pauseDeferred.promise;
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

        const orchestratorGroup = await within(sessionList).findByRole("group", {
          name: /Orchestration Review Flow/i,
        });

        await act(async () => {
          fireEvent.click(
            within(orchestratorGroup).getByRole("button", {
              name: /^Pause orchestration/,
            }),
          );
        });
        await settleAsyncUi();

        expect(
          fetchMock.mock.calls.some(
            ([input]) =>
              String(input) === "/api/orchestrators/orchestrator-review-flow/pause",
          ),
        ).toBe(true);

        const pendingPauseButton = within(orchestratorGroup).getByRole("button", {
          name: /^Pause orchestration/,
        });
        const pendingStopButton = within(orchestratorGroup).getByRole("button", {
          name: /^Stop orchestration/,
        });

        expect(pendingPauseButton).toBeDisabled();
        expect(pendingPauseButton).toHaveAttribute("aria-busy", "true");
        expect(
          pendingPauseButton.querySelector(
            ".session-orchestrator-group-action-spinner",
          ),
        ).not.toBeNull();
        expect(pendingStopButton).toBeDisabled();
        expect(pendingStopButton).not.toHaveAttribute("aria-busy");

        await act(async () => {
          pauseDeferred.resolve(jsonResponse(buildGroupedSessionState("paused", 2)));
          await Promise.resolve();
        });
        await settleAsyncUi();

        await waitFor(() => {
          const resumeButton = within(orchestratorGroup).getByRole("button", {
            name: /^Resume orchestration/,
          });
          expect(orchestratorGroup).toHaveAttribute("data-status", "paused");
          expect(resumeButton).toBeEnabled();
          expect(resumeButton).not.toHaveAttribute("aria-busy");
          expect(
            within(orchestratorGroup).getByRole("button", {
              name: /^Stop orchestration/,
            }),
          ).toBeEnabled();
        });
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
});
