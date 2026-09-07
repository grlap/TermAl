// Owns cold activation of an inactive, summary-only session after live deltas.
// Passive repairs of explicitly navigated history belong to the hydration suite.
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import { setAppTestHooksForTests } from "./app-test-hooks";
import { getSessionRecordSnapshotForTesting } from "./session-store";
import {
  EventSourceMock,
  clickAndSettle,
  createDeferred,
  createScheduledAnimationFrameMocks,
  dispatchOpenedStateEvent,
  flushUiWork,
  latestEventSource,
  makeSession,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  mockScrollToAndApplyTop,
  renderAppWithProjectAndSession,
  settleAsyncUi,
  stubElementScrollGeometry,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";
import type { Message } from "./types";

describe("App live state - sparse tail activation", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    const frames = createScheduledAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", frames.requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", frames.cancelAnimationFrameMock);
    HTMLElement.prototype.scrollTo = vi.fn();
    EventSourceMock.instances = [];
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(makeWorkspaceLayoutResponse());
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    window.localStorage.clear();
    setAppTestHooksForTests(null);
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it.each([1, 3])("bottom-aligns %i inactive deltas before tail hydration without changing FOLLOW", async (deltaCount) => {
    await withVerifiedNoReactActWarnings(async () => {
      // Both the sparse suffix and the hydrated short tail fit in this viewport.
      // This models scroll geometry only; pixel alignment is browser-verified.
      const restoreGeometry = stubElementScrollGeometry({ clientHeight: 600, scrollHeight: 600 });
      const scrollTo = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = getSessionRecordSnapshotForTesting("session-1")!;
        const summary = makeSession("cold-session", {
          name: "Cold session",
          status: "active",
          messageCount: 1000 - deltaCount,
          sessionMutationStamp: 8,
          messages: [],
          queuePaused: false,
        });
        const pendingTail = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const originalFetchTail = api.fetchSessionTail;
        const fetchTail = vi.spyOn(api, "fetchSessionTail").mockImplementation((id, limit) =>
          id === summary.id ? pendingTail.promise : originalFetchTail(id, limit),
        );
        vi.spyOn(api, "fetchSessionOverview").mockResolvedValue({
          sessionId: summary.id,
          messageCount: 1000,
          sessionMutationStamp: 8 + deltaCount,
          buckets: [],
          markers: [],
          latestPosition: 999,
        });
        const tail: Message[] = Array.from({ length: 20 }, (_, index) => ({
          id: `tail-${980 + index}`,
          type: "text",
          author: "assistant",
          timestamp: "10:00",
          text: `Recent response ${980 + index}`,
        }));
        await dispatchOpenedStateEvent(latestEventSource(), makeStateResponse({
          revision: 2,
          serverInstanceId: "test-instance",
          projects: [],
          orchestrators: [],
          workspaces: [],
          sessions: [{ ...initial, queuePaused: initial.queuePaused ?? false, messageCount: initial.messageCount ?? initial.messages.length }, summary],
        }));
        expect(getSessionRecordSnapshotForTesting(summary.id)).toMatchObject({
          messages: [], messageStartIndex: 1000 - deltaCount,
          hasOlderHistory: true, hasNewerHistory: false,
        });
        expect(fetchTail.mock.calls.some(([id]) => id === summary.id)).toBe(false);
        await act(async () => {
          tail.slice(-deltaCount).forEach((message, index) => {
            latestEventSource().dispatchNamedEvent("delta", {
              type: "messageCreated",
              revision: 3 + index,
              sessionId: summary.id,
              messageId: message.id,
              messageIndex: 1000 - deltaCount + index,
              messageCount: 1001 - deltaCount + index,
              message,
              preview: message.type === "text" ? message.text : "",
              status: "active",
              sessionMutationStamp: 9 + index,
            });
          });
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(getSessionRecordSnapshotForTesting(summary.id)?.messages).toEqual(tail.slice(-deltaCount));
        expect(document.querySelector('[data-message-id="tail-999"]')).toBeNull();
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector<HTMLElement>(".session-list")!;
        await clickAndSettle(within(sessionList).getByText("Cold session").closest("button")!);
        await waitFor(() => expect(fetchTail).toHaveBeenCalledWith(summary.id, expect.anything()));
        const stack = document.querySelector<HTMLElement>(".workspace-pane.active .message-stack")!;
        const sparsePage = stack.querySelector(".session-conversation-page");
        expect(sparsePage).toHaveClass("has-incomplete-live-tail");
        expect(stack.querySelector('[data-message-id="tail-999"]')).toBeInTheDocument();
        expect(stack).toHaveClass("is-tail-following");
        expect(stack.scrollTop).toBe(0);
        scrollTo.mockClear();
        await act(async () => {
          pendingTail.resolve({
            revision: 2 + deltaCount,
            serverInstanceId: "test-instance",
            session: {
              ...summary,
              messageCount: 1000,
              sessionMutationStamp: 8 + deltaCount,
              messageStartIndex: 980,
              messagesLoaded: false,
              hasOlderHistory: true,
              hasNewerHistory: false,
              messages: tail,
            },
          });
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(getSessionRecordSnapshotForTesting(summary.id)).toMatchObject({ messages: tail, messageStartIndex: 980 });
        expect(stack.querySelector(".session-conversation-page")).toBe(sparsePage);
        expect(sparsePage).toHaveClass("has-incomplete-live-tail");
        expect(stack.querySelector('[data-message-id="tail-999"]')).toBeInTheDocument();
        expect(stack).toHaveClass("is-tail-following");
        expect(stack.scrollTop).toBe(0);
        expect(scrollTo).not.toHaveBeenCalled();
      } finally {
        context.cleanup();
        restoreGeometry();
      }
    });
  });
});
