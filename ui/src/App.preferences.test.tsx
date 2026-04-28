// App.preferences.test.tsx
//
// Owns: App-level tests for the preferences surface and the
// generic ThemedCombobox UI — theme / editor appearance / UI
// style preference persistence, UI density persistence, Codex
// model-switch notice text (describeCodexModelAdjustmentNotice
// pure function), and the generic ThemedCombobox behaviours
// that App exercises (active option applied on space without
// closing the menu, off-screen selection scrolled into view
// when the menu opens).
//
// Does not own: session-lifecycle model-refresh tests,
// live-state / watchdog tests, control-panel tests.
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

describe("App preferences", () => {
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
        fetchStateDeferred.resolve(
          makeStateResponse({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-current", {
                name: "Current Session",
                preview: "Should remain visible",
              }),
            ],
          }),
        );
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

      await clickAndSettle(screen.getByRole("tab", { name: "Editor & UI" }));

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

  it("adopts a replacement-instance preferences fallback after a failed settings save", async () => {
    await withSuppressedActWarnings(async () => {
      // This flow intentionally exercises a detached async settings-save
      // rejection. The fallback fetch is resolved in act below; keep the known
      // React warning local so the assertion remains load-bearing.
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      type FetchStateResult = Awaited<ReturnType<typeof api.fetchState>>;
      const createFetchStateDeferred = () => createDeferred<FetchStateResult>();
      let initialStateDeferred: ReturnType<
        typeof createFetchStateDeferred
      > | null = null;
      let replacementFallbackDeferred: ReturnType<
        typeof createFetchStateDeferred
      > | null = null;
      let fetchStateCallCount = 0;
      let useReplacementFallback = false;
      let rejectUpdateAppSettings: (error: unknown) => void = () => {};
      const updateAppSettingsPromise = new Promise<
        Awaited<ReturnType<typeof api.updateAppSettings>>
      >((_resolve, reject) => {
        rejectUpdateAppSettings = reject;
      });
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => {
          fetchStateCallCount += 1;
          if (useReplacementFallback) {
            replacementFallbackDeferred ??= createFetchStateDeferred();
            return replacementFallbackDeferred.promise;
          }
          initialStateDeferred ??= createFetchStateDeferred();
          return initialStateDeferred.promise;
        });
      const updateAppSettingsSpy = vi
        .spyOn(api, "updateAppSettings")
        .mockImplementation(() => updateAppSettingsPromise);
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
        const currentState = makeStateResponse({
          revision: 5,
          serverInstanceId: "current-instance",
          preferences: {
            defaultCodexReasoningEffort: "medium",
            defaultClaudeApprovalMode: "ask",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [
            makeSession("session-current", {
              name: "Current Session",
              preview: "Still present after rejected fallback",
            }),
          ],
        });
        if (initialStateDeferred) {
          await act(async () => {
            initialStateDeferred?.resolve(currentState);
            await flushUiWork();
          });
        } else {
          await dispatchOpenedStateEvent(latestEventSource(), currentState);
        }

        useReplacementFallback = true;
        const fetchStateCallsBeforeSave = fetchStateCallCount;

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(screen.getByRole("tab", { name: "Codex" }));
        await selectComboboxOption("Default reasoning effort", /high/i);

        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultCodexReasoningEffort: "high",
          });
        });
        await act(async () => {
          rejectUpdateAppSettings(
            new api.ApiRequestError(
              "backend-unavailable",
              "backend restarted",
              {
                restartRequired: true,
              },
            ),
          );
          await flushUiWork();
        });
        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(
            fetchStateCallsBeforeSave + 1,
          );
        });
        const replacementState = makeStateResponse({
          revision: 6,
          serverInstanceId: "replacement-instance",
          preferences: {
            defaultCodexReasoningEffort: "low",
            defaultClaudeApprovalMode: "ask",
            defaultClaudeEffort: "default",
          },
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [
            makeSession("session-replacement", {
              name: "Replacement Session",
              preview: "Recovered preferences fallback",
            }),
          ],
        });
        await act(async () => {
          replacementFallbackDeferred?.resolve(replacementState);
          await flushUiWork();
        });
        await waitFor(() => {
          expect(
            screen.getByRole("combobox", { name: "Default reasoning effort" }),
          ).toHaveTextContent("low");
        });
        await clickAndSettle(
          screen.getByRole("button", { name: "Close dialog" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        expect(
          within(sessionList).getByText("Replacement Session"),
        ).toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        updateAppSettingsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });

  it("rejects replacement-instance preferences fallback after a non-backend settings save failure", async () => {
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      type FetchStateResult = Awaited<ReturnType<typeof api.fetchState>>;
      const fallbackDeferred = createDeferred<FetchStateResult>();
      let rejectUpdateAppSettings: (error: unknown) => void = () => {};
      const updateAppSettingsPromise = new Promise<
        Awaited<ReturnType<typeof api.updateAppSettings>>
      >((_resolve, reject) => {
        rejectUpdateAppSettings = reject;
      });
      const fetchStateSpy = vi
        .spyOn(api, "fetchState")
        .mockImplementation(() => fallbackDeferred.promise);
      const updateAppSettingsSpy = vi
        .spyOn(api, "updateAppSettings")
        .mockImplementation(() => updateAppSettingsPromise);
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
        await dispatchOpenedStateEvent(
          latestEventSource(),
          makeStateResponse({
            revision: 5,
            serverInstanceId: "current-instance",
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeApprovalMode: "ask",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-current", {
                name: "Current Session",
                preview: "Should remain visible",
              }),
            ],
          }),
        );

        await clickAndSettle(
          await screen.findByRole("button", { name: "Open preferences" }),
        );
        await clickAndSettle(screen.getByRole("tab", { name: "Codex" }));
        await selectComboboxOption("Default reasoning effort", /high/i);

        await waitFor(() => {
          expect(updateAppSettingsSpy).toHaveBeenCalledWith({
            defaultCodexReasoningEffort: "high",
          });
        });
        await act(async () => {
          rejectUpdateAppSettings(new Error("validation failed"));
          await flushUiWork();
        });
        await waitFor(() => {
          expect(fetchStateSpy).toHaveBeenCalledTimes(1);
        });
        await act(async () => {
          fallbackDeferred.resolve(
            makeStateResponse({
              revision: 6,
              serverInstanceId: "replacement-instance",
              preferences: {
                defaultCodexReasoningEffort: "low",
                defaultClaudeApprovalMode: "ask",
                defaultClaudeEffort: "default",
              },
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-replacement", {
                  name: "Replacement Session",
                  preview: "Should not adopt",
                }),
              ],
            }),
          );
          await flushUiWork();
        });

        expect(
          screen.getByRole("combobox", { name: "Default reasoning effort" }),
        ).toHaveTextContent("high");
        await clickAndSettle(
          screen.getByRole("button", { name: "Close dialog" }),
        );
        await clickAndSettle(
          await screen.findByRole("button", { name: "Sessions" }),
        );
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        expect(
          within(sessionList).queryByText("Replacement Session"),
        ).not.toBeInTheDocument();
        expect(
          within(sessionList).getByText("Current Session"),
        ).toBeInTheDocument();
      } finally {
        scrollIntoViewSpy.mockRestore();
        fetchStateSpy.mockRestore();
        updateAppSettingsSpy.mockRestore();
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
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
        fetchStateDeferred.resolve(
          makeStateResponse({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }),
        );
        await flushUiWork();
      });

      await clickAndSettle(
        await screen.findByRole("button", { name: "Open preferences" }),
      );
      await clickAndSettle(screen.getByRole("tab", { name: "Editor & UI" }));

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
        fetchStateDeferred.resolve(
          makeStateResponse({
            revision: 1,
            preferences: {
              defaultCodexReasoningEffort: "medium",
              defaultClaudeEffort: "default",
            },
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [],
          }),
        );
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
