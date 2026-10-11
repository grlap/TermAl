// Owns minimap cancellation across the real pane, virtualizer and history bridge.
// Does not own rail geometry or transcript rendering assertions.
import { act, render, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as api from "../api";
import { useSessionPaneScrollState } from "../SessionPaneView.scroll";
import { installAnimationFrameHarness, params, session } from "../SessionPaneView.scroll.fixtures";
import { addSessionHistoryPageDemandListener } from "../session-history-demand";
import { loadBoundedSessionHistoryWindow, type SessionHistoryLoadingContext } from "../session-history-loading";
import { useConversationOverviewController } from "./conversation-overview-controller";
import { VirtualizedConversationMessageList } from "./VirtualizedConversationMessageList";
import type { VirtualizedConversationMessageListHandleRef } from "./virtualized-conversation-types";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("minimap navigation ownership", () => {
  it.each(["End", "Home", "marker", "Find input", "Find window adoption"] as const)(
    "preserves navigation ownership through %s",
    async (navigation) => {
      const { drainAnimationFrames } = installAnimationFrameHarness(1_000 / 60);
      const adoptingFindWindow = navigation === "Find window adoption";
      vi.spyOn(api, "fetchSessionOverview").mockReturnValue(new Promise(() => {}));
      let current = { ...session(false), messageStartIndex: 999, sessionMutationStamp: 1 };
      const page = (position: number) => ({
        sessionId: current.id,
        messages: [{ ...current.messages[0], id: `message-${position}` }],
        messageCount: 1_000, messageStartIndex: position,
        hasMore: position > 0, hasNewer: position < 999,
        nextBefore: position > 0 ? `message-${position}` : null,
        nextAfter: position < 999 ? `message-${position}` : null,
        revision: 1, serverInstanceId: "server-a", sessionMutationStamp: 1,
      });
      let resolveAround!: (value: ReturnType<typeof page>) => void;
      const around = new Promise<ReturnType<typeof page>>((resolve) => { resolveAround = resolve; });
      const fetch = vi.spyOn(api, "fetchSessionHistory").mockImplementation((_id, options) =>
        options.around === 20 ? around : Promise.resolve(page(options.from === "start" ? 0 : 999)),
      );
      const context: SessionHistoryLoadingContext = {
        getLastSeenServerInstanceId: () => "server-a",
        getSession: () => current,
        inFlightOlderLoads: new Map(), isMounted: () => true,
        publishSession: vi.fn((next) => { current = next as typeof current; return true; }),
        reportRequestError: vi.fn(), requestActionRecoveryResync: vi.fn(),
      };
      const loads: Promise<void>[] = [];
      const remove = addSessionHistoryPageDemandListener((demand) => {
        loads.push(loadBoundedSessionHistoryWindow({ context, demand }));
      });
      const node = document.createElement("section");
      Object.defineProperties(node, {
        clientHeight: { value: 200 }, clientWidth: { value: 600 },
        scrollHeight: { value: 1_000 }, scrollTop: { writable: true, value: 800 },
        scrollTo: { value: (options: ScrollToOptions) => { node.scrollTop = options.top ?? 0; } },
      });
      const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = { current: null };
      const scrollContainerRef = { current: node };
      const input = { ...params(current), isSessionTabActive: true };
      const matchId = () => current.messages[current.messages.length - 1].id;
      const list = (search: boolean) => <VirtualizedConversationMessageList
        isActive sessionId={current.id} messages={current.messages}
        conversationSearchQuery={search ? "Live" : ""}
        conversationSearchActiveItemKey={search ? `message:${matchId()}` : null}
        scrollContainerRef={scrollContainerRef} virtualizerHandleRef={virtualizerHandleRef}
        renderMessageCard={(message) => <div>{message.id}</div>}
        onApprovalDecision={() => {}} onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}} onCodexAppRequestSubmit={() => {}}
      />;
      const view = render(list(adoptingFindWindow));
      const hook = renderHook(({ search }) => {
        const pane = useSessionPaneScrollState({
          ...input, activeSession: current, hasSessionFindQuery: search,
          activeSessionSearchMatch: search ? {
            itemId: matchId(), itemKey: `message:${matchId()}`,
            itemKind: "message", snippet: "Live tail message",
          } : null,
        });
        pane.messageStackRef.current = node;
        pane.virtualizerHandleRef.current = virtualizerHandleRef.current;
        const overview = useConversationOverviewController({
          isActive: true, messageCount: 1_000, messageStartIndex: current.messageStartIndex,
          renderedMessageCount: current.messages.length, scrollContainerRef,
          scrollStateKey: input.scrollStateKey, sessionId: current.id,
          sessionMutationStamp: 1, tailFollowIntent: pane.liveTailPinned,
          onNavigateStart: pane.beginOverviewNavigation, virtualizerHandleRef,
        });
        return { pane, overview };
      }, { initialProps: { search: adoptingFindWindow } });
      try {
        let completion!: Promise<boolean>;
        act(() => { completion = hook.result.current.overview.navigate(20); });
        const signal = fetch.mock.calls[0]?.[1].signal;
        expect(signal?.aborted).toBe(false);
        if (adoptingFindWindow) {
          await act(async () => {
            const next = page(20);
            next.messages.push({ ...next.messages[0], id: "message-21" });
            next.nextAfter = "message-21";
            resolveAround(next);
            await Promise.all(loads);
          });
          expect(context.reportRequestError).not.toHaveBeenCalled();
          expect(current.messageStartIndex).toBe(20);
          // Loaded-window adoption changes the derived Find selection without
          // a query edit or Next/Previous gesture. It must not revoke the seek.
          act(() => {
            hook.rerender({ search: true });
            view.rerender(list(true));
          });
          expect(signal?.aborted).toBe(false);
          const jump = vi.spyOn(virtualizerHandleRef.current!, "jumpToMessageIndex");
          drainAnimationFrames();
          await expect(completion).resolves.toBe(true);
          expect(jump).toHaveBeenLastCalledWith(0, expect.objectContaining({ align: "center" }));
          return;
        }
        await act(async () => {
          if (navigation === "marker") virtualizerHandleRef.current!.beginUserScrollNavigation();
          else if (navigation === "Find input") {
            hook.result.current.pane.beginSearchNavigation();
            hook.rerender({ search: true });
            view.rerender(list(true));
          }
          else hook.result.current.pane.scrollMessageStackToBoundary(navigation === "Home" ? "top" : "bottom");
          await Promise.resolve();
        });
        expect(signal?.aborted).toBe(true);
        expect(current.messageStartIndex).toBe(navigation === "Home" ? 0 : 999);
        const newerWindow = current.messages;
        await act(async () => {
          resolveAround(page(20));
          await Promise.all(loads);
          await expect(completion).resolves.toBe(false);
        });
        expect(current.messages).toBe(newerWindow);
        expect(context.reportRequestError).not.toHaveBeenCalled();
      } finally {
        remove(); hook.unmount(); view.unmount();
      }
    },
  );
});
