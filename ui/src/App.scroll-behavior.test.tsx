// App.scroll-behavior.test.tsx
//
// Owns: tests for App-level scroll and layout-clamp behaviour
// - Ctrl+PageUp jump-to-top regression (cancels a pending
//   settle-to-bottom RAF), session scroll restoration (the
//   manual-message-scroll store path, the settled-scroll
//   minimum-attempts helper, the new-response scroll
//   correction, the default-scroll-to-bottom useLayoutEffect
//   branch), wheel passive/non-passive listener registration,
//   docked control-panel layout clamps (saved-layout floor,
//   control-panel pixel minimum vs generic row split clamp,
//   standalone control-surface pixel minimum, standalone
//   width -> initial dock ratio matching, and the initial
//   dock-ratio clamp when the standalone width would crowd
//   out the session pane).
//
// Does not own: control-panel integration tests (see
// App.control-panel.test.tsx), DnD tests, live-state tests,
// workspace-layout tests, session-lifecycle tests.
//
// Split out of: ui/src/App.test.tsx (Slice 9 of the App-split
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
import { upsertSessionStoreSession } from "./session-store";
import { ThemedCombobox } from "./preferences-panels";
import {
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  describeUnknownSessionModelWarning,
  resolveControlPanelWorkspaceRoot,
  resolveUnknownSessionModelSendAttempt,
} from "./session-model-utils";
import { setAppTestHooksForTests } from "./app-test-hooks";
import {
  CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX,
  CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX,
  resolveStandaloneControlPanelDockWidthRatio,
} from "./control-panel-layout";
import {
  buildControlSurfaceSessionListEntries,
  formatSessionOrchestratorGroupName,
} from "./control-surface-state";
import { collectRestoredGitDiffDocumentContentRefreshes } from "./git-diff-refresh";
import { resolveSettledScrollMinimumAttempts } from "./scroll-position";
import {
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
  requestMessageStackBottomRepin,
} from "./message-stack-scroll-sync";
import { addSessionHistoryPageDemandListener } from "./session-history-demand";
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
import type {
  AgentReadiness,
  McpElicitationRequestMessage,
  OrchestratorInstance,
  Session,
} from "./types";
import * as workspaceStorage from "./workspace-storage";
import { WORKSPACE_LAYOUT_STORAGE_KEY } from "./workspace-storage";
import type { WorkspaceState, WorkspaceTab } from "./workspace";
import type { AppTestStateResponse } from "./app-test-harness";
import {
  EventSourceMock,
  ResizeObserverMock,
  advanceTimers,
  clickAndSettle,
  createScheduledAnimationFrameMocks,
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
  withVerifiedNoReactActWarnings,
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

function scrollToTopsWithBehavior(
  scrollToMock: ReturnType<typeof vi.fn>,
  behavior: ScrollBehavior,
) {
  return scrollToMock.mock.calls.flatMap((call) => {
    const options = call[0];
    return typeof options === "object" &&
      options !== null &&
      options.behavior === behavior &&
      typeof options.top === "number"
      ? [options.top]
      : [];
  });
}

function scrollToTopsForElementWithBehavior(
  scrollToMock: ReturnType<typeof vi.fn>,
  element: HTMLElement,
  behavior: ScrollBehavior,
) {
  return scrollToMock.mock.calls.flatMap((call, index) => {
    const options = call[0];
    return scrollToMock.mock.contexts[index] === element &&
      typeof options === "object" &&
      options !== null &&
      options.behavior === behavior &&
      typeof options.top === "number"
      ? [options.top]
      : [];
  });
}

describe("App scroll behaviour", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;
  const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
  const originalCancelAnimationFrame = globalThis.cancelAnimationFrame;

  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createScheduledAnimationFrameMocks();
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

  it("cancels a pending settle-to-bottom frame when Ctrl+PageUp jumps to the top", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(
        window.navigator,
        "platform",
      );
      Object.defineProperty(window.navigator, "platform", {
        configurable: true,
        value: "Win32",
      });
      const pendingFrames = new Map<number, FrameRequestCallback>();
      let nextFrameId = 1;
      vi.stubGlobal("requestAnimationFrame", ((
        callback: FrameRequestCallback,
      ) => {
        const frameId = nextFrameId;
        nextFrameId += 1;
        pendingFrames.set(frameId, callback);
        return frameId;
      }) as typeof requestAnimationFrame);
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
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        messageStack.scrollTop = 800;
        const dequeuedBeforeCancellation = [...pendingFrames.values()];

        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "PageUp",
            code: "PageUp",
            ctrlKey: true,
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(0);
        expect(
          filterScrollToCallsAt(scrollToMock, 0, "auto").length,
        ).toBeGreaterThan(0);
        expect(cancelAnimationFrameMock).toHaveBeenCalled();

        // A browser may already have dequeued a frame when cancellation lands.
        // Its callback must still observe cancellation and leave the reader's
        // explicit navigation in place.
        for (const callback of dequeuedBeforeCancellation) {
          await act(async () => {
            callback(Date.now());
            await flushUiWork();
          });
        }

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

        // Plain Home takes the same real pane -> virtualizer user-owned seek
        // path; keep one end-to-end producer check alongside Ctrl+PageUp.
        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.keyDown(messageStack, { key: "Home", code: "Home" });
        });
        await settleAsyncUi();
        expect(messageStack.scrollTop).toBe(0);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
        if (originalPlatform) {
          Object.defineProperty(window.navigator, "platform", originalPlatform);
        } else {
          Reflect.deleteProperty(window.navigator, "platform");
        }
      }
    });
  });

  it("routes direct macOS Command+Arrow keys through bounded start and tail history", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(
        window.navigator,
        "platform",
      );
      const navigatorWithUserAgentData = window.navigator as Navigator & {
        userAgentData?: { platform?: string };
      };
      const originalUserAgentData = Object.getOwnPropertyDescriptor(
        navigatorWithUserAgentData,
        "userAgentData",
      );
      Object.defineProperty(window.navigator, "platform", {
        configurable: true,
        value: "MacIntel",
      });
      Object.defineProperty(navigatorWithUserAgentData, "userAgentData", {
        configurable: true,
        value: { platform: "macOS" },
      });
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const windowedSession = makeSession("session-1", {
        name: "Session 1",
        projectId: "project-termal",
        workdir: "/projects/termal",
        messagesLoaded: false,
        hasOlderHistory: true,
        hasNewerHistory: true,
        messageCount: 1_000,
      });
      const fetchHistorySpy = vi
        .spyOn(api, "fetchSessionHistory")
        .mockImplementation(async (_sessionId, options) => ({
          messages: windowedSession.messages,
          nextBefore: null,
          hasMore: false,
          nextAfter: options.from === "start" ? "message-tail" : null,
          hasNewer: options.from === "start",
          messageStartIndex: options.from === "start" ? 0 : 999,
          messageCount: 1_000,
          revision: options.from === "start" ? 2 : 3,
          sessionMutationStamp: options.from === "start" ? 2 : 3,
          serverInstanceId: "test-instance",
        }));
      const context = await renderAppWithProjectAndSession();

      try {
        await act(async () => {
          upsertSessionStoreSession({
            committedDraft: "",
            draftAttachments: [],
            session: windowedSession,
          });
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(fetchHistorySpy).not.toHaveBeenCalled();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "ArrowUp",
            code: "ArrowUp",
            metaKey: true,
          });
        });
        await waitFor(() => {
          expect(fetchHistorySpy).toHaveBeenCalledTimes(1);
        });
        expect(fetchHistorySpy.mock.calls[0]?.[1]).toMatchObject({
          from: "start",
        });

        fetchHistorySpy.mockClear();
        messageStack.scrollTop = 0;
        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "ArrowDown",
            code: "ArrowDown",
            metaKey: true,
          });
        });
        await waitFor(() => {
          expect(fetchHistorySpy).toHaveBeenCalledTimes(1);
        });
        expect(fetchHistorySpy.mock.calls[0]?.[1].from).toBeUndefined();
        expect(fetchHistorySpy.mock.calls[0]?.[1].before).toBeUndefined();
        expect(fetchHistorySpy.mock.calls[0]?.[1].after).toBeUndefined();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
        if (originalPlatform) {
          Object.defineProperty(window.navigator, "platform", originalPlatform);
        } else {
          Reflect.deleteProperty(window.navigator, "platform");
        }
        if (originalUserAgentData) {
          Object.defineProperty(
            navigatorWithUserAgentData,
            "userAgentData",
            originalUserAgentData,
          );
        } else {
          Reflect.deleteProperty(navigatorWithUserAgentData, "userAgentData");
        }
      }
    });
  });

  it("jumps to the top on Ctrl+Shift+PageUp from the composer textarea", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(
        window.navigator,
        "platform",
      );
      Object.defineProperty(window.navigator, "platform", {
        configurable: true,
        value: "Win32",
      });
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, { target: { value: "hello world" } });
        });

        messageStack.scrollTop = 800;
        composer.focus();
        composer.setSelectionRange(
          composer.value.length,
          composer.value.length,
        );

        await act(async () => {
          fireEvent.keyDown(composer, {
            key: "PageUp",
            code: "PageUp",
            ctrlKey: true,
            shiftKey: true,
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(0);
        expect(
          filterScrollToCallsAt(scrollToMock, 0, "auto").length,
        ).toBeGreaterThan(0);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
        if (originalPlatform) {
          Object.defineProperty(window.navigator, "platform", originalPlatform);
        } else {
          Reflect.deleteProperty(window.navigator, "platform");
        }
      }
    });
  });

  it("keeps focused transcript selection-extension keys browser-owned", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        messageStack.focus();
        let ctrlShiftHomeContinues = false;
        let shiftPageUpContinues = false;
        await act(async () => {
          ctrlShiftHomeContinues = fireEvent.keyDown(messageStack, {
            key: "Home",
            code: "Home",
            ctrlKey: true,
            shiftKey: true,
          });
          shiftPageUpContinues = fireEvent.keyDown(messageStack, {
            key: "PageUp",
            code: "PageUp",
            shiftKey: true,
          });
        });
        await settleAsyncUi();

        expect(ctrlShiftHomeContinues).toBe(true);
        expect(shiftPageUpContinues).toBe(true);
        expect(messageStack.scrollTop).toBe(800);
        expect(filterScrollToCallsAt(scrollToMock, 0, "auto")).toEqual([]);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps pane boundary keys inside transcript-native controls", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const elicitationMessage: McpElicitationRequestMessage = {
        id: "message-mcp-scroll-control",
        type: "mcpElicitationRequest",
        author: "assistant",
        timestamp: "10:05",
        title: "Codex needs MCP input",
        detail: "deployment-helper requested structured input.",
        state: "pending",
        request: {
          threadId: "thread-1",
          turnId: "turn-1",
          serverName: "deployment-helper",
          mode: "form",
          message: "Choose the replica count.",
          requestedSchema: {
            type: "object",
            properties: {
              replicas: {
                type: "integer",
                title: "Replicas",
              },
              note: {
                type: "string",
                title: "Deployment note",
              },
            },
            required: ["replicas", "note"],
          },
        },
      };

      try {
        await act(async () => {
          upsertSessionStoreSession({
            committedDraft: "",
            draftAttachments: [],
            session: makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              messages: [elicitationMessage],
              messagesLoaded: true,
              messageCount: 1,
            }),
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        const numberInput = await screen.findByRole("spinbutton");
        const textInput = messageStack.querySelector(
          'input.user-input-text[type="text"]',
        );
        if (!(textInput instanceof HTMLInputElement)) {
          throw new Error("MCP text input not found");
        }

        messageStack.scrollTop = 800;
        const intentListener = vi.fn();
        messageStack.addEventListener(
          MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
          intentListener,
        );
        let shiftedPageUpContinues = false;
        await act(async () => {
          fireEvent.change(textInput, { target: { value: "selection text" } });
          textInput.focus();
          textInput.setSelectionRange(4, 4);
          shiftedPageUpContinues = fireEvent.keyDown(textInput, {
            key: "PageUp",
            code: "PageUp",
            shiftKey: true,
          });
        });
        await settleAsyncUi();
        expect(shiftedPageUpContinues).toBe(true);
        expect(intentListener).not.toHaveBeenCalled();
        expect(messageStack.scrollTop).toBe(800);

        numberInput.focus();
        let homeContinues = false;
        let endContinues = false;
        let pageUpContinues = true;
        await act(async () => {
          homeContinues = fireEvent.keyDown(numberInput, {
            key: "Home",
            code: "Home",
          });
          endContinues = fireEvent.keyDown(numberInput, {
            key: "End",
            code: "End",
          });
          pageUpContinues = fireEvent.keyDown(numberInput, {
            key: "PageUp",
            code: "PageUp",
          });
        });
        await settleAsyncUi();

        expect(homeContinues).toBe(true);
        expect(endContinues).toBe(true);
        expect(pageUpContinues).toBe(false);
        expect(intentListener).toHaveBeenCalledTimes(1);
        expect(
          (intentListener.mock.calls[0]?.[0] as CustomEvent).detail,
        ).toMatchObject({ direction: "up", scrollKind: "page_jump" });
        expect(messageStack.scrollTop).toBe(630);
        expect(filterScrollToCallsAt(scrollToMock, 0, "auto")).toEqual([]);
        messageStack.removeEventListener(
          MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
          intentListener,
        );
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps plain PageDown inside the composer textarea when the caret is not at the start", async () => {
    await withVerifiedNoReactActWarnings(async () => {
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
        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, { target: { value: "hello world" } });
        });

        messageStack.scrollTop = 800;
        composer.focus();
        composer.setSelectionRange(5, 5);

        await act(async () => {
          fireEvent.keyDown(composer, {
            key: "PageDown",
            code: "PageDown",
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(800);
        expect(filterScrollToCallsAt(scrollToMock, 0, "auto")).toEqual([]);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("requests current-session history after pane-captured nested Page keys", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const session1 = makeSession("session-1", {
        name: "Session 1",
        projectId: "project-termal",
        workdir: "/projects/termal",
        hasOlderHistory: true,
        messages: [
          {
            id: "session-1-resident-message",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Resident historical window",
          },
        ],
      });
      const session2 = makeSession("session-2", {
        name: "Session 2",
        projectId: "project-termal",
        workdir: "/projects/termal",
        hasNewerHistory: true,
        messages: [
          {
            id: "session-2-resident-message",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Resident historical window",
          },
        ],
      });
      const fetchHistorySpy = vi
        .spyOn(api, "fetchSessionHistory")
        .mockImplementation(async (sessionId) => ({
          messages:
            sessionId === "session-1" ? session1.messages : session2.messages,
          nextBefore: null,
          hasMore: false,
          nextAfter: null,
          hasNewer: false,
          messageStartIndex: 0,
          messageCount: session2.messages.length,
          revision: 2,
          sessionMutationStamp: 2,
          serverInstanceId: "test-instance",
        }));
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
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
            sessions: [session1, session2],
          });
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-nested-page-fallback-session-switch`;
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-nested-page-fallback-session-switch",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        layoutStorageKey,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              type: "pane",
              paneId: "pane-session",
            },
            panes: [
              {
                id: "pane-session",
                tabs: [
                  {
                    id: "tab-session-1",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-session-2",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-session-1",
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
      const historyDemandListener = vi.fn();
      const removeHistoryDemandListener =
        addSessionHistoryPageDemandListener(historyDemandListener);

      try {
        await renderApp();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) =>
            within(candidate).queryByRole("tab", { name: "Session 1" }),
          );
        if (!tablist) {
          throw new Error("Session pane tablist not found");
        }
        const session1Tab = within(tablist).getByRole("tab", {
          name: "Session 1",
        });
        const session2Tab = within(tablist).getByRole("tab", {
          name: "Session 2",
        });

        await clickAndSettle(session1Tab);
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Active message stack not found");
        }

        const session1Composer = await screen.findByLabelText(
          "Message Session 1",
        );
        if (!(session1Composer instanceof HTMLTextAreaElement)) {
          throw new Error("Session 1 composer not found");
        }
        session1Composer.focus();
        session1Composer.setSelectionRange(0, 0);
        const stopSession1Propagation = (event: KeyboardEvent) => {
          event.stopPropagation();
        };
        session1Composer.addEventListener("keydown", stopSession1Propagation);
        messageStack.scrollTop = 0;
        await act(async () => {
          try {
            fireEvent.keyDown(session1Composer, {
              key: "PageUp",
              code: "PageUp",
            });
          } finally {
            session1Composer.removeEventListener(
              "keydown",
              stopSession1Propagation,
            );
          }
        });
        await waitFor(() => {
          expect(historyDemandListener).toHaveBeenCalledWith({
            direction: "older",
            sessionId: "session-1",
          });
        });
        expect(fetchHistorySpy).not.toHaveBeenCalled();
        fetchHistorySpy.mockClear();

        messageStack.scrollTop = 150;
        act(() => {
          fireEvent.scroll(messageStack);
        });

        await clickAndSettle(session2Tab);
        expect(messageStack.scrollTop).toBe(800);
        messageStack.scrollTop = 800;
        act(() => {
          fireEvent.scroll(messageStack);
        });

        const composer = await screen.findByLabelText("Message Session 2");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Session 2 composer not found");
        }
        await act(async () => {
          fireEvent.change(composer, { target: { value: "hello world" } });
        });
        composer.focus();
        composer.setSelectionRange(0, 0);
        const stopPropagation = (event: KeyboardEvent) => {
          event.stopPropagation();
        };
        composer.addEventListener("keydown", stopPropagation);

        await act(async () => {
          try {
            fireEvent.keyDown(composer, {
              key: "PageDown",
              code: "PageDown",
            });
          } finally {
            composer.removeEventListener("keydown", stopPropagation);
          }
        });
        await settleAsyncUi();

        await waitFor(() => {
          expect(fetchHistorySpy).toHaveBeenCalledTimes(1);
        });
        expect(fetchHistorySpy.mock.calls[0]?.[0]).toBe("session-2");
        expect(messageStack.scrollTop).toBe(800);

        const currentTablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) =>
            within(candidate).queryByRole("tab", {
              name: "Session 2",
              selected: true,
            }),
          );
        if (!currentTablist) {
          throw new Error("Session pane tablist not found after tab switch");
        }
        await clickAndSettle(
          within(currentTablist).getByRole("tab", { name: "Session 1" }),
        );
        expect(messageStack.scrollTop).toBe(150);
      } finally {
        removeHistoryDemandListener();
        restoreScrollGeometry();
      }
    });
  });

  it("restores an attached virtualized session before the first frame after a tab switch", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        // Keep the DOM bottom in the same neighborhood as the virtualizer's
        // estimated layout for eighty short assistant messages. A tiny fake
        // scrollHeight would describe the physical bottom as the middle of the
        // estimated page model and make the mounted-range assertion invalid.
        scrollHeight: 10_000,
      });
      const makeVirtualizedMessages = (
        sessionId: string,
      ): Session["messages"] =>
        Array.from({ length: 80 }, (_, index) => ({
          id: `${sessionId}-message-${index}`,
          type: "text" as const,
          timestamp: `10:${String(index).padStart(2, "0")}`,
          author: "assistant" as const,
          text: `${sessionId} response ${index}`,
        }));
      const session1Messages = makeVirtualizedMessages("session-1");
      const session2Messages = makeVirtualizedMessages("session-2");
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
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
                messages: session1Messages,
              }),
              makeSession("session-2", {
                name: "Session 2",
                projectId: "project-termal",
                workdir: "/projects/termal",
                messages: session2Messages,
              }),
            ],
          });
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      const workspaceId = "test-virtualized-tab-first-paint";
      window.history.replaceState(
        window.history.state,
        "",
        `/?workspace=${workspaceId}`,
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceId}`,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              type: "pane",
              paneId: "pane-session",
            },
            panes: [
              {
                id: "pane-session",
                tabs: [
                  {
                    id: "tab-session-1",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-session-2",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-session-1",
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

      try {
        await renderApp();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        const tablist = screen
          .getAllByRole("tablist", { name: "Tile tabs" })
          .find((candidate) =>
            within(candidate).queryByRole("tab", { name: "Session 1" }),
          );
        if (!tablist) {
          throw new Error("Session pane tablist not found");
        }
        const session2Tab = within(tablist).getByRole("tab", {
          name: "Session 2",
        });
        await clickAndSettle(session2Tab);

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Active message stack not found");
        }
        let session2GeometryShiftPx = 0;
        const originalGetBoundingClientRect =
          HTMLElement.prototype.getBoundingClientRect;
        vi.spyOn(
          HTMLElement.prototype,
          "getBoundingClientRect",
        ).mockImplementation(function (this: HTMLElement) {
          if (this === messageStack) {
            return {
              bottom: 200,
              height: 200,
              left: 0,
              right: 800,
              top: 0,
              width: 800,
              x: 0,
              y: 0,
              toJSON: () => ({}),
            } as DOMRect;
          }
          const messageId = this.dataset.messageId;
          const session2Match = messageId?.match(/^session-2-message-(\d+)$/);
          if (
            session2Match &&
            this.classList.contains("virtualized-message-slot")
          ) {
            const messageIndex = Number(session2Match[1]);
            const top =
              messageIndex * 125 +
              20 +
              session2GeometryShiftPx -
              messageStack.scrollTop;
            return {
              bottom: top + 100,
              height: 100,
              left: 0,
              right: 800,
              top,
              width: 800,
              x: 0,
              y: top,
              toJSON: () => ({}),
            } as DOMRect;
          }
          return originalGetBoundingClientRect.call(this);
        });
        expect(
          messageStack.querySelector(".virtualized-message-list"),
        ).not.toBeNull();
        expect(messageStack.scrollTop).toBe(9800);

        const bodyOwnedIntentListener = vi.fn();
        messageStack.addEventListener(
          MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
          bodyOwnedIntentListener,
        );
        const nonInteractiveMessageSlot = messageStack.querySelector(
          ".virtualized-message-slot",
        );
        expect(nonInteractiveMessageSlot).not.toBeNull();

        act(() => {
          // A body-targeted key is not transcript input until the reader has
          // interacted with this stack. This prevents sidebar/dialog keys from
          // silently detaching the active transcript.
          fireEvent.mouseDown(document.body);
          fireEvent.keyDown(document.body, { key: "ArrowUp" });
        });
        expect(bodyOwnedIntentListener).not.toHaveBeenCalled();

        act(() => {
          // Wheel/touch movement detaches the viewport but does not make the
          // transcript Chromium's body-keyboard scroll owner.
          fireEvent.wheel(messageStack, { deltaY: -20 });
          fireEvent.keyDown(document.body, { key: "ArrowUp" });
        });
        expect(bodyOwnedIntentListener).not.toHaveBeenCalled();

        act(() => {
          // Clicking non-focusable transcript content can leave focus on the
          // document body even though Chromium routes ArrowUp to this scroll
          // container. Exercise that body-owned intent before the browser's
          // native scroll event, then keep the existing middle-transcript
          // geometry that makes the A -> B -> A anchor assertion exact.
          fireEvent.mouseDown(nonInteractiveMessageSlot!);
          messageStack.scrollTop = 5001;
          fireEvent.keyDown(document.body, { key: "ArrowUp" });
          messageStack.scrollTop = 5000;
          fireEvent.scroll(messageStack);
        });
        expect(bodyOwnedIntentListener).toHaveBeenCalledTimes(1);
        act(() => {
          // Once an outside pointer interaction revokes ownership, another
          // body-targeted key must not publish transcript intent.
          fireEvent.mouseDown(document.body);
          fireEvent.keyDown(document.body, { key: "ArrowUp" });
        });
        expect(bodyOwnedIntentListener).toHaveBeenCalledTimes(1);
        messageStack.removeEventListener(
          MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
          bodyOwnedIntentListener,
        );
        // Commit the keyboard/detach update before advancing timers. Keeping
        // flushUiWork inside one async act lets jsdom run an old activation
        // timeout before React can apply the cancellation, an ordering the
        // browser cannot produce between separate input and timer tasks.
        expect(messageStack.scrollTop).toBeLessThan(9800);
        await settleAsyncUi();
        const detachedScrollTop = messageStack.scrollTop;
        expect(detachedScrollTop).toBeLessThan(9800);
        const detachedAnchor = Array.from(
          messageStack.querySelectorAll<HTMLElement>(
            ".virtualized-message-slot[data-message-id]",
          ),
        ).find((slot) => {
          const rect = slot.getBoundingClientRect();
          return rect.bottom > 0 && rect.top < 200;
        });
        expect(detachedAnchor).toBeDefined();
        const detachedAnchorId = detachedAnchor?.dataset.messageId;
        const detachedAnchorOffset =
          detachedAnchor?.getBoundingClientRect().top;

        let nextFrameId = 1;
        const pendingFrames = new Map<number, FrameRequestCallback>();
        vi.stubGlobal(
          "requestAnimationFrame",
          vi.fn((callback: FrameRequestCallback) => {
            const frameId = nextFrameId;
            nextFrameId += 1;
            pendingFrames.set(frameId, callback);
            return frameId;
          }),
        );
        vi.stubGlobal(
          "cancelAnimationFrame",
          vi.fn((frameId: number) => pendingFrames.delete(frameId)),
        );

        const scrollKinds: Array<string | undefined> = [];
        const recordScrollWrite = (event: Event) => {
          scrollKinds.push(
            event instanceof CustomEvent
              ? (event.detail as { scrollKind?: string } | undefined)
                  ?.scrollKind
              : undefined,
          );
        };
        messageStack.addEventListener(
          MESSAGE_STACK_SCROLL_WRITE_EVENT,
          recordScrollWrite,
        );
        try {
          const currentTablist = screen
            .getAllByRole("tablist", { name: "Tile tabs" })
            .find((candidate) =>
              within(candidate).queryByRole("tab", {
                name: "Session 2",
                selected: true,
              }),
            );
          if (!currentTablist) {
            throw new Error("Active Session 2 tablist not found");
          }

          await act(async () => {
            fireEvent.click(
              within(currentTablist).getByRole("tab", { name: "Session 1" }),
            );
          });

          const restoredMessageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          expect(restoredMessageStack).toBe(messageStack);
          expect(messageStack.scrollTop).toBe(9800);
          expect(scrollKinds).toContain("bottom_pin");
          expect(pendingFrames.size).toBeGreaterThan(0);
          expect(
            messageStack.querySelector(
              '[data-message-id="session-1-message-79"]',
            ),
          ).not.toBeNull();

          const firstFrameCallbacks = Array.from(pendingFrames.values());
          pendingFrames.clear();
          await act(async () => {
            const firstFrameTime = performance.now() + 1000 / 60;
            for (const callback of firstFrameCallbacks) {
              callback(firstFrameTime);
            }
            await flushUiWork();
          });
          expect(messageStack.scrollTop).toBe(9800);
          expect(
            scrollKinds.filter((scrollKind) => scrollKind === "bottom_pin")
              .length,
          ).toBeGreaterThanOrEqual(1);

          await act(async () => {
            upsertSessionStoreSession({
              committedDraft: "",
              draftAttachments: [],
              session: makeSession("session-2", {
                name: "Session 2",
                projectId: "project-termal",
                workdir: "/projects/termal",
                messages: [
                  ...session2Messages,
                  {
                    id: "session-2-message-80",
                    type: "text",
                    timestamp: "11:20",
                    author: "assistant",
                    text: "Session 2 response received while inactive",
                  },
                ],
              }),
            });
            await flushUiWork();
          });

          // Session 2's virtualizer remounts when its tab is reactivated. Its
          // fresh estimates and measurements can shift document-space pixels
          // above the reader, so restoring only the old absolute scrollTop
          // would display a different part of the conversation.
          session2GeometryShiftPx = 400;
          scrollKinds.length = 0;
          const session1Tablist = screen
            .getAllByRole("tablist", { name: "Tile tabs" })
            .find((candidate) =>
              within(candidate).queryByRole("tab", {
                name: "Session 1",
                selected: true,
              }),
            );
          if (!session1Tablist) {
            throw new Error("Active Session 1 tablist not found");
          }
          await act(async () => {
            fireEvent.click(
              within(session1Tablist).getByRole("tab", {
                name: "Session 2",
              }),
            );
          });

          expect(messageStack.scrollTop).toBe(detachedScrollTop + 400);
          expect(scrollKinds).toContain("position_restore");
          expect(scrollKinds).not.toContain("bottom_pin");
          expect(scrollKinds).not.toContain("bottom_boundary");
          const restoredAnchor = messageStack.querySelector(
            `[data-message-id="${detachedAnchorId}"]`,
          );
          expect(restoredAnchor).not.toBeNull();
          expect(restoredAnchor?.getBoundingClientRect().top).toBe(
            detachedAnchorOffset,
          );
        } finally {
          messageStack.removeEventListener(
            MESSAGE_STACK_SCROLL_WRITE_EVENT,
            recordScrollWrite,
          );
        }
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("restores a detached session before paint when its real tab drag lands in another pane", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 10_000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const makeMessages = (sessionId: string): Session["messages"] =>
        Array.from({ length: 80 }, (_, index) => ({
          id: `${sessionId}-message-${index}`,
          type: "text" as const,
          timestamp: `10:${String(index).padStart(2, "0")}`,
          author: "assistant" as const,
          text: `${sessionId} response ${index}`,
        }));
      const session1OverviewResolvers: Array<(response: Response) => void> = [];
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [
                {
                  id: "project-termal",
                  name: "TermAl",
                  rootPath: "/projects/termal",
                },
              ],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Session 1",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                  messages: makeMessages("session-1"),
                }),
                makeSession("session-2", {
                  name: "Session 2",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                  messages: makeMessages("session-2"),
                }),
                makeSession("session-3", {
                  name: "Session 3",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                  messages: makeMessages("session-3"),
                }),
              ],
            }),
          );
        }
        const overviewMatch = requestUrl.pathname.match(
          /^\/api\/sessions\/([^/]+)\/overview$/,
        );
        if (overviewMatch) {
          const sessionId = decodeURIComponent(overviewMatch[1] ?? "");
          const response = jsonResponse({
            sessionId,
            messageCount: 80,
            sessionMutationStamp: 1,
            buckets: [{ c: 80, k: "text", u: 8, m: false }],
            markers: [],
            latestPosition: 79,
          });
          if (sessionId === "session-1") {
            return new Promise<Response>((resolve) => {
              session1OverviewResolvers.push(resolve);
            });
          }
          return response;
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-tab-drag-scroll-restore`;
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-tab-drag-scroll-restore",
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
              ratio: 0.5,
              first: { type: "pane", paneId: "pane-source" },
              second: { type: "pane", paneId: "pane-target" },
            },
            panes: [
              {
                id: "pane-source",
                tabs: [
                  {
                    id: "tab-session-1",
                    kind: "session",
                    sessionId: "session-1",
                  },
                  {
                    id: "tab-session-3",
                    kind: "session",
                    sessionId: "session-3",
                  },
                ],
                activeTabId: "tab-session-1",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-target",
                tabs: [
                  {
                    id: "tab-session-2",
                    kind: "session",
                    sessionId: "session-2",
                  },
                ],
                activeTabId: "tab-session-2",
                activeSessionId: "session-2",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-source",
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
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();
        expect(session1OverviewResolvers).toHaveLength(1);

        const session1Tab = screen.getByRole("tab", { name: "Session 1" });
        const session2Tab = screen.getByRole("tab", { name: "Session 2" });
        const sourcePane = session1Tab.closest(".workspace-pane");
        const targetPane = session2Tab.closest(".workspace-pane");
        const sourceStack = sourcePane?.querySelector(".message-stack");
        const targetStack = targetPane?.querySelector(".message-stack");
        const targetTablist = session2Tab.closest('[role="tablist"]');
        if (
          !(sourceStack instanceof HTMLElement) ||
          !(targetStack instanceof HTMLElement) ||
          !(targetTablist instanceof HTMLElement)
        ) {
          throw new Error(
            "Expected both session panes and the target tab rail",
          );
        }

        sourceStack.scrollTop = 5_001;
        act(() => {
          fireEvent.wheel(sourceStack, { deltaY: -1 });
        });
        expect(sourceStack.scrollTop).toBe(5_000);

        let pendingNativeSmoothTarget: number | null = 9_800;
        const targetScrollTo = vi.fn(
          (optionsOrX?: ScrollToOptions | number, y?: number) => {
            const options =
              typeof optionsOrX === "object" && optionsOrX !== null
                ? optionsOrX
                : { top: y ?? 0 };
            if (options.behavior === "auto") {
              pendingNativeSmoothTarget = null;
            }
            if (typeof options.top === "number") {
              targetStack.scrollTop = options.top;
            }
          },
        );
        targetStack.scrollTo = targetScrollTo as typeof targetStack.scrollTo;
        const targetScrollKinds: Array<string | undefined> = [];
        targetStack.addEventListener(
          MESSAGE_STACK_SCROLL_WRITE_EVENT,
          (event) => {
            targetScrollKinds.push(
              event instanceof CustomEvent
                ? (event.detail as { scrollKind?: string } | undefined)
                    ?.scrollKind
                : undefined,
            );
          },
        );
        scrollToMock.mockClear();

        const dragGrip = within(session1Tab).getByRole("button", {
          name: "Drag Session 1",
        });
        const dataTransfer = createDragDataTransfer();
        await act(async () => {
          fireEvent.dragStart(dragGrip, { dataTransfer });
          fireEvent.dragEnter(targetTablist, { dataTransfer });
          fireEvent.dragOver(targetTablist, { dataTransfer });
          fireEvent.drop(targetTablist, { clientX: 0, dataTransfer });
          fireEvent.dragEnd(dragGrip, { dataTransfer });
        });
        await settleAsyncUi();
        expect(session1OverviewResolvers).toHaveLength(2);

        const movedSessionTab = screen.getByRole("tab", {
          name: "Session 1",
          selected: true,
        });
        const movedPane = movedSessionTab.closest(".workspace-pane");
        const movedStack = movedPane?.querySelector(".message-stack");
        expect(movedPane).toHaveAttribute(
          "data-workspace-pane-id",
          "pane-target",
        );
        expect(movedStack).toBe(targetStack);
        if (pendingNativeSmoothTarget !== null) {
          targetStack.scrollTop = pendingNativeSmoothTarget;
        }
        expect(targetStack.scrollTop).toBe(5_000);
        expect(movedStack).not.toHaveClass("is-tail-following");
        expect(targetScrollTo).toHaveBeenCalledWith({
          top: 5_000,
          behavior: "auto",
        });
        expect(targetScrollKinds).toContain("position_restore");
        expect(targetScrollKinds).not.toContain("bottom_pin");

        targetScrollKinds.length = 0;
        session1OverviewResolvers[1]?.(
          jsonResponse({
            sessionId: "session-1",
            messageCount: 80,
            sessionMutationStamp: 1,
            buckets: [{ c: 80, k: "text", u: 8, m: false }],
            markers: [],
            latestPosition: 79,
          }),
        );
        await settleAsyncUi();
        await waitFor(() =>
          expect(
            movedPane?.querySelector(".conversation-with-overview"),
          ).not.toBeNull(),
        );
        expect(targetStack.scrollTop).toBe(5_000);
        expect(targetScrollKinds).not.toContain("bottom_pin");
        expect(targetScrollKinds).not.toContain("position_restore");

        targetStack.scrollTop = 4_001;
        act(() => {
          fireEvent.wheel(targetStack, { deltaY: -1 });
        });
        expect(targetStack.scrollTop).toBe(4_000);

        const remainingSessionTab = screen.getByRole("tab", {
          name: "Session 3",
        });
        const remainingPane = remainingSessionTab.closest(".workspace-pane");
        if (!(remainingPane instanceof HTMLElement)) {
          throw new Error("Expected the source pane to survive the first move");
        }
        const movedDragGrip = within(movedSessionTab).getByRole("button", {
          name: "Drag Session 1",
        });
        const dispatchEventSpy = vi.spyOn(
          HTMLElement.prototype,
          "dispatchEvent",
        );
        const edgeDataTransfer = createDragDataTransfer();
        act(() => {
          fireEvent.dragStart(movedDragGrip, {
            dataTransfer: edgeDataTransfer,
          });
        });
        const leftDropZone = remainingPane.querySelector(
          ".pane-drop-zone-left",
        );
        if (!(leftDropZone instanceof HTMLElement)) {
          throw new Error("Expected the real pane-edge drop overlay");
        }
        await act(async () => {
          fireEvent.dragEnter(leftDropZone, {
            dataTransfer: edgeDataTransfer,
          });
          fireEvent.dragOver(leftDropZone, {
            dataTransfer: edgeDataTransfer,
          });
          fireEvent.drop(leftDropZone, { dataTransfer: edgeDataTransfer });
          fireEvent.dragEnd(movedDragGrip, {
            dataTransfer: edgeDataTransfer,
          });
        });
        await settleAsyncUi();
        expect(session1OverviewResolvers).toHaveLength(3);

        const edgeMovedSessionTab = screen.getByRole("tab", {
          name: "Session 1",
          selected: true,
        });
        const edgeMovedPane = edgeMovedSessionTab.closest(".workspace-pane");
        const edgeMovedStack = edgeMovedPane?.querySelector(".message-stack");
        expect(edgeMovedPane).not.toHaveAttribute(
          "data-workspace-pane-id",
          "pane-source",
        );
        expect(edgeMovedPane).not.toHaveAttribute(
          "data-workspace-pane-id",
          "pane-target",
        );
        expect(edgeMovedStack).toBeInstanceOf(HTMLElement);
        const edgeScrollKinds = dispatchEventSpy.mock.calls.flatMap(
          (call, index) => {
            const event = call[0];
            return dispatchEventSpy.mock.contexts[index] === edgeMovedStack &&
              event instanceof CustomEvent &&
              event.type === MESSAGE_STACK_SCROLL_WRITE_EVENT
              ? [
                  (event.detail as { scrollKind?: string } | undefined)
                    ?.scrollKind,
                ]
              : [];
          },
        );
        expect(edgeScrollKinds).toContain("position_restore");
        expect(edgeScrollKinds).not.toContain("bottom_pin");
        const edgeScrollToCalls = scrollToMock.mock.calls.filter(
          (_call, index) =>
            scrollToMock.mock.contexts[index] === edgeMovedStack,
        );
        expect(edgeScrollToCalls.length).toBeGreaterThan(0);
        expect(
          edgeScrollToCalls.every((call) => {
            const options = call[0];
            return (
              typeof options === "object" &&
              options?.top === 4_000 &&
              options.behavior === "auto"
            );
          }),
        ).toBe(true);
        expect((edgeMovedStack as HTMLElement).scrollTop).toBe(4_000);
        expect(edgeMovedStack).not.toHaveClass("is-tail-following");

        session1OverviewResolvers[2]?.(
          jsonResponse({
            sessionId: "session-1",
            messageCount: 80,
            sessionMutationStamp: 1,
            buckets: [{ c: 80, k: "text", u: 8, m: false }],
            markers: [],
            latestPosition: 79,
          }),
        );
        await settleAsyncUi();
        await waitFor(() =>
          expect(
            edgeMovedPane?.querySelector(".conversation-with-overview"),
          ).not.toBeNull(),
        );
        const edgeScrollKindsAfterOverview =
          dispatchEventSpy.mock.calls.flatMap((call, index) => {
            const event = call[0];
            return dispatchEventSpy.mock.contexts[index] === edgeMovedStack &&
              event instanceof CustomEvent &&
              event.type === MESSAGE_STACK_SCROLL_WRITE_EVENT
              ? [
                  (event.detail as { scrollKind?: string } | undefined)
                    ?.scrollKind,
                ]
              : [];
          });
        expect(edgeScrollKindsAfterOverview).not.toContain("bottom_pin");
        expect((edgeMovedStack as HTMLElement).scrollTop).toBe(4_000);

        const freshSessionTab = screen.getByRole("tab", {
          name: "Session 3",
        });
        const freshDragGrip = within(freshSessionTab).getByRole("button", {
          name: "Drag Session 3",
        });
        const freshDataTransfer = createDragDataTransfer();
        act(() => {
          fireEvent.dragStart(freshDragGrip, {
            dataTransfer: freshDataTransfer,
          });
        });
        const rightDropZone = edgeMovedPane?.querySelector(
          ".pane-drop-zone-right",
        );
        if (!(rightDropZone instanceof HTMLElement)) {
          throw new Error("Expected a pane edge for the fresh-session move");
        }
        await act(async () => {
          fireEvent.dragEnter(rightDropZone, {
            dataTransfer: freshDataTransfer,
          });
          fireEvent.dragOver(rightDropZone, {
            dataTransfer: freshDataTransfer,
          });
          fireEvent.drop(rightDropZone, { dataTransfer: freshDataTransfer });
          fireEvent.dragEnd(freshDragGrip, {
            dataTransfer: freshDataTransfer,
          });
        });
        await settleAsyncUi();

        const freshMovedTab = screen.getByRole("tab", {
          name: "Session 3",
          selected: true,
        });
        const freshMovedPane = freshMovedTab.closest(".workspace-pane");
        const freshMovedStack = freshMovedPane?.querySelector(".message-stack");
        expect(freshMovedStack).toBeInstanceOf(HTMLElement);
        expect((freshMovedStack as HTMLElement).scrollTop).toBe(9_800);
        expect(freshMovedStack).toHaveClass("is-tail-following");
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("ignores nested editable PageDown targets outside the active pane", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });

      try {
        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          await settleAsyncUi();

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          if (!(messageStack instanceof HTMLElement)) {
            throw new Error("Active message stack not found");
          }

          messageStack.scrollTop = 320;
          act(() => {
            fireEvent.scroll(messageStack);
          });

          const externalTextarea = document.createElement("textarea");
          externalTextarea.value = "outside";
          document.body.appendChild(externalTextarea);
          externalTextarea.focus();
          externalTextarea.setSelectionRange(0, 0);

          try {
            await act(async () => {
              fireEvent.keyDown(externalTextarea, {
                key: "PageDown",
                code: "PageDown",
              });
            });
            await settleAsyncUi();
          } finally {
            externalTextarea.remove();
          }

          expect(messageStack.scrollTop).toBe(320);
        } finally {
          teardown();
        }
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("pages the session transcript by 85% of the viewport on plain PageDown", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        scrollToMock.mockClear();
        messageStack.scrollTop = 400;

        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "PageDown",
            code: "PageDown",
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(570);
        expect(
          scrollToTopsForElementWithBehavior(
            scrollToMock,
            messageStack,
            "auto",
          ),
        ).toContain(570);

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

        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("pages the session transcript upward by 85% of the viewport on plain PageUp", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        scrollToMock.mockClear();
        messageStack.scrollTop = 800;

        await act(async () => {
          fireEvent.keyDown(messageStack, {
            key: "PageUp",
            code: "PageUp",
          });
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(630);
        expect(
          scrollToTopsForElementWithBehavior(
            scrollToMock,
            messageStack,
            "auto",
          ),
        ).toContain(630);

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

        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps a detached viewport stable while a send is in flight", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const pendingSend = createDeferred<Response>();
      const baseState = {
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
            preview: "Latest user prompt.",
            messages: [
              {
                id: "message-user-1",
                type: "text",
                timestamp: "10:01",
                author: "you",
                text: "Latest user prompt.",
              },
            ],
          }),
        ],
      };

      context.fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(baseState);
        }
        if (requestUrl.pathname === "/api/sessions/session-1/messages") {
          return pendingSend.promise;
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        act(() => {
          fireEvent.wheel(messageStack, { deltaY: -800 });
          messageStack.scrollTop = 0;
          fireEvent.scroll(messageStack);
        });

        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Follow this prompt" },
          });
        });

        scrollToMock.mockClear();

        await act(async () => {
          fireEvent.click(screen.getByRole("button", { name: "Send" }));
          await Promise.resolve();
        });
        await settleAsyncUi();

        expect(scrollToTopsWithBehavior(scrollToMock, "auto")).toHaveLength(0);
        expect(messageStack.scrollTop).toBe(0);
        expect(
          screen.getByRole("button", { name: "New activity" }),
        ).toBeInTheDocument();

        context.cleanup();
        await flushUiWork();
        expect(scrollToTopsWithBehavior(scrollToMock, "auto")).toHaveLength(0);
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("keeps a growing near-bottom send inside one live follow", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const pendingSend = createDeferred<Response>();
      const baseState = {
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
            preview: "Latest user prompt.",
            messages: [
              {
                id: "message-user-1",
                type: "text",
                timestamp: "10:01",
                author: "you",
                text: "Latest user prompt.",
              },
            ],
          }),
        ],
      };

      context.fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(baseState);
        }
        if (requestUrl.pathname === "/api/sessions/session-1/messages") {
          return pendingSend.promise;
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        // Establish an attached near-bottom position through a browser clamp,
        // not through an unannounced upward frame (which is reader movement).
        scrollHeight = 960;
        messageStack.scrollTop = 760;
        act(() => {
          fireEvent.scroll(messageStack);
        });
        expect(messageStack).toHaveClass("is-tail-following");

        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Near-bottom prompt" },
          });
        });

        scrollToMock.mockClear();

        await act(async () => {
          fireEvent.click(screen.getByRole("button", { name: "Send" }));
          await Promise.resolve();
        });

        scrollHeight = 1120;
        await settleAsyncUi();

        const followedTops = scrollToTopsWithBehavior(scrollToMock, "auto");
        expect(followedTops.length).toBeGreaterThan(0);
        expect(Math.max(...followedTops)).toBeGreaterThan(760);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("makes room when the first agent card appears above the live turn", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const baseState = {
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
            preview: "Current response.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(messageStack).toHaveClass("is-tail-following");
        expect(
          Math.max(...scrollToTopsWithBehavior(scrollToMock, "auto")),
        ).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("does not consume the live-turn bottom-follow edge while its pane is inactive", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [
                {
                  id: "project-termal",
                  name: "TermAl",
                  rootPath: "/projects/termal",
                },
              ],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Session 1",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                  preview: "Current response.",
                  messages,
                }),
              ],
            }),
          );
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-waiting-indicator-inactive-edge`;
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-waiting-indicator-inactive-edge",
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
              ratio: 0.35,
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
            activePaneId: "pane-control",
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
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Session message stack not found");
        }
        const sessionPane = messageStack.closest(".workspace-pane");
        if (!(sessionPane instanceof HTMLElement)) {
          throw new Error("Session pane not found");
        }
        expect(sessionPane).not.toHaveClass("active");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
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
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(filterScrollToCallsAt(scrollToMock, 920, "auto")).toEqual([]);

        await act(async () => {
          fireEvent.mouseDown(sessionPane);
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(sessionPane).toHaveClass("active");
        expect(
          Math.max(...scrollToTopsWithBehavior(scrollToMock, "auto")),
        ).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("does not repeat bottom-follow while the live waiting indicator remains visible", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const baseState = {
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
            preview: "Current response.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(
          Math.max(...scrollToTopsWithBehavior(scrollToMock, "auto")),
        ).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);

        scrollToMock.mockClear();
        scrollHeight = 1140;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 4,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(filterScrollToCallsAt(scrollToMock, 940, "auto")).toEqual([]);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps the final reply visible when it replaces live turn after earlier output", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let transcriptScrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () =>
          transcriptScrollHeight +
          (document.querySelector(".activity-card-live") ? 120 : 0),
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const userMessage: Session["messages"][number] = {
        id: "message-user-live-final",
        type: "text",
        timestamp: "10:00",
        author: "you",
        text: "Finish this turn",
      };
      const commandMessage: Session["messages"][number] = {
        id: "message-command-live-final",
        type: "command",
        timestamp: "10:00",
        author: "assistant",
        command: "cargo check",
        output: "Finished successfully",
        status: "success",
      };
      const activeState = {
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
            status: "active",
            preview: "Finish this turn",
            messages: [userMessage, commandMessage],
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), activeState);
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(
          document.querySelector(".activity-card-live"),
        ).toBeInTheDocument();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 920;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        // The backend can publish idle before the final transcript delta. This
        // status-only commit removes LIVE TURN without changing messages, so it
        // must arm (rather than consume) final-message synchronization.
        await dispatchStateEvent(latestEventSource(), {
          ...activeState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "idle",
              preview: "Finishing",
              messages: [userMessage, commandMessage],
            }),
          ],
        });
        await settleAsyncUi();
        expect(screen.queryByText("Live turn")).not.toBeInTheDocument();

        // Emulate the browser's automatic clamp after the in-flow LIVE TURN
        // card disappears, then isolate writes caused by the later final delta.
        messageStack.scrollTop = 800;
        scrollToMock.mockClear();

        // This turn already has agent output (the command), so final text is not
        // the "first output". The pending post-live latch must nevertheless
        // commit the final bottom synchronously before that message paints.
        transcriptScrollHeight = 1080;
        await dispatchStateEvent(latestEventSource(), {
          ...activeState,
          revision: 4,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "idle",
              preview: "Final response",
              messages: [
                userMessage,
                commandMessage,
                {
                  id: "message-assistant-live-final",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Final response",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          within(messageStack).getByText("Final response"),
        ).toBeInTheDocument();
        expect(screen.queryByText("Live turn")).not.toBeInTheDocument();
        expect(
          document.querySelector(".activity-card-live"),
        ).not.toBeInTheDocument();

        const replacementTops = scrollToTopsForElementWithBehavior(
          scrollToMock,
          messageStack,
          "auto",
        );
        expect(replacementTops).toEqual([880]);

        scrollToMock.mockClear();
        transcriptScrollHeight = 1160;
        await dispatchStateEvent(latestEventSource(), {
          ...activeState,
          revision: 5,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "idle",
              preview: "Final response with rendered detail",
              messages: [
                userMessage,
                commandMessage,
                {
                  id: "message-assistant-live-final",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Final response with rendered detail",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const followedTops = scrollToTopsForElementWithBehavior(
          scrollToMock,
          messageStack,
          "auto",
        );
        expect(followedTops).toEqual([960]);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("does not bottom-follow the live waiting indicator when far from bottom", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const baseState = {
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
            preview: "Current response.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 600;
        await act(async () => {
          fireEvent.wheel(messageStack, { deltaY: -160 });
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(messageStack).not.toHaveClass("is-tail-following");
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(messageStack).not.toHaveClass("is-tail-following");
        expect(filterScrollToCallsAt(scrollToMock, 920, "auto")).toEqual([]);
        expect(messageStack.scrollTop).toBe(440);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("bottom-follows the live waiting indicator through the virtualized transcript boundary", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = Array.from(
        { length: 90 },
        (_, index) => ({
          id: `message-assistant-${index + 1}`,
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: `Assistant response ${index + 1}.`,
        }),
      );
      const baseState = {
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
            preview: "Assistant response 90.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        for (let iteration = 0; iteration < 10; iteration += 1) {
          await settleAsyncUi();
        }

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        expect(
          messageStack.querySelector(".virtualized-message-list"),
        ).not.toBeNull();

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Assistant response 90.",
              messages,
            }),
          ],
        });
        for (let iteration = 0; iteration < 10; iteration += 1) {
          await settleAsyncUi();
        }

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(messageStack.scrollTop).toBe(920);
        expect(
          filterScrollToCallsAt(scrollToMock, 920, "auto").length,
        ).toBeGreaterThan(0);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps measured command-card growth inside one live bottom follow", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const resizeCallbacksByTarget = new Map<
        Element,
        Set<ResizeObserverCallback>
      >();
      class ResizeObserverHarness {
        private readonly callback: ResizeObserverCallback;
        private readonly targets = new Set<Element>();

        constructor(callback: ResizeObserverCallback) {
          this.callback = callback;
        }

        observe(target: Element) {
          this.targets.add(target);
          const callbacks = resizeCallbacksByTarget.get(target) ?? new Set();
          callbacks.add(this.callback);
          resizeCallbacksByTarget.set(target, callbacks);
        }

        unobserve(target: Element) {
          this.targets.delete(target);
          const callbacks = resizeCallbacksByTarget.get(target);
          callbacks?.delete(this.callback);
          if (callbacks?.size === 0) {
            resizeCallbacksByTarget.delete(target);
          }
        }

        disconnect() {
          this.targets.forEach((target) => this.unobserve(target));
        }
      }

      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession({
        resizeObserver:
          ResizeObserverHarness as unknown as typeof ResizeObserver,
      });
      // Keep the production bottom-follow cooldown active while this test
      // advances the simulated scroll sequence. Under full-suite CPU load,
      // more than the real 1.2-second window can elapse between assertions,
      // which makes the test exercise cooldown expiry instead of live
      // follow continuation. Install the spy only after potentially-throwing
      // setup so its immediately following `try` owns the full lifetime.
      const performanceNowSpy = vi
        .spyOn(performance, "now")
        .mockReturnValue(1_000);

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

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
              preview: "First assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const conversationPage = messageStack.querySelector(
          ".session-conversation-page:not([hidden])",
        );
        if (!(conversationPage instanceof HTMLElement)) {
          throw new Error("Active conversation content not found");
        }
        let conversationPageHeight = 240;
        conversationPage.getBoundingClientRect = () =>
          ({ height: conversationPageHeight }) as DOMRect;

        messageStack.scrollTop = 800;
        expect(
          messageStack.scrollHeight -
            messageStack.scrollTop -
            messageStack.clientHeight,
        ).toBe(0);
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();
        let growCommandCardAfterFirstFollow = true;
        // This test needs a stateful scroll mock after the initial setup:
        // the command card gains its measured height after the first follow
        // scroll, then delivers the same ResizeObserver callback as production.
        // Keep later assertions in this `it` aware that the standard
        // apply-top mock is intentionally replaced from this point on.
        scrollToMock.mockImplementation(function (
          this: HTMLElement,
          options?: ScrollToOptions | number,
          y?: number,
        ) {
          if (
            typeof options === "object" &&
            options !== null &&
            typeof options.top === "number"
          ) {
            this.scrollTop = options.top;
            // Simulate rendered message content measuring taller after the
            // first follow scroll.
            if (
              growCommandCardAfterFirstFollow &&
              options.behavior === "auto" &&
              options.top > 800
            ) {
              growCommandCardAfterFirstFollow = false;
              scrollHeight = 1200;
              conversationPageHeight = 340;
              queueMicrotask(() => {
                resizeCallbacksByTarget
                  .get(conversationPage)
                  ?.forEach((callback) =>
                    callback(
                      [
                        {
                          target: conversationPage,
                          contentRect: conversationPage.getBoundingClientRect(),
                        } as unknown as ResizeObserverEntry,
                      ],
                      {} as ResizeObserver,
                    ),
                  );
              });
            }
            return;
          }

          if (typeof options === "number" && typeof y === "number") {
            this.scrollTop = y;
          }
        });

        scrollHeight = 1100;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
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
              preview: "Running cargo check.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
                {
                  id: "message-command-2",
                  type: "command",
                  timestamp: "10:02",
                  author: "assistant",
                  command: "cargo check",
                  output: "",
                  status: "running",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const commandFollowTops = scrollToTopsWithBehavior(
          scrollToMock,
          "auto",
        );
        expect(commandFollowTops.length).toBeGreaterThan(0);
        expect(Math.max(...commandFollowTops)).toBeGreaterThan(900);
        expect(
          screen.queryByRole("button", { name: "New response" }),
        ).not.toBeInTheDocument();

        // The settled retargeting loop can finish before the programmatic
        // scroll classification window. A later measurement must still be
        // corrected once instead of being discarded by that stale time marker.
        scrollToMock.mockClear();
        performanceNowSpy.mockReturnValue(2_200);
        scrollHeight = 1300;
        conversationPageHeight = 440;
        await act(async () => {
          resizeCallbacksByTarget.get(conversationPage)?.forEach((callback) =>
            callback(
              [
                {
                  target: conversationPage,
                  contentRect: conversationPage.getBoundingClientRect(),
                } as unknown as ResizeObserverEntry,
              ],
              {} as ResizeObserver,
            ),
          );
          await flushUiWork();
        });
        expect(
          filterScrollToCallsAt(scrollToMock, 1100, "auto").length,
        ).toBeGreaterThan(0);

        // The measured command card collapses and the browser clamps the
        // attached viewport to the new physical bottom. F5 deliberately yields
        // a non-shrinking upward frame to the reader, so model the shrink before
        // publishing its native scroll event.
        scrollHeight = 960;
        messageStack.scrollTop = 760;
        expect(
          messageStack.scrollHeight -
            messageStack.scrollTop -
            messageStack.clientHeight,
        ).toBe(0);
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(messageStack).toHaveClass("is-tail-following");
        scrollToMock.mockClear();

        scrollHeight = 1200;
        await dispatchStateEvent(latestEventSource(), {
          revision: 4,
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
              preview: "Third assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Second assistant response.",
                },
                {
                  id: "message-assistant-3",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Third assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const responseFollowTops = scrollToTopsWithBehavior(
          scrollToMock,
          "auto",
        );
        expect(responseFollowTops.length).toBeGreaterThan(0);
        expect(Math.max(...responseFollowTops)).toBeGreaterThan(760);
        expect(
          screen.queryByRole("button", { name: "New response" }),
        ).not.toBeInTheDocument();

        scrollToMock.mockClear();
        messageStack.scrollTop = 760;
        await act(async () => {
          fireEvent.mouseDown(messageStack);
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        scrollHeight = 1300;
        await dispatchStateEvent(latestEventSource(), {
          revision: 5,
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
              preview: "Fourth assistant response.",
              messages: [
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "First assistant response.",
                },
                {
                  id: "message-assistant-2",
                  type: "text",
                  timestamp: "10:02",
                  author: "assistant",
                  text: "Second assistant response.",
                },
                {
                  id: "message-assistant-3",
                  type: "text",
                  timestamp: "10:03",
                  author: "assistant",
                  text: "Third assistant response.",
                },
                {
                  id: "message-assistant-4",
                  type: "text",
                  timestamp: "10:04",
                  author: "assistant",
                  text: "Fourth assistant response.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(filterScrollToCallsAt(scrollToMock, 1100, "auto")).toEqual([]);
        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
        performanceNowSpy.mockRestore();
      }
    });
  });

  it("scrolls down when queued prompts append in transcript order above the live turn", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

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
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
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
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:02",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const liveTurnCard = screen
          .getByText("Live turn")
          .closest(".activity-card-live");
        const queuedPromptCard = screen
          .getByText("Queued follow-up")
          .closest(".pending-prompt-card");
        expect(liveTurnCard).not.toBeNull();
        expect(queuedPromptCard).not.toBeNull();
        expect(
          Boolean(
            queuedPromptCard!.compareDocumentPosition(liveTurnCard!) &
            Node.DOCUMENT_POSITION_FOLLOWING,
          ),
        ).toBe(true);
        expect(
          Math.max(...scrollToTopsWithBehavior(scrollToMock, "auto")),
        ).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("labels the bottom indicator as activity when only queued prompts append", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        const messages: Session["messages"] = [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Current prompt",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Current turn partial.",
          },
        ];
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
              status: "active",
              preview: "Current turn partial.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 700;
        await act(async () => {
          fireEvent.mouseDown(messageStack);
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
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
              status: "active",
              preview: "Current turn partial.",
              messages,
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:02",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          await screen.findByRole("button", { name: "New activity" }),
        ).toBeInTheDocument();
        expect(
          screen.queryByRole("button", { name: "New response" }),
        ).not.toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("never silently hides queued prompts behind a detached history window", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const context = await renderAppWithProjectAndSession();

      try {
        await act(async () => {
          upsertSessionStoreSession({
            session: makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              messagesLoaded: false,
              hasOlderHistory: false,
              hasNewerHistory: true,
              messageCount: 1_000,
              messages: [
                {
                  id: "message-history-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "assistant",
                  text: "Detached historical window",
                },
              ],
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:01",
                  text: "First queued follow-up",
                },
                {
                  id: "pending-prompt-2",
                  timestamp: "10:02",
                  text: "Second queued follow-up",
                },
                {
                  id: "pending-prompt-3",
                  timestamp: "10:03",
                  text: "Third queued follow-up",
                },
              ],
            }),
            committedDraft: "",
            draftAttachments: [],
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(document.querySelectorAll(".pending-prompt-card")).toHaveLength(
          0,
        );
        const jumpToLatest = await screen.findByRole("button", {
          name: /Jump to latest/,
        });
        expect(jumpToLatest).toBeInTheDocument();
        expect(within(jumpToLatest).getByText("3 queued")).toHaveClass(
          "new-response-indicator-queued-count",
        );
      } finally {
        context.cleanup();
      }
    });
  });

  it("keeps a response indicator when queued prompts append after an unseen assistant response", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        const baseMessages: Session["messages"] = [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Current prompt",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Current turn partial.",
          },
        ];
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
              preview: "Current turn partial.",
              messages: baseMessages,
            }),
          ],
        });
        await settleAsyncUi();

        messageStack.scrollTop = 700;
        await act(async () => {
          fireEvent.mouseDown(messageStack);
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        const responseMessages: Session["messages"] = [
          ...baseMessages,
          {
            id: "message-assistant-2",
            type: "text",
            timestamp: "10:02",
            author: "assistant",
            text: "Fresh assistant response.",
          },
        ];
        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 3,
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
              messages: responseMessages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          await screen.findByRole("button", { name: "New response" }),
        ).toBeInTheDocument();

        scrollHeight = 1220;
        await dispatchStateEvent(latestEventSource(), {
          revision: 4,
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
              status: "active",
              preview: "Fresh assistant response.",
              messages: responseMessages,
              pendingPrompts: [
                {
                  id: "pending-prompt-1",
                  timestamp: "10:03",
                  text: "Queued follow-up",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        expect(
          screen.getByRole("button", { name: "New response" }),
        ).toBeInTheDocument();
        expect(
          screen.queryByRole("button", { name: "New activity" }),
        ).not.toBeInTheDocument();
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("detaches live-turn bottom follow on explicit transcript navigation", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

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
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const liveTail = screen
          .getByText("Live turn")
          .closest(".conversation-live-tail");
        expect(liveTail).not.toBeNull();
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        messageStack.scrollTop = 640;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "detached");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        // Trackpad momentum and touch overscroll often continue after the
        // transcript has reached its physical bottom. Those no-op gestures
        // must not silently turn live follow off.
        await act(async () => {
          fireEvent.wheel(messageStack, { deltaY: 20 });
          fireEvent.touchStart(messageStack, {
            touches: [{ clientY: 100 }],
          });
          fireEvent.touchMove(messageStack, {
            touches: [{ clientY: 80 }],
          });
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        act(() => {
          fireEvent.wheel(messageStack, { deltaY: -20 });
        });
        // Sticky LIVE TURN ownership must be gone in the input task itself;
        // waiting for the resulting native scroll or an animation frame lets
        // the card overlay history for one painted frame.
        expect(liveTail).toHaveAttribute("data-tail-follow", "detached");
        await act(async () => {
          await flushUiWork();
        });

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        act(() => {
          fireEvent.keyDown(messageStack, { key: "ArrowUp" });
        });
        // This is the production React onKeyDown -> normalized intent ->
        // virtualizer bridge. Detachment must happen before the browser's first
        // animated native scroll frame.
        expect(liveTail).toHaveAttribute("data-tail-follow", "detached");
        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        // Any deliberate transcript navigation switches LIVE TURN back to
        // normal flow before the custom non-passive wheel writer moves the
        // container. Downward navigation must obey the same mode switch as an
        // upward escape; reaching the real bottom can attach it again later.
        messageStack.scrollTop = 640;
        await act(async () => {
          fireEvent.wheel(messageStack, { deltaY: 20 });
          await flushUiWork();
        });
        expect(liveTail).toHaveAttribute("data-tail-follow", "detached");
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("keeps live-turn bottom follow attached across temporary layout gaps", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const context = await renderAppWithProjectAndSession();

      try {
        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

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
              status: "active",
              preview: "Current turn partial.",
              messages: [
                {
                  id: "message-user-1",
                  type: "text",
                  timestamp: "10:00",
                  author: "you",
                  text: "Current prompt",
                },
                {
                  id: "message-assistant-1",
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: "Current turn partial.",
                },
              ],
            }),
          ],
        });
        await settleAsyncUi();

        const liveTail = screen
          .getByText("Live turn")
          .closest(".conversation-live-tail");
        expect(liveTail).not.toBeNull();
        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        scrollHeight = 1120;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });

        expect(liveTail).toHaveAttribute("data-tail-follow", "attached");
      } finally {
        context.cleanup();
        restoreScrollGeometry();
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

  it("jumps the new-response button to the virtualized bottom without settled-scroll spam", async () => {
    await withVerifiedNoReactActWarnings(async () => {
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
                messages: Array.from({ length: 90 }, (_, index) => ({
                  id: `message-assistant-${index + 1}`,
                  type: "text",
                  timestamp: "10:01",
                  author: "assistant",
                  text: `Fresh assistant response ${index + 1}.`,
                })),
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

          expect(messageStack.scrollTop).toBe(800);
          // The boundary command performs one immediate ownership-transfer
          // write so any native smooth scroll is cancelled; the virtualizer
          // must not add settled-scroll retries after that single landing.
          expect(filterScrollToCallsAt(scrollToMock, 800, "auto")).toHaveLength(
            1,
          );
        } finally {
          teardown();
        }
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("runs the default-scroll-to-bottom branch of the session scroll useLayoutEffect on mount and lets the cleanup return cleanly", async () => {
    // A newly mounted or reactivated attached transcript must establish its
    // physical bottom during the layout phase. Deferring that first write to
    // requestAnimationFrame paints one frame at the previous tab's offset. The
    // settled scheduler still runs afterward to absorb late measurements, and
    // its cleanup must not issue a meaningless cancelAnimationFrame(0).
    await withVerifiedNoReactActWarnings(async () => {
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
        const scrollToMock = HTMLElement.prototype
          .scrollTo as unknown as ReturnType<typeof vi.fn>;
        scrollToMock.mockClear?.();

        const { cleanup: teardown } = await renderAppWithProjectAndSession();
        try {
          await settleAsyncUi();

          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          expect(messageStack).not.toBeNull();

          // The layout effect must establish `top: 800` synchronously before
          // the first animation frame. The auto write explicitly aborts any
          // stale native smooth animation before the direct readback value is
          // published, so neither operation may be deferred to a later frame.
          expect((messageStack as HTMLElement).scrollTop).toBe(800);
          expect(scrollToMock).toHaveBeenCalledWith({
            top: 800,
            behavior: "auto",
          });
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

  it("re-pins content, viewport, and active-page changes without observing composer resize frames", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const resizeCallbacksByTarget = new Map<
        Element,
        Set<ResizeObserverCallback>
      >();
      class ResizeObserverHarness {
        private readonly callback: ResizeObserverCallback;
        private readonly targets = new Set<Element>();

        constructor(callback: ResizeObserverCallback) {
          this.callback = callback;
        }

        observe(target: Element) {
          this.targets.add(target);
          const callbacks = resizeCallbacksByTarget.get(target) ?? new Set();
          callbacks.add(this.callback);
          resizeCallbacksByTarget.set(target, callbacks);
        }

        unobserve(target: Element) {
          this.targets.delete(target);
          const callbacks = resizeCallbacksByTarget.get(target);
          callbacks?.delete(this.callback);
          if (callbacks?.size === 0) {
            resizeCallbacksByTarget.delete(target);
          }
        }

        disconnect() {
          this.targets.forEach((target) => this.unobserve(target));
        }
      }

      const scrollToMock = mockScrollToAndApplyTop();
      const { cleanup: teardown } = await renderAppWithProjectAndSession({
        resizeObserver:
          ResizeObserverHarness as unknown as typeof ResizeObserver,
      });

      try {
        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        const composer = await screen.findByLabelText("Message Session 1");
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Session composer not found");
        }

        expect(messageStack.contains(composer)).toBe(false);
        expect(messageStack.parentElement).toBe(
          composer.closest(".workspace-pane"),
        );

        let clientHeight = 200;
        Object.defineProperty(messageStack, "clientHeight", {
          configurable: true,
          get: () => clientHeight,
        });
        let scrollHeight = 1000;
        Object.defineProperty(messageStack, "scrollHeight", {
          configurable: true,
          get: () => scrollHeight,
        });
        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();
        const authorityScrollWrites: CustomEvent[] = [];
        messageStack.addEventListener(
          MESSAGE_STACK_SCROLL_WRITE_EVENT,
          (event) => {
            authorityScrollWrites.push(event as CustomEvent);
          },
        );

        // The composer owns the one height mutation and requests one
        // synchronous correction. SessionPaneView must not observe the message
        // stack itself: a CSS height transition previously delivered several
        // observer callbacks (and scroll writes) for one keyboard edit.
        expect(resizeCallbacksByTarget.get(messageStack)).toBeUndefined();
        messageStack.scrollTop = 790;
        requestMessageStackBottomRepin(messageStack);
        expect(authorityScrollWrites).not.toHaveLength(0);
        expect(messageStack.scrollTop).toBe(800);

        const conversationPage = messageStack.querySelector(
          ".session-conversation-page:not([hidden]), .empty-state",
        );
        if (!(conversationPage instanceof HTMLElement)) {
          throw new Error("Active conversation content not found");
        }
        expect(
          resizeCallbacksByTarget.get(conversationPage)?.size,
        ).toBeGreaterThan(0);
        const conversationPageGeometry = vi.fn(
          () => ({ height: 240 }) as DOMRect,
        );
        conversationPage.getBoundingClientRect = conversationPageGeometry;
        scrollHeight = 1120;
        messageStack.scrollTop = 800;
        await act(async () => {
          resizeCallbacksByTarget.get(conversationPage)?.forEach((callback) =>
            callback(
              [
                {
                  target: conversationPage,
                  contentRect: conversationPage.getBoundingClientRect(),
                } as unknown as ResizeObserverEntry,
              ],
              {} as ResizeObserver,
            ),
          );
          await flushUiWork();
        });
        expect(messageStack.scrollTop).toBe(920);

        // Streaming mutates descendants of the active page for every chunk.
        // Those mutations must not run the structural page-rebind path or force
        // another synchronous geometry read.
        conversationPageGeometry.mockClear();
        const streamedChunk = document.createElement("span");
        await act(async () => {
          conversationPage.append(streamedChunk);
          streamedChunk.textContent = "streamed token";
          await flushUiWork();
        });
        expect(conversationPageGeometry).not.toHaveBeenCalled();
        expect(messageStack.scrollTop).toBe(920);

        const paneFrame = messageStack.closest(".workspace-pane");
        if (!(paneFrame instanceof HTMLElement)) {
          throw new Error("Workspace pane frame not found");
        }
        expect(resizeCallbacksByTarget.get(paneFrame)?.size).toBeGreaterThan(0);
        clientHeight = 260;
        await act(async () => {
          resizeCallbacksByTarget.get(paneFrame)?.forEach((callback) =>
            callback(
              [
                {
                  target: paneFrame,
                  contentRect: paneFrame.getBoundingClientRect(),
                } as unknown as ResizeObserverEntry,
              ],
              {} as ResizeObserver,
            ),
          );
          await flushUiWork();
        });
        expect(messageStack.scrollTop).toBe(860);

        const replacementPage = document.createElement("div");
        replacementPage.className = "session-conversation-page";
        replacementPage.getBoundingClientRect = () =>
          ({ height: 300 }) as DOMRect;
        scrollHeight = 1_200;
        await act(async () => {
          conversationPage.hidden = true;
          messageStack.append(replacementPage);
          await flushUiWork();
        });
        expect(resizeCallbacksByTarget.get(conversationPage)).toBeUndefined();
        expect(
          resizeCallbacksByTarget.get(replacementPage)?.size,
        ).toBeGreaterThan(0);
        expect(messageStack.scrollTop).toBe(940);

        replacementPage.getBoundingClientRect = () =>
          ({ height: 360 }) as DOMRect;
        scrollHeight = 1_260;
        await act(async () => {
          resizeCallbacksByTarget.get(replacementPage)?.forEach((callback) =>
            callback(
              [
                {
                  target: replacementPage,
                  contentRect: replacementPage.getBoundingClientRect(),
                } as unknown as ResizeObserverEntry,
              ],
              {} as ResizeObserver,
            ),
          );
          await flushUiWork();
        });
        expect(messageStack.scrollTop).toBe(1_000);

        fireEvent.change(composer, {
          target: { value: "first line\nsecond line\nthird line" },
        });
        await act(async () => {
          await flushUiWork();
        });
        expect(resizeCallbacksByTarget.get(messageStack)).toBeUndefined();
      } finally {
        teardown();
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
        const persistedLayoutRaw =
          window.localStorage.getItem(layoutStorageKey);
        expect(persistedLayoutRaw).not.toBeNull();
        const persistedLayout = JSON.parse(persistedLayoutRaw ?? "null") as {
          workspace: {
            root: {
              ratio: number;
            } | null;
          };
        };
        expect(persistedLayout.workspace.root?.ratio).toBeCloseTo(0.576, 5);
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

  it("keeps control panel width fallbacks aligned with the 36rem floor", () => {
    expect(CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX).toBe(36 * 16);
    expect(CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX).toBe(36 * 16);
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
});
