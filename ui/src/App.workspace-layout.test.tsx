// App.workspace-layout.test.tsx
//
// Owns: integration tests for the workspace-layout layer of
// App — the workspace switcher UI (open / saved-workspaces
// listing / new-window / delete / delete-errors / stale-refresh
// races / overlapping-delete ordering / active-workspace delete
// guard), the pagehide keepalive flush of pending layout saves,
// and the workspace-layout restart-required recovery notices
// (claim-side merge, notice clearance, unrelated-error
// preservation) that run outside the live-state transport tests.
//
// Does not own: live-state / SSE recovery (see
// App.live-state.*.test.tsx), session creation / model refresh
// (see App.session-lifecycle.test.tsx planned slice), control
// panel pane-local scoping and DnD tests, scroll / clamp tests.
//
// Split out of: ui/src/App.test.tsx (Slice 4 of the App-split
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

describe("App workspace layout", () => {
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

  it("opens the workspace switcher with one refresh under StrictMode", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
          ],
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
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

      try {
        await act(async () => {
          render(
            <StrictMode>
              <App />
            </StrictMode>,
          );
        });
        await settleAsyncUi();

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await waitFor(() => {
          expect(fetchWorkspaceLayoutsSpy).toHaveBeenCalledTimes(1);
        });
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("shows a workspace switcher with saved workspaces and can open a new workspace window", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const openSpy = vi.spyOn(window, "open").mockImplementation(() => null);
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
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

      try {
        await renderApp();

        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Open tab" }),
        );

        const switcherTrigger = await screen.findByRole("button", {
          name: /workspace /i,
        });
        await clickAndSettle(switcherTrigger);

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        expect(
          within(switcherDialog).getAllByText("monitor-left").length,
        ).toBeGreaterThan(0);
        expect(
          within(switcherDialog).getAllByText("monitor-right").length,
        ).toBeGreaterThan(0);

        await clickAndSettle(
          await screen.findByRole("button", { name: "New window" }),
        );

        expect(openSpy).toHaveBeenCalledTimes(1);
        expect(String(openSpy.mock.calls[0]?.[0] ?? "")).toContain(
          "workspace=",
        );
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        openSpy.mockRestore();
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("deletes a saved workspace from the workspace switcher", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteStoredWorkspaceLayoutSpy = vi.spyOn(
        workspaceStorage,
        "deleteStoredWorkspaceLayout",
      );
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        const deleteButton = within(switcherDialog).getByRole("button", {
          name: "Delete workspace monitor-left",
        });

        await clickAndSettle(deleteButton);

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-right",
          }),
        ).toBeInTheDocument();
        expect(deleteStoredWorkspaceLayoutSpy).toHaveBeenCalledWith(
          "monitor-left",
        );
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        deleteStoredWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
        window.localStorage.removeItem(
          `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        );
      }
    });
  });


  it("shows workspace delete errors and restores the delete button", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockRejectedValue(new Error("Delete failed."));
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        expect(await within(switcherDialog).findByText("Delete failed.")).toBeInTheDocument();
        expect(
          within(switcherDialog).getAllByText("monitor-left").length,
        ).toBeGreaterThan(0);
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toBeEnabled();
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toHaveTextContent("Delete");
        expect(
          window.localStorage.getItem(
            `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
          ),
        ).not.toBeNull();
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
        window.localStorage.removeItem(
          `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        );
      }
    });
  });

  it("disables the workspace delete button while the request is in flight", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const deleteWorkspaceDeferred = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockImplementation((workspaceId: string) => {
          if (workspaceId === "monitor-left") {
            return deleteWorkspaceDeferred.promise;
          }
          throw new Error(`Unexpected workspace delete: ${workspaceId}`);
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toBeDisabled();
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).toHaveTextContent("Deleting");

        deleteWorkspaceDeferred.resolve({
          workspaces: [
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
        window.localStorage.removeItem(
          `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        );
      }
    });
  });

  it("ignores stale workspace refresh results after a delete", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const staleRefresh = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
      const fetchWorkspaceLayoutsSpy = vi.mocked(api.fetchWorkspaceLayouts);
      fetchWorkspaceLayoutsSpy.mockResolvedValueOnce({
        workspaces: [
          {
            id: "monitor-left",
            revision: 4,
            updatedAt: "2026-03-28 18:00:00",
            controlPanelSide: "left",
          },
          {
            id: "monitor-right",
            revision: 1,
            updatedAt: "2026-03-28 17:30:00",
            controlPanelSide: "right",
          },
        ],
      });
      fetchWorkspaceLayoutsSpy.mockReturnValueOnce(staleRefresh.promise);
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

        const switcherTrigger = await screen.findByRole("button", {
          name: /workspace /i,
        });
        await clickAndSettle(switcherTrigger);

        let switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        expect(within(switcherDialog).getAllByText("monitor-left").length).toBeGreaterThan(0);

        await clickAndSettle(switcherTrigger);
        await waitFor(() => {
          expect(
            screen.queryByRole("dialog", { name: "Workspace switcher" }),
          ).not.toBeInTheDocument();
        });

        await clickAndSettle(switcherTrigger);
        switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });

        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );

        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenCalledWith("monitor-left");
        });
        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });

        staleRefresh.resolve({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
        expect(fetchWorkspaceLayoutsSpy).toHaveBeenCalledTimes(2);
        expect(
          within(switcherDialog).getAllByText("monitor-right").length,
        ).toBeGreaterThan(0);
      } finally {
        deleteWorkspaceLayoutSpy.mockRestore();
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("applies overlapping workspace deletes in completion order", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const deleteMonitorLeft = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
      const deleteMonitorRight = createDeferred<{
        workspaces: Array<{
          id: string;
          revision: number;
          updatedAt: string;
          controlPanelSide: "left" | "right";
        }>;
      }>();
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockImplementation((workspaceId: string) => {
          if (workspaceId === "monitor-left") {
            return deleteMonitorLeft.promise;
          }
          if (workspaceId === "monitor-right") {
            return deleteMonitorRight.promise;
          }
          throw new Error(`Unexpected workspace delete: ${workspaceId}`);
        });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
          },
        }),
      );
      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-right`,
        JSON.stringify({
          controlPanelSide: "right",
          workspace: {
            root: null,
            panes: [],
            activePaneId: null,
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

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });
        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        );
        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenNthCalledWith(
            1,
            "monitor-left",
          );
        });

        await clickAndSettle(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-right",
          }),
        );
        await waitFor(() => {
          expect(deleteWorkspaceLayoutSpy).toHaveBeenNthCalledWith(
            2,
            "monitor-right",
          );
        });

        deleteMonitorRight.resolve({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
          ],
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryByText("monitor-right"),
          ).not.toBeInTheDocument();
        });
        expect(
          within(switcherDialog).getAllByText("monitor-left").length,
        ).toBeGreaterThan(0);

        deleteMonitorLeft.resolve({ workspaces: [] });
        await settleAsyncUi();

        await waitFor(() => {
          expect(
            within(switcherDialog).queryAllByText("monitor-left").length,
          ).toBe(0);
        });
        expect(
          window.localStorage.getItem(
            `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-left`,
          ),
        ).toBeNull();
        expect(
          window.localStorage.getItem(
            `${WORKSPACE_LAYOUT_STORAGE_KEY}:monitor-right`,
          ),
        ).toBeNull();
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        window.localStorage.clear();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("does not offer delete for the active workspace in the workspace switcher", async () => {
    await withSuppressedActWarnings(async () => {
      const originalFetch = globalThis.fetch;
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "monitor-left",
              revision: 4,
              updatedAt: "2026-03-28 18:00:00",
              controlPanelSide: "left",
            },
            {
              id: "monitor-right",
              revision: 1,
              updatedAt: "2026-03-28 17:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const deleteWorkspaceLayoutSpy = vi
        .spyOn(api, "deleteWorkspaceLayout")
        .mockResolvedValue({ workspaces: [] });
      const fetchMock = vi.fn(
        async (input: RequestInfo | URL, _init?: RequestInit) => {
          const requestUrl = new URL(String(input), "http://localhost");
          if (requestUrl.pathname === "/api/state") {
            return jsonResponse({
              revision: 1,
              projects: [],
              sessions: [],
            });
          }

          throw new Error(
            `Unexpected fetch: ${requestUrl.pathname}${requestUrl.search}`,
          );
        },
      );

      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=monitor-left",
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

        await clickAndSettle(
          await screen.findByRole("button", { name: /workspace /i }),
        );

        const switcherDialog = await screen.findByRole("dialog", {
          name: "Workspace switcher",
        });

        expect(
          within(switcherDialog).queryByRole("button", {
            name: "Delete workspace monitor-left",
          }),
        ).not.toBeInTheDocument();
        expect(
          within(switcherDialog).getByRole("button", {
            name: "Delete workspace monitor-right",
          }),
        ).toBeInTheDocument();
        expect(deleteWorkspaceLayoutSpy).not.toHaveBeenCalled();
      } finally {
        fetchWorkspaceLayoutsSpy.mockRestore();
        deleteWorkspaceLayoutSpy.mockRestore();
        window.history.replaceState(window.history.state, "", originalUrl);
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("flushes a pending workspace layout save with keepalive on pagehide", async () => {
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
      const fetchWorkspaceLayoutsSpy = vi
        .mocked(api.fetchWorkspaceLayouts)
        .mockResolvedValue({
          workspaces: [
            {
              id: "workspace-next",
              revision: 2,
              updatedAt: "2026-03-30 09:30:00",
              controlPanelSide: "right",
            },
          ],
        });
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-current",
            updatedAt: "2026-03-30 09:31:00",
          }),
        );
      window.localStorage.clear();
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
        saveWorkspaceLayoutSpy.mockClear();
        act(() => {
          window.dispatchEvent(new Event("pagehide"));
        });

        await waitFor(() => {
          expect(saveWorkspaceLayoutSpy).toHaveBeenCalledWith(
            expect.any(String),
            expect.objectContaining({
              workspace: expect.any(Object),
            }),
            { keepalive: true },
          );
        });
      } finally {
        window.localStorage.clear();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        fetchWorkspaceLayoutsSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("does not resave workspace layout when SSE state preserves the same sessions", async () => {
    await withSuppressedActWarnings(async () => {
      vi.useFakeTimers();
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const fetchStateSpy = vi.spyOn(api, "fetchState").mockResolvedValue(makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [
          {
            id: "workspace-current",
            revision: 1,
            updatedAt: "2026-04-10 10:00:00",
            controlPanelSide: "left",
          },
        ],
        sessions: [],
      }));
      const fetchWorkspaceLayoutSpy = vi
        .mocked(api.fetchWorkspaceLayout)
        .mockResolvedValue(null);
      const saveWorkspaceLayoutSpy = vi
        .mocked(api.saveWorkspaceLayout)
        .mockResolvedValue(
          makeWorkspaceLayoutResponse({
            id: "workspace-current",
            updatedAt: "2026-04-10 10:00:01",
          }),
        );
      window.localStorage.clear();
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
        await advanceTimers(200);
        saveWorkspaceLayoutSpy.mockClear();

        await dispatchStateEvent(
          latestEventSource(),
          makeStateResponse({
            revision: 2,
            projects: [],
            orchestrators: [],
            workspaces: [
              {
                id: "workspace-current",
                revision: 2,
                updatedAt: "2026-04-10 10:00:02",
                controlPanelSide: "left",
              },
            ],
            sessions: [],
          }),
        );

        await advanceTimers(200);
        expect(saveWorkspaceLayoutSpy).not.toHaveBeenCalled();
      } finally {
        window.localStorage.clear();
        fetchStateSpy.mockRestore();
        fetchWorkspaceLayoutSpy.mockRestore();
        saveWorkspaceLayoutSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("keeps a claimed local control panel layout side while merging server preferences", async () => {
    const originalFetch = globalThis.fetch;
    const originalEventSource = globalThis.EventSource;
    const originalResizeObserver = globalThis.ResizeObserver;
    const originalUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-control-panel-resize-race`;
    const fetchWorkspaceLayoutDeferred =
      createDeferred<Awaited<ReturnType<typeof api.fetchWorkspaceLayout>> | null>();
    const fetchWorkspaceLayoutSpy = vi
      .mocked(api.fetchWorkspaceLayout)
      .mockImplementation(() => fetchWorkspaceLayoutDeferred.promise);
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
      "/?workspace=test-control-panel-resize-race",
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
    const scrollIntoViewSpy = stubScrollIntoView();

    try {
      await renderApp();
      const divider = document.querySelector(".tile-divider-row");
      if (!(divider instanceof HTMLDivElement)) {
        throw new Error("Control panel divider not found");
      }
      const split = divider.parentElement;
      if (!(split instanceof HTMLDivElement)) {
        throw new Error("Control panel split container not found");
      }
      Object.defineProperty(split, "getBoundingClientRect", {
        configurable: true,
        value: () =>
          ({
            bottom: 800,
            height: 800,
            left: 0,
            right: 2000,
            top: 0,
            width: 2000,
            x: 0,
            y: 0,
            toJSON: () => ({}),
          }) satisfies DOMRect,
      });

      await act(async () => {
        fireEvent.pointerDown(divider, { clientX: 440, clientY: 40 });
        fireEvent.pointerMove(window, { clientX: 840, clientY: 40 });
        fireEvent.pointerUp(window);
        await flushUiWork();
      });

      await act(async () => {
        fetchWorkspaceLayoutDeferred.resolve({
          layout: {
            id: "workspace-test",
            revision: 1,
            updatedAt: "2026-03-30 09:00:00",
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
          },
        });
        await flushUiWork();
      });

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
        // The drag target is 840/2000 = 0.42, but the resize clamp nudges
        // the ratio upward to respect the control panel minimum width.
        expect(persistedLayout.workspace.root?.ratio).toBeCloseTo(0.44, 4);
      });
    } finally {
      window.history.replaceState(window.history.state, "", originalUrl);
      window.localStorage.clear();
      scrollIntoViewSpy.mockRestore();
      fetchWorkspaceLayoutSpy.mockRestore();
      restoreGlobal("fetch", originalFetch);
      restoreGlobal("EventSource", originalEventSource);
      restoreGlobal("ResizeObserver", originalResizeObserver);
    }
  });

  it("clears the tracked workspace-layout restart-required notice after recovery", () => {
    const restartMessage =
      "The running backend does not expose /api/workspaces/test-layout-restart-recovery (HTTP 200). Restart TermAl so the latest API routes are loaded.";

    expect(
      resolveRecoveredWorkspaceLayoutRequestError(
        restartMessage,
        restartMessage,
      ),
    ).toBeNull();
  });

  it("preserves unrelated request errors when a workspace layout recovers", () => {
    const restartMessage =
      "The running backend does not expose /api/workspaces/test-layout-restart-recovery (HTTP 200). Restart TermAl so the latest API routes are loaded.";
    const unrelatedError = "Could not refresh projects.";

    expect(
      resolveRecoveredWorkspaceLayoutRequestError(
        unrelatedError,
        restartMessage,
      ),
    ).toBe(unrelatedError);
    expect(
      resolveRecoveredWorkspaceLayoutRequestError(
        unrelatedError,
        null,
      ),
    ).toBe(unrelatedError);
  });
});
