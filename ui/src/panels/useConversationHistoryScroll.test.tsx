// Owns native missing-history scroll requests, coalescing, and load integration.
// Does not claim browser thumb feel; the actual virtualizer/history owners are used.
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as api from "../api";
import { installAnimationFrameHarness, session } from "../SessionPaneView.scroll.fixtures";
import { addSessionHistoryPageDemandListener } from "../session-history-demand";
import { loadBoundedSessionHistoryWindow, type SessionHistoryLoadingContext } from "../session-history-loading";
import { notifyMessageStackNavigationStart, notifyMessageStackScrollWrite, notifyMessageStackUserScrollIntent } from "../message-stack-scroll-sync";
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
      // The rendered estimates can be held at the viewport; their sum is not.
      const unit = (history.prefixHeight + history.suffixHeight) / (1_000 - current.messages.length);
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
        act(() => { fireEvent.mouseMove(window, { buttons: 1 }); scrollTo(60); });
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
      // The loaded target is placed under the viewport by the estimate above
      // it; the scroll position the reader chose (the thumb) does not move.
      expect(jump).not.toHaveBeenCalled();
      expect(Math.abs(history.prefixHeight - node.scrollTop)).toBeLessThan(200);
      expect(context.reportRequestError).not.toHaveBeenCalled();
    } finally { remove(); view.unmount(); node.remove(); }
  });

  // A transcript of 1,000 messages whose newest message is resident. The
  // rest of the scroll extent is the estimate for history that is not loaded.
  function mountLongHistory() {
    vi.useFakeTimers();
    // Input leases are measured on performance.now(); keep it on the fake clock.
    vi.spyOn(performance, "now").mockImplementation(() => Date.now());
    const { drainAnimationFrames } = installAnimationFrameHarness(1_000 / 60);
    let current = { ...session(false), messageCount: 1_000, messageStartIndex: 999, hasOlderHistory: true, hasNewerHistory: false };
    const resolvePages = new Map<number, (page: unknown) => void>();
    const fetch = vi.spyOn(api, "fetchSessionHistory").mockImplementation((_id, options) =>
      new Promise(resolve => { resolvePages.set(options.around ?? -1, resolve as (page: unknown) => void); }));
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
    const handle: VirtualizedConversationMessageListHandleRef = { current: null };
    let history!: ReturnType<typeof useConversationHistoryScroll>;
    function Conversation() {
      history = useConversationHistoryScroll({
        isActive: true, sessionId: current.id, scrollStateKey: "pane", messageCount: 1_000,
        messageStartIndex: current.messageStartIndex, renderedMessageCount: current.messages.length,
        scrollContainerRef, virtualizerHandleRef: handle,
      });
      return <div ref={history.contentRef}>
        <VirtualizedConversationMessageList isActive sessionId={current.id} messages={current.messages}
          messageStartIndex={current.messageStartIndex} scrollContainerRef={scrollContainerRef} virtualizerHandleRef={handle}
          historyBeforePx={history.prefixHeight} historyAfterPx={history.suffixHeight}
          renderMessageCard={message => <div>{message.id}</div>}
          onApprovalDecision={() => {}} onUserInputSubmit={async () => {}}
          onMcpElicitationSubmit={() => {}} onCodexAppRequestSubmit={() => {}} />
      </div>;
    }
    const view = render(<Conversation />, { container: node });
    vi.spyOn(history.contentRef.current!, "getBoundingClientRect").mockImplementation(() => ({ top: -node.scrollTop, left: 0, width: 600, height: 300_000, right: 600, bottom: 300_000 - node.scrollTop, x: 0, y: -node.scrollTop, toJSON: () => ({}) }));
    // The rendered estimates can be held at the viewport; their sum is not.
    const unit = () => (history.prefixHeight + history.suffixHeight) / (1_000 - current.messages.length);
    // Centers the 200 px viewport on the given message position.
    const topFor = (position: number) => (position + 0.25) * unit() - 100;
    const scrollTo = (position: number) => { node.scrollTop = topFor(position); fireEvent.scroll(node); };
    // Completes the pending request around a position with a two-message page.
    const loadPage = async (position: number) => {
      await act(async () => {
        resolvePages.get(position)!({
          sessionId: current.id,
          messages: [0, 1].map(offset => ({ ...current.messages[0], id: `message-${position + offset}` })),
          messageCount: 1_000, messageStartIndex: position, hasMore: position > 0, hasNewer: position + 2 < 1_000,
          nextBefore: `message-${position}`, nextAfter: `message-${position + 1}`,
          revision: 1, serverInstanceId: "server-a", sessionMutationStamp: 1,
        });
        await Promise.all(loads);
      });
      act(() => { view.rerender(<Conversation />); });
      act(() => { drainAnimationFrames(); });
    };
    const rendered = () => ({ prefix: history.prefixHeight, suffix: history.suffixHeight, heldAtViewport: history.heldAtViewport });
    return { node, fetch, unit, topFor, scrollTo, handle, loadPage, rendered, cleanup: () => { remove(); view.unmount(); node.remove(); } };
  }

  it("loads the settled position when a smooth wheel scroll outlasts its input lease", async () => {
    const { fetch, scrollTo, cleanup, node } = mountLongHistory();
    try {
      act(() => { fireEvent.wheel(node, { deltaY: -1_200 }); });
      // The browser animates one large wheel step frame by frame, here for
      // about 400 ms, so the scroll settles after the 250 ms input lease.
      for (let frame = 0; frame <= 25; frame += 1) {
        await act(async () => { scrollTo(20 + frame * 0.8); await vi.advanceTimersByTimeAsync(16); });
      }
      await act(async () => { await vi.advanceTimersByTimeAsync(80); });
      // The viewport rests in history that is not loaded. Leaving it there is
      // the blank viewport: it must request the history it is showing.
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0][1]).toMatchObject({ around: 40 });
    } finally { cleanup(); }
  });

  it("loads the released position after a scrollbar drag whose last pointer move does not scroll", async () => {
    const { fetch, scrollTo, cleanup, node } = mountLongHistory();
    try {
      act(() => { fireEvent.mouseDown(node, { button: 0 }); });
      // Each drag tick moves the pointer, then the thumb.
      for (const position of [300, 200, 120, 60, 40]) {
        await act(async () => { fireEvent.mouseMove(window, { buttons: 1 }); scrollTo(position); await vi.advanceTimersByTimeAsync(16); });
      }
      // The pointer still moves (sideways, or past the end of the track)
      // without moving the thumb, then the button is released.
      await act(async () => { fireEvent.mouseMove(window, { buttons: 1 }); await vi.advanceTimersByTimeAsync(16); });
      act(() => { fireEvent.mouseUp(window); });
      await act(async () => { await vi.advanceTimersByTimeAsync(400); });
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0][1]).toMatchObject({ around: 40 });
    } finally { cleanup(); }
  });

  it("holds the loaded messages at the viewport while a scrollbar jump's target loads", async () => {
    const { fetch, scrollTo, handle, loadPage, rendered, cleanup, node } = mountLongHistory();
    try {
      const hold = vi.spyOn(handle.current!, "jumpToMessageId");
      const jump = vi.spyOn(handle.current!, "jumpToMessageIndex");
      // A thumb drag from the newest message deep into unloaded history.
      act(() => { fireEvent.mouseDown(node, { button: 0 }); });
      for (const position of [600, 300, 120, 60, 40]) {
        await act(async () => { fireEvent.mouseMove(window, { buttons: 1 }); scrollTo(position); await vi.advanceTimersByTimeAsync(16); });
      }
      act(() => { fireEvent.mouseUp(window); });
      await act(async () => { await vi.advanceTimersByTimeAsync(400); });
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0][1]).toMatchObject({ around: 40 });
      // Until the target is resident, the estimate above the loaded messages
      // is rendered at the viewport (about 40 messages down, not 999), so they
      // fill it instead of the empty estimate. The thumb is not moved.
      const position40Top = node.scrollTop;
      expect(rendered().heldAtViewport).toBe(true);
      expect(Math.abs(rendered().prefix - position40Top)).toBeLessThan(1_000);
      expect(hold).not.toHaveBeenCalled();
      expect(jump).not.toHaveBeenCalled();
      // Once the target fragment is resident it is placed under the viewport
      // without moving the thumb, and that placement becomes its estimate:
      // the thumb's position then maps to the history in view, so nothing
      // else is loaded.
      const thumbTop = node.scrollTop;
      await loadPage(40);
      expect(jump).not.toHaveBeenCalled();
      expect(node.scrollTop).toBe(thumbTop);
      expect(Math.abs(rendered().prefix - thumbTop)).toBeLessThan(200);
      await act(async () => { await vi.advanceTimersByTimeAsync(400); });
      expect(fetch).toHaveBeenCalledTimes(1);
    } finally { cleanup(); }
  });

  it("loads where a thumb drag stops even when the browser never delivers its mouseup", async () => {
    const { fetch, scrollTo, cleanup, node } = mountLongHistory();
    try {
      act(() => { fireEvent.mouseDown(node, { button: 0 }); });
      for (const position of [300, 200, 120, 60, 40]) {
        await act(async () => {
          scrollTo(position);
          // The virtualizer handles each native frame after this hook and
          // announces it as reader movement with a navigation start.
          await Promise.resolve();
          notifyMessageStackNavigationStart(node);
          await vi.advanceTimersByTimeAsync(16);
        });
      }
      // No mouseup: Chrome often keeps it from the page after a thumb drag.
      await act(async () => { await vi.advanceTimersByTimeAsync(400); });
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0][1]).toMatchObject({ around: 40 });
    } finally { cleanup(); }
  });

  it("loads where a scrollbar track click's animated jump settles", async () => {
    const { fetch, scrollTo, cleanup, node } = mountLongHistory();
    try {
      // A click on the track, released at once; the browser then animates
      // the jump frame by frame for about 400 ms, long after the release.
      act(() => { fireEvent.mouseDown(node, { button: 0 }); });
      await act(async () => { await vi.advanceTimersByTimeAsync(40); });
      act(() => { fireEvent.mouseUp(window); });
      for (let frame = 0; frame <= 25; frame += 1) {
        await act(async () => { scrollTo(140 - frame * 4); await vi.advanceTimersByTimeAsync(16); });
      }
      await act(async () => { await vi.advanceTimersByTimeAsync(400); });
      expect(fetch).toHaveBeenCalled();
      expect(fetch.mock.calls[fetch.mock.calls.length - 1][1]).toMatchObject({ around: 40 });
    } finally { cleanup(); }
  });

  it("ends a continuing wheel motion at a notified programmatic write", async () => {
    const { fetch, topFor, scrollTo, cleanup, node } = mountLongHistory();
    try {
      act(() => { fireEvent.wheel(node, { deltaY: -1_200 }); });
      // Native frames continue the motion beyond the wheel's input lease.
      for (let frame = 0; frame <= 20; frame += 1) {
        await act(async () => { scrollTo(300 - frame * 5); await vi.advanceTimersByTimeAsync(16); });
      }
      // A follow or restore write then moves the viewport elsewhere in
      // unloaded history. It is not the reader's motion: nothing is requested.
      await act(async () => {
        node.scrollTop = topFor(40);
        notifyMessageStackScrollWrite(node, { scrollKind: "bottom_follow" });
        fireEvent.scroll(node);
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(fetch).not.toHaveBeenCalled();
    } finally { cleanup(); }
  });

  it("loads the history that held ArrowUp scrolls into", async () => {
    const { fetch, topFor, unit, cleanup, node } = mountLongHistory();
    try {
      // The pane turns each repeated ArrowUp into an intent, then its own
      // user-sourced 40 px write, as SessionPaneView does.
      node.scrollTop = topFor(999);
      const stepPx = 40;
      const steps = Math.ceil((topFor(999) - topFor(997)) / stepPx);
      for (let repeat = 0; repeat < steps; repeat += 1) {
        await act(async () => {
          notifyMessageStackUserScrollIntent(node, { scrollKind: "incremental", direction: "up", viewportCanMove: true });
          node.scrollTop -= stepPx;
          notifyMessageStackScrollWrite(node, { scrollKind: "incremental", scrollSource: "user" });
          fireEvent.scroll(node);
          await vi.advanceTimersByTimeAsync(33);
        });
      }
      await act(async () => { await vi.advanceTimersByTimeAsync(400); });
      const settled = Math.floor((node.scrollTop + 100) / unit());
      expect(settled).toBeLessThan(999);
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0][1]).toMatchObject({ around: settled });
    } finally { cleanup(); }
  });

  function mountMeasuredWindow({ follow = false, scrollHeight = 400_000 } = {}) {
    const { drainAnimationFrames } = installAnimationFrameHarness(1_000 / 60);
    const node = document.createElement("section");
    node.scrollTop = 103_000;
    Object.defineProperties(node, {
      scrollHeight: { value: scrollHeight }, clientHeight: { value: 600 },
      scrollTo: { value: (options: ScrollToOptions) => { node.scrollTop = options.top ?? 0; } },
    });
    const layout = { height: 4_000, start: 500 };
    let committed = () => {};
    const handle: VirtualizedConversationMessageListHandleRef = { current: {
      getLayoutSnapshot: () => ({ sessionId: "session", estimatedTotalHeightPx: layout.height }),
      getUserScrollGeneration: () => 5,
      subscribeToCommittedLayout: (listener: () => void) => { committed = listener; return () => { committed = () => {}; }; },
    } as unknown as VirtualizedConversationMessageListHandle };
    const scrollContainerRef = { current: node };
    const hook = renderHook(() => useConversationHistoryScroll({
      isActive: true, sessionId: "session", scrollStateKey: "pane", messageCount: 1_000,
      messageStartIndex: layout.start, renderedMessageCount: 20, tailFollowIntent: follow,
      scrollContainerRef, virtualizerHandleRef: handle,
    }));
    const commit = (next: Partial<typeof layout>) => {
      act(() => { Object.assign(layout, next); hook.rerender(); committed(); drainAnimationFrames(); });
    };
    return { node, hook, commit };
  }

  it.each([
    // An older page loads: the resident window moves from 500 to 490.
    { name: "overlapping STAY", follow: false, nextStart: 490, scrollHeight: 400_000, expectedTop: 199_000 },
    { name: "FOLLOW", follow: true, nextStart: 490, scrollHeight: 400_000, expectedTop: 103_000 },
    { name: "different window", follow: false, nextStart: 900, scrollHeight: 400_000, expectedTop: 103_000 },
    { name: "bounded correction", follow: false, nextStart: 490, scrollHeight: 120_000, expectedTop: 119_400 },
  ])("corrects prefix estimates only within the current resident authority: $name", ({ follow, nextStart, scrollHeight, expectedTop }) => {
    const { node, hook, commit } = mountMeasuredWindow({ follow, scrollHeight });
    expect(hook.result.current.prefixHeight).toBe(100_000);
    commit({ height: 8_000, start: nextStart });
    expect(hook.result.current.prefixHeight).toBe(nextStart * 400);
    expect(node.scrollTop).toBe(expectedTop);
    hook.unmount();
  });

  it.each([
    { name: "a reader scrolling the window", follow: false },
    { name: "the live tail while a response streams", follow: true },
  ])("keeps the estimate and the viewport still while resident heights change: $name", ({ follow }) => {
    const { node, hook, commit } = mountMeasuredWindow({ follow });
    expect(hook.result.current.prefixHeight).toBe(100_000);
    // Scrolling measures more resident pages, and a streaming response grows
    // the last one; their heights move in both directions.
    for (const height of [4_600, 5_200, 4_900, 6_100, 8_000]) {
      commit({ height });
      // The estimate above the window does not move: no correction write cuts
      // a smooth or momentum scroll short, and content that follows the tail
      // is never pushed down into the empty estimate between two frames.
      expect(hook.result.current.prefixHeight).toBe(100_000);
      expect(node.scrollTop).toBe(103_000);
    }
    hook.unmount();
  });
});
