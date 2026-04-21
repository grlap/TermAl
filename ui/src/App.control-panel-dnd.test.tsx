// App.control-panel-dnd.test.tsx
//
// Owns: integration tests for the control-panel drag/drop layer
// of App — dock-section drags that split the workspace,
// launcher drags that the pane body accepts even when the
// dragover payload only exposes text/plain, and tab-rail drops
// that land the control-panel section without splitting the
// receiving pane.
//
// Does not own: non-DnD control-panel behaviour (see
// App.control-panel.test.tsx), workspace-layout or live-state
// tests, scroll / layout-clamp tests.
//
// Split out of: ui/src/App.test.tsx (Slice 8 of the App-split
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

describe("App control panel DnD", () => {
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
});

