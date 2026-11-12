// Owns native missing-history scroll requests, coalescing, and load integration.
// Does not claim browser thumb feel; the actual virtualizer/history owners are used.
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as api from "../api";
import { installAnimationFrameHarness, session } from "../SessionPaneView.scroll.fixtures";
import { addSessionHistoryPageDemandListener } from "../session-history-demand";
import { loadBoundedSessionHistoryWindow, type SessionHistoryLoadingContext } from "../session-history-loading";
import { notifyMessageStackScrollWrite, notifyMessageStackUserScrollIntent } from "../message-stack-scroll-sync";
import { useConversationHistoryScroll } from "./useConversationHistoryScroll";
import { VirtualizedConversationMessageList } from "./VirtualizedConversationMessageList";
import type { VirtualizedConversationMessageListHandle, VirtualizedConversationMessageListHandleRef } from "./virtualized-conversation-types";

afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("native history scrolling", () => {
  it.each(["thumb", "track", "wheel", "keyboard", "superseded thumb", "suffix", "unmeasured", "late native listener", "programmatic follow", "deactivated pane", "session switch", "scope switch"] as const)("loads only measurable missing history through native %s input", async (input) => {
    const superseded = input === "superseded thumb";
    vi.useFakeTimers();
    const { drainAnimationFrames } = installAnimationFrameHarness(1_000 / 60);
    let current = { ...session(false), messageCount: 1_000, messageStartIndex: input === "suffix" ? 0 : 999, hasOlderHistory: input !== "suffix", hasNewerHistory: input === "suffix" };
    const makePage = (position: number) => ({
      sessionId: current.id,
      messages: [0, 1].map(offset => ({ ...current.messages[0], id: `message-${position + offset}` })),
      messageCount: 1_000, messageStartIndex: position,
      hasMore: position > 0, hasNewer: position + 2 < 1_000,
      nextBefore: `message-${position}`, nextAfter: `message-${position + 1}`,
      revision: 1, serverInstanceId: "server-a", sessionMutationStamp: 1,
    });
    const resolvePages = new Map<number, (page: ReturnType<typeof makePage>) => void>();
    const fetch = vi.spyOn(api, "fetchSessionHistory").mockImplementation((_id, options) =>
      new Promise(resolve => { resolvePages.set(options.around ?? -1, resolve); }));
    const loads: Promise<void>[] = [];
    const context: SessionHistoryLoadingContext = {
      getLastSeenServerInstanceId: () => "server-a", getSession: () => current,
      inFlightOlderLoads: new Map(), isMounted: () => true,
      publishSession: vi.fn(next => { current = next as typeof current; return true; }),
      reportRequestError: vi.fn(), requestActionRecoveryResync: vi.fn(),
    };
    const remove = addSessionHistoryPageDemandListener(demand => { loads.push(loadBoundedSessionHistoryWindow({ context, demand })); });
    const node = document.createElement("section");
    Object.defineProperties(node, {
      clientHeight: { value: 200 }, clientWidth: { value: 600 },
      scrollHeight: { value: 300_000 }, scrollTop: { value: 199_800, writable: true },
      scrollTo: { value: (options: ScrollToOptions) => { node.scrollTop = options.top ?? 0; } },
    });
    document.body.append(node);
    const scrollContainerRef = { current: node };
    let active = true;
    let scope = "pane";
    const handle: VirtualizedConversationMessageListHandleRef = { current: null };
    let history!: ReturnType<typeof useConversationHistoryScroll>;
    function Conversation() {
      history = useConversationHistoryScroll({
        isActive: active, sessionId: current.id, scrollStateKey: scope, messageCount: 1_000,
        messageStartIndex: current.messageStartIndex, renderedMessageCount: current.messages.length,
        scrollContainerRef, virtualizerHandleRef: handle,
      });
      return <div ref={history.contentRef}>
        <VirtualizedConversationMessageList isActive={active} sessionId={current.id} messages={current.messages}
          messageStartIndex={current.messageStartIndex} scrollContainerRef={scrollContainerRef} virtualizerHandleRef={handle}
          historyBeforePx={history.prefixHeight} historyAfterPx={history.suffixHeight}
          renderMessageCard={message => <div>{message.id}</div>}
          onApprovalDecision={() => {}} onUserInputSubmit={async () => {}}
          onMcpElicitationSubmit={() => {}} onCodexAppRequestSubmit={() => {}} />
      </div>;
    }
    const view = render(<Conversation />, { container: node });
    if (input === "late native listener") node.addEventListener("scroll", () => { handle.current!.beginUserScrollNavigation(); });
    const content = history.contentRef.current!;
    vi.spyOn(content, "getBoundingClientRect").mockImplementation(() => ({ top: -node.scrollTop, left: 0, width: 600, height: input === "unmeasured" ? 0 : 300_000, right: 600, bottom: 300_000 - node.scrollTop, x: 0, y: -node.scrollTop, toJSON: () => ({}) }));
    const scrollTo = (position: number) => {
      const unit = current.messageStartIndex > 0 ? history.prefixHeight / current.messageStartIndex : history.suffixHeight / (1_000 - current.messages.length);
      node.scrollTop = (position + 0.25) * unit - 100;
      fireEvent.scroll(node);
    };
    try {
      act(() => {
        if (input === "wheel") fireEvent.wheel(node, { deltaY: -120 });
        else if (input === "keyboard") notifyMessageStackUserScrollIntent(node, { scrollKind: "page_jump", direction: "up", viewportCanMove: true });
        else fireEvent.mouseDown(node, { button: 0 });
        scrollTo(20);
        scrollTo(40);
        if (input === "track") fireEvent.mouseUp(window);
        if (input === "programmatic follow") {
          node.scrollTop = 299_800;
          notifyMessageStackScrollWrite(node, { scrollKind: "bottom_follow" });
          fireEvent.scroll(node);
        }
      });
      await act(async () => { await vi.advanceTimersByTimeAsync(80); });
      if (input === "unmeasured" || input === "programmatic follow") {
        expect(fetch).not.toHaveBeenCalled();
        expect(context.publishSession).not.toHaveBeenCalled();
        return;
      }
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0][1]).toMatchObject({ around: 40 });
      if (input === "deactivated pane" || input === "session switch" || input === "scope switch") {
        const jump = vi.spyOn(handle.current!, "jumpToMessageIndex");
        act(() => {
          if (input === "deactivated pane") active = false;
          else if (input === "session switch") current = { ...current, id: "another-session" };
          else scope = "another-pane";
          view.rerender(<Conversation />);
        });
        expect(fetch.mock.calls[0][1].signal?.aborted).toBe(true);
        await act(async () => { resolvePages.get(40)!(makePage(40)); await Promise.all(loads); });
        act(() => { drainAnimationFrames(); });
        expect(jump).not.toHaveBeenCalled();
        expect(context.publishSession).not.toHaveBeenCalled();
        expect(current.messageStartIndex).toBe(999);
        return;
      }
      let expectedPosition = 40;
      if (superseded) {
        act(() => { fireEvent.mouseMove(window); scrollTo(60); });
        await act(async () => { await vi.advanceTimersByTimeAsync(80); });
        expect(fetch.mock.calls[0][1].signal?.aborted).toBe(true);
        expect(fetch).toHaveBeenCalledTimes(2);
        expect(fetch.mock.calls[1][1]).toMatchObject({ around: 60 });
        await act(async () => { resolvePages.get(40)!(makePage(40)); await loads[0]; });
        expect(current.messageStartIndex).toBe(999);
        expectedPosition = 60;
      }
      const jump = vi.spyOn(handle.current!, "jumpToMessageIndex");
      await act(async () => { resolvePages.get(expectedPosition)!(makePage(expectedPosition)); await Promise.all(loads); });
      act(() => { view.rerender(<Conversation />); });
      act(() => { drainAnimationFrames(); });
      expect(screen.getByText(`message-${expectedPosition}`)).toBeInTheDocument();
      expect(current.messageStartIndex).toBe(expectedPosition);
      expect(jump).toHaveBeenCalledWith(0, expect.objectContaining({ preserveDetachedAuthority: true }));
      expect(context.reportRequestError).not.toHaveBeenCalled();
    } finally { remove(); view.unmount(); node.remove(); }
  });

  it.each([
    { name: "overlapping STAY", follow: false, nextStart: 500, scrollHeight: 400_000, expectedTop: 203_000 },
    { name: "FOLLOW", follow: true, nextStart: 500, scrollHeight: 400_000, expectedTop: 103_000 },
    { name: "different window", follow: false, nextStart: 900, scrollHeight: 400_000, expectedTop: 103_000 },
    { name: "bounded correction", follow: false, nextStart: 500, scrollHeight: 120_000, expectedTop: 119_400 },
  ])("corrects prefix estimates only within the current resident authority: $name", ({ follow, nextStart, scrollHeight, expectedTop }) => {
    const { drainAnimationFrames } = installAnimationFrameHarness(1_000 / 60);
    const node = document.createElement("section");
    node.scrollTop = 103_000;
    Object.defineProperties(node, {
      scrollHeight: { value: scrollHeight }, clientHeight: { value: 600 },
      scrollTo: { value: (options: ScrollToOptions) => { node.scrollTop = options.top ?? 0; } },
    });
    let height = 4_000;
    let start = 500;
    let committed = () => {};
    const handle: VirtualizedConversationMessageListHandleRef = { current: {
      getLayoutSnapshot: () => ({ sessionId: "session", estimatedTotalHeightPx: height }),
      getUserScrollGeneration: () => 5,
      subscribeToCommittedLayout: (listener: () => void) => { committed = listener; return () => { committed = () => {}; }; },
    } as unknown as VirtualizedConversationMessageListHandle };
    const scrollContainerRef = { current: node };
    const hook = renderHook(() => useConversationHistoryScroll({
      isActive: true, sessionId: "session", scrollStateKey: "pane", messageCount: 1_000,
      messageStartIndex: start, renderedMessageCount: 20, tailFollowIntent: follow,
      scrollContainerRef, virtualizerHandleRef: handle,
    }));
    expect(hook.result.current.prefixHeight).toBe(100_000);
    act(() => { height = 8_000; start = nextStart; hook.rerender(); committed(); drainAnimationFrames(); });
    expect(hook.result.current.prefixHeight).toBe(nextStart * 400);
    expect(node.scrollTop).toBe(expectedTop);
    hook.unmount();
  });
});
