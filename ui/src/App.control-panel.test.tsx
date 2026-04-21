// App.control-panel.test.tsx
//
// Owns: the compact control-panel helper and smoke coverage:
// workspace-root resolution, workspace-tab -> control-panel
// section mapping, control-surface session list entry building,
// project selector filtering, and the divider-resizable check.
//
// Does not own:
//   - pane-local session/project scoping tests, which live in
//     App.control-panel.scoping.test.tsx
//   - opener / Files / Git / Canvas flows, which live in
//     App.control-panel.openers.test.tsx
//
// Split out of: ui/src/App.test.tsx (Slice 7), then reduced in
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

describe("App control panel - helpers and compact coverage", () => {
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

