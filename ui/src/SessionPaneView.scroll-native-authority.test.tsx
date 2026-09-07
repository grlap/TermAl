// Real pane and virtualizer listeners share one node. Browser geometry changes
// and native event delivery are independent of application scroll writes.
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { useLayoutEffect, type UIEvent as ReactUIEvent } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useSessionPaneScrollState } from "./SessionPaneView.scroll";
import { installAnimationFrameHarness, params, session } from "./SessionPaneView.scroll.fixtures";
import { MESSAGE_STACK_USER_SCROLL_INTENT_EVENT, peekMessageStackNativeScrollOwnership } from "./message-stack-scroll-sync";
import { VirtualizedConversationMessageList } from "./panels/VirtualizedConversationMessageList";
import type { PaneScrollPosition } from "./pane-scroll-position-migration";
import type { Message } from "./types";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function renderNativeConversation(
  paneFirst: boolean,
  detachedRestoreTop?: number,
  reader?: { anchor: NonNullable<PaneScrollPosition["anchor"]>; ordinary?: boolean },
) {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const frames = installAnimationFrameHarness(1_000 / 60);
  const observers = new Map<ResizeObserverCallback, Set<Element>>();
  vi.stubGlobal("ResizeObserver", class {
    constructor(private readonly callback: ResizeObserverCallback) {
      observers.set(callback, new Set());
    }
    observe(target: Element) { observers.get(this.callback)?.add(target); }
    unobserve(target: Element) { observers.get(this.callback)?.delete(target); }
    disconnect() { observers.delete(this.callback); }
  });
  let height = 1_000;
  let top = 800;
  const writes: number[] = [];
  const slotTops = new Map<string, number>();
  let node!: HTMLElement;
  let page!: HTMLDivElement;
  const bindNode = (mounted: HTMLElement | null) => {
    if (!mounted) return;
    node = mounted;
    state.messageStackRef.current = node;
    Object.defineProperties(node, {
      clientHeight: { get: () => 200 },
      clientWidth: { get: () => 1_000 },
      scrollHeight: { get: () => height },
      scrollTop: {
        get: () => top,
        set: (value: number) => { top = value; writes.push(value); },
      },
      scrollTo: { value: ({ top: value }: ScrollToOptions) => {
        if (typeof value === "number") node.scrollTop = value;
      } },
    });
  };
  const bindPage = (mounted: HTMLDivElement | null) => {
    if (mounted) page = mounted;
  };
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const measuredHeight = this === node ? 200 : height;
    const messageId = (this as HTMLElement).dataset?.messageId;
    const measuredTop = this === node ? 0 : (slotTops.get(messageId ?? "") ?? 0) - top;
    return {
      height: measuredHeight, width: 1_000, top: measuredTop,
      bottom: measuredTop + measuredHeight, left: 0, right: 1_000,
      x: 0, y: measuredTop, toJSON: () => ({}),
    };
  });
  let activeSession = {
    ...session(false), hasOlderHistory: false, messagesLoaded: true, messageCount: 1,
  };
  const key = "pane-1:session-history";
  let childMessages = activeSession.messages;
  const shared = {
    ...params(activeSession),
    paneScrollPositions: { [key]: {
      top: detachedRestoreTop ?? top,
      shouldStick: detachedRestoreTop === undefined,
      ...(reader ? { anchor: reader.anchor } : {}),
    } } as Record<string, PaneScrollPosition>,
    paneShouldStickToBottomRef: { current: { [key]: detachedRestoreTop === undefined } },
  };
  let state!: ReturnType<typeof useSessionPaneScrollState>;
  let props = { visible: false, waiting: false, revision: 0, searching: false };
  function Conversation({ visible, waiting, revision, searching }: typeof props) {
    state = useSessionPaneScrollState({
      ...shared, activeSession, isActive: true, isSessionTabActive: visible,
      hasSessionFindQuery: searching,
      activeSessionSearchMatch: searching ? {
        itemId: "search-message", itemKey: "search-message", itemKind: "message", snippet: "match",
      } : null,
      showWaitingIndicator: waiting,
      visibleContentSignature: `content-${revision}`,
      visibleMessageContentSignature: `message-${revision}`,
    });
    const handleScroll = state.handleMessageStackScroll;
    useLayoutEffect(() => {
      const listener = (event: Event) => handleScroll({
        currentTarget: node, nativeEvent: event,
      } as ReactUIEvent<HTMLElement>);
      node.addEventListener("scroll", listener, paneFirst);
      return () => node.removeEventListener("scroll", listener, paneFirst);
    }, [handleScroll]);
    return <section
      ref={bindNode}
      className="message-stack"
      onWheel={state.handleMessageStackUserScrollIntent}
      onTouchStart={state.handleMessageStackTouchStart}
      onTouchMove={state.handleMessageStackUserScrollIntent}
      onKeyDown={state.handleMessageStackUserScrollIntent}
      onMouseDown={state.handleMessageStackUserScrollIntent}
      onFocusCapture={state.handleMessageStackFocusCapture}
    ><div ref={bindPage} className="session-conversation-page">{visible ? reader?.ordinary ?
      childMessages.map((message) => <div key={message.id} className="message-slot" data-message-id={message.id}>
        <article className="message-card">{message.id}</article>
      </div>) :
      <VirtualizedConversationMessageList
        isActive
        tailFollowIntentIsAuthoritative
        sessionId={activeSession.id}
        messages={childMessages}
        scrollContainerRef={state.messageStackRef}
        virtualizerHandleRef={state.virtualizerHandleRef}
        tailFollowIntent={state.liveTailPinned}
        renderMessageCard={(message) => <article className="message-card">{message.id}</article>}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        conversationSearchQuery=""
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
      /> : null}</div></section>;
  }
  const rendered = render(<Conversation {...props} />);
  const update = (changes: Partial<typeof props>) => {
    props = { ...props, ...changes };
    rendered.rerender(<Conversation {...props} />);
  };
  update({ visible: true });
  if (detachedRestoreTop === undefined) {
    act(() => state.scrollMessageStackToBoundary("bottom"));
    frames.drainAnimationFrames();
    act(() => vi.runOnlyPendingTimers());
    frames.drainAnimationFrames();
    act(() => node.dispatchEvent(new Event("scroll")));
  }
  const userIntents = vi.fn();
  node.addEventListener(MESSAGE_STACK_USER_SCROLL_INTENT_EVENT, userIntents);
  writes.length = 0;
  const notify = (target: Element) => act(() => {
    for (const [callback, targets] of observers) {
      if (targets.has(target)) callback([], {} as ResizeObserver);
    }
  });
  return {
    node, page, writes, userIntents, update, frames,
    get savedPosition() { return shared.paneScrollPositions[key]; },
    get state() { return state; },
    hydrate(messages: Message[], positions: Record<string, number>, deferChild = false) {
      for (const [id, value] of Object.entries(positions)) slotTops.set(id, value);
      activeSession = { ...activeSession, messages };
      if (!deferChild) childMessages = messages;
      update({ revision: props.revision + 1 });
    },
    commitChild() {
      childMessages = activeSession.messages;
      update({});
    },
    browserHeight(next: number) {
      height = next;
      top = Math.min(top, Math.max(0, height - 200));
    },
    browserTop(next: number) { top = next; },
    nativeScroll() { act(() => node.dispatchEvent(new Event("scroll"))); },
    measure() {
      const slot = page.querySelector(".virtualized-message-slot");
      expect(slot).not.toBeNull();
      notify(slot!);
      notify(page);
      frames.drainAnimationFrames();
    },
    output(nextHeight: number) {
      height = nextHeight;
      activeSession = {
        ...activeSession,
        messages: [...activeSession.messages, {
          id: `output-${props.revision + 1}`, type: "text", author: "assistant",
          timestamp: "12:01", text: `New output ${props.revision + 1}`,
        }],
      };
      childMessages = activeSession.messages;
      update({ revision: props.revision + 1 });
      frames.drainAnimationFrames();
    },
    dispose() { cleanup(); },
  };
}

describe("native scroll attachment authority", () => {
  it.each([false, true])("restores the original signed fractional anchor after numeric fallback finishes (ordinary=%s)", (ordinary) => {
    const anchor = { messageId: "returning-reader-message", viewportOffsetPx: -12.25 };
    const view = renderNativeConversation(false, 400, { anchor, ordinary });
    view.frames.drainAnimationFrames();
    expect(view.node.scrollTop).toBe(400);
    view.nativeScroll();
    act(() => view.state.captureDetachedMessageStackPosition());
    expect(view.savedPosition.anchor).toEqual(anchor);

    const messages: Message[] = [{
      id: anchor.messageId, type: "text", author: "assistant", timestamp: "12:01",
      text: "The same reading point, after history and estimates above it changed",
    }];
    view.hydrate(messages, { [anchor.messageId]: 735.5 }, true);
    expect(view.node.querySelector(`[data-message-id="${anchor.messageId}"]`)).toBeNull();
    view.commitChild();
    view.frames.drainAnimationFrames();
    const slot = view.node.querySelector(`[data-message-id="${anchor.messageId}"]`);
    expect(slot).not.toBeNull();
    expect(slot!.getBoundingClientRect().top - view.node.getBoundingClientRect().top).toBe(-12.25);
    expect(view.node.scrollTop).toBe(747.75);
    expect(view.savedPosition.anchor).toEqual(anchor);
    expect(view.state.liveTailPinned).toBe(false);
    const restoredTop = view.node.scrollTop;
    view.update({ revision: 10 });
    expect(view.node.scrollTop).toBe(restoredTop);
  });

  it.each(["wheel", "follow", "tab", "handle"] as const)("lets %s navigation replace a missing reader restore", (navigation) => {
    const anchor = { messageId: "returning-reader-message", viewportOffsetPx: -12.25 };
    const view = renderNativeConversation(false, 400, { anchor });
    view.frames.drainAnimationFrames();
    if (navigation === "wheel") fireEvent.wheel(view.node, { deltaY: -60 });
    if (navigation === "follow") act(() => view.state.scrollMessageStackToBoundary("bottom"));
    if (navigation === "tab") view.update({ visible: false });
    if (navigation === "handle") act(() => { view.state.virtualizerHandleRef.current!.beginUserScrollNavigation(); });
    const topBeforeHydration = view.node.scrollTop;
    view.hydrate([{
      id: anchor.messageId, type: "text", author: "assistant", timestamp: "12:01", text: "Late history",
    }], { [anchor.messageId]: 735.5 });
    view.frames.drainAnimationFrames();
    expect(view.node.scrollTop).not.toBe(747.75);
    if (navigation !== "follow") expect(view.node.scrollTop).toBe(topBeforeHydration);
  });

  it("keeps the original reader anchor through rapid hide/show and rejects the old activation callback", () => {
    const anchor = { messageId: "returning-reader-message", viewportOffsetPx: -12.25 };
    const view = renderNativeConversation(false, 400, { anchor });
    view.frames.drainAnimationFrames();
    const oldHandle = view.state.virtualizerHandleRef.current!;
    const oldRestore = vi.spyOn(oldHandle, "restoreViewportAnchor");
    view.update({ revision: 1 });
    const oldOptions = oldRestore.mock.calls[0]?.[1];
    expect(oldOptions).toBeDefined();
    act(() => view.state.captureDetachedMessageStackPosition());
    view.update({ visible: false });
    view.update({ visible: true });
    expect(oldOptions!.isCurrent()).toBe(false);
    expect(view.savedPosition.anchor).toEqual(anchor);
    view.hydrate([{
      id: anchor.messageId, type: "text", author: "assistant", timestamp: "12:01", text: "Returned history",
    }], { [anchor.messageId]: 735.5 });
    view.frames.drainAnimationFrames();
    const slot = view.node.querySelector(`[data-message-id="${anchor.messageId}"]`)!;
    expect(slot.getBoundingClientRect().top).toBe(-12.25);
    const restored = view.savedPosition;
    act(() => oldOptions!.onRestored());
    expect(view.savedPosition).toBe(restored);
  });

  it("lets a search selection replace the newly persistent missing reader request", () => {
    const anchor = { messageId: "returning-reader-message", viewportOffsetPx: -12.25 };
    const view = renderNativeConversation(false, 400, { anchor, ordinary: true });
    view.frames.drainAnimationFrames();
    const target = view.node.querySelector<HTMLElement>(".message-slot")!;
    target.scrollIntoView = vi.fn(() => { view.node.scrollTop = 200; });
    act(() => view.state.handleConversationSearchItemMount("search-message", target));
    // Persistent reader restoration is new. Search is explicit navigation,
    // not the passive STAY recapture that now deliberately preserves it.
    view.update({ searching: true });
    expect(target.scrollIntoView).toHaveBeenCalled();
    expect(view.node.scrollTop).toBe(200);
    view.update({ searching: false });
    view.hydrate([{
      id: anchor.messageId, type: "text", author: "assistant", timestamp: "12:01", text: "Late history",
    }], { [anchor.messageId]: 735.5 });
    view.frames.drainAnimationFrames();
    expect(view.node.scrollTop).toBe(200);
    expect(view.node.scrollTop).not.toBe(747.75);
  });

  it.each(["wheel", "keyboard", "touch", "pointer"] as const)(
    "preserves real %s escape and its late native momentum", (input) => {
      const view = renderNativeConversation(false);
      try {
        if (input === "wheel") fireEvent.wheel(view.node, { deltaY: -40 });
        if (input === "keyboard") fireEvent.keyDown(view.node, { key: "ArrowUp" });
        if (input === "pointer") {
          fireEvent.mouseDown(view.node);
          fireEvent.mouseUp(document);
        }
        if (input === "touch") {
          fireEvent.touchStart(view.node, { touches: [{ clientY: 100 }] });
          fireEvent.touchMove(view.node, { touches: [{ clientY: 140 }] });
        }
        expect(view.state.liveTailPinned).toBe(false);
        const now = performance.now();
        vi.spyOn(performance, "now").mockReturnValue(now + 10_000);
        expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
        view.writes.length = 0;
        view.browserTop(650);
        view.nativeScroll();
        view.browserTop(620);
        view.nativeScroll();
        view.output(1_100);
        expect(view.state.liveTailPinned).toBe(false);
        expect(view.node.scrollTop).toBe(620);
        expect(view.writes).toEqual([]);
      } finally { view.dispose(); }
    },
  );

  it.each([true, false])("owns a held descendant selection drag while streaming (pane first=%s)", (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      view.update({ waiting: true });
      view.frames.drainAnimationFrames();
      view.writes.length = 0;
      fireEvent.mouseDown(view.page.querySelector(".message-card")!);
      expect(view.state.liveTailPinned).toBe(true);
      const now = performance.now();
      vi.spyOn(performance, "now").mockReturnValue(now + 10_000);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toEqual({ owner: "pointer", direction: null });
      view.browserTop(600);
      view.nativeScroll();
      view.output(1_100);
      expect(view.state.liveTailPinned).toBe(false);
      expect(view.node.scrollTop).toBe(600);
      expect(view.writes).toEqual([]);
      fireEvent.mouseUp(document);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      view.browserTop(580);
      view.nativeScroll();
      view.output(1_200);
      expect(view.node.scrollTop).toBe(580);
      expect(view.writes).toEqual([]);
    } finally { view.dispose(); }
  });

  it("does not detach for a descendant click without movement", () => {
    const view = renderNativeConversation(false);
    try {
      fireEvent.mouseDown(view.page.querySelector(".message-card")!);
      view.nativeScroll();
      expect(view.state.liveTailPinned).toBe(true);
      fireEvent.mouseUp(document);
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.state.showNewResponseIndicator).toBe(false);
      view.output(1_100);
      expect(view.node.scrollTop).toBe(900);
    } finally { view.dispose(); }
  });

  it.each([true, false])("does not retain a context-menu pointer lease when mouseup is lost (pane first=%s)", (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      const card = view.page.querySelector(".message-card")!;
      fireEvent.mouseDown(card, { button: 2, buttons: 2 });
      fireEvent.contextMenu(card);
      // Native menus can consume release outside the document. No mouseup is delivered.
      view.browserHeight(600);
      view.browserHeight(1_000);
      view.nativeScroll();
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.userIntents).not.toHaveBeenCalled();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      view.output(1_100);
      expect(view.node.scrollTop).toBe(900);
    } finally { view.dispose(); }
  });

  it.each([1, 2])("does not treat mouse button %s as a held primary scrollbar drag", (button) => {
    const view = renderNativeConversation(false);
    try {
      fireEvent.mouseDown(view.node, { button });
      expect(view.state.liveTailPinned).toBe(true);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
    } finally { view.dispose(); }
  });

  it.each(["contextmenu", "dragend"] as const)("ends a primary drag on %s even when propagation is stopped", (release) => {
    const view = renderNativeConversation(false);
    try {
      const card = view.page.querySelector(".message-card")!;
      fireEvent.mouseDown(card, { button: 0, buttons: 1, ctrlKey: true });
      expect(peekMessageStackNativeScrollOwnership(view.node)?.owner).toBe("pointer");
      card.addEventListener(release, (event) => event.stopPropagation());
      fireEvent(card, new MouseEvent(release, { bubbles: true, cancelable: true }));
      view.browserHeight(600);
      view.browserHeight(1_000);
      view.nativeScroll();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.userIntents).not.toHaveBeenCalled();
    } finally { view.dispose(); }
  });

  it.each([true, false])("reattaches at a proven downward focus landing before delayed native delivery (pane first=%s)", async (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      const first = document.createElement("button");
      const last = document.createElement("button");
      view.page.append(first, last);
      act(() => { first.focus(); view.browserTop(600); });
      await act(async () => { await Promise.resolve(); });
      view.nativeScroll();
      expect(view.state.liveTailPinned).toBe(false);
      act(() => { last.focus(); view.browserTop(800); });
      await act(async () => { await Promise.resolve(); });
      expect(view.state.liveTailPinned).toBe(true);
      view.nativeScroll();
      view.output(1_100);
      expect(view.node.scrollTop).toBe(900);
      expect(view.state.liveTailPinned).toBe(true);
    } finally { view.dispose(); }
  });

  it.each([true, false])("cancels a queued detached restore as soon as focus movement is proven (pane first=%s)", async (paneFirst) => {
    const view = renderNativeConversation(paneFirst, 1_200);
    try {
      expect(view.savedPosition.top).toBe(1_200);
      expect(view.frames.animationFrames.size).toBeGreaterThan(0);
      const focused = document.createElement("button");
      view.page.append(focused);
      act(() => {
        focused.focus();
        view.browserTop(600);
      });
      await act(async () => { await Promise.resolve(); });
      view.browserHeight(1_600);
      view.writes.length = 0;
      // Run the queued restore before delivering the focus scroll event.
      view.frames.drainAnimationFrames();
      expect(view.node.scrollTop).toBe(600);
      expect(view.writes).toEqual([]);
      expect(view.savedPosition.top).toBe(600);
      expect(view.userIntents.mock.calls.filter(([event]) => !event.detail.pendingNativeMovement)).toHaveLength(1);
      view.userIntents.mockClear();
      view.nativeScroll();
      expect(view.userIntents).not.toHaveBeenCalled();
      expect(view.savedPosition.top).toBe(600);
      expect(view.state.liveTailPinned).toBe(false);
    } finally { view.dispose(); }
  });

  it.each([true, false])("does not republish focus navigation on a direction-conflicting native tick (pane first=%s)", async (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      // Native baseline is 800, but an undelivered clamp precedes focus 400 -> 600.
      view.browserHeight(600);
      view.browserHeight(1_000);
      const focused = document.createElement("button");
      view.page.append(focused);
      act(() => { focused.focus(); view.browserTop(600); });
      await act(async () => { await Promise.resolve(); });
      expect(view.userIntents.mock.calls.filter(([event]) => !event.detail.pendingNativeMovement)).toHaveLength(1);
      view.userIntents.mockClear();
      view.nativeScroll();
      expect(view.userIntents).not.toHaveBeenCalled();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      expect(view.state.liveTailPinned).toBe(false);
      view.output(1_100);
      expect(view.node.scrollTop).toBe(600);
    } finally { view.dispose(); }
  });

  it.each([true, false])("preserves browser-owned Space motion during growth despite an older saved bottom (pane first=%s)", (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      view.browserHeight(1_400);
      view.browserTop(1_100);
      const event = new KeyboardEvent("keydown", { key: " ", shiftKey: true, bubbles: true, cancelable: true });
      act(() => view.node.dispatchEvent(event));
      expect(event.defaultPrevented).toBe(false);
      view.browserTop(1_000); // Up from 1100, but still down from the old saved 800.
      view.nativeScroll();
      expect(view.state.liveTailPinned).toBe(false);
      expect(view.savedPosition.top).toBe(1_000);
      view.output(1_500);
      expect(view.node.scrollTop).toBe(1_000);
    } finally { view.dispose(); }
  });

  it.each([true, false])("keeps preventScroll focus attached, including later layout clamps (pane first=%s)", async (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      const focused = document.createElement("button");
      view.page.append(focused);
      act(() => focused.focus({ preventScroll: true }));
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.state.showNewResponseIndicator).toBe(false);
      await act(async () => { await Promise.resolve(); });
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      view.browserHeight(600);
      view.browserHeight(1_000);
      view.nativeScroll();
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.state.showNewResponseIndicator).toBe(false);
      expect(view.writes).toEqual([]);
      view.output(1_100);
      expect(view.node.scrollTop).toBe(900);
    } finally { view.dispose(); }
  });

  it.each([true, false])("keeps actual offscreen focus movement owned through delayed native delivery (pane first=%s)", async (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      const focused = document.createElement("button");
      view.page.append(focused);
      act(() => {
        focused.focus();
        // JSDOM does not implement the browser's synchronous focus scroll.
        view.browserTop(600);
      });
      expect(view.state.liveTailPinned).toBe(true);
      await act(async () => { await Promise.resolve(); });
      const now = performance.now();
      vi.spyOn(performance, "now").mockReturnValue(now + 10_000);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toEqual({ owner: "focus", direction: "up" });
      view.writes.length = 0;
      view.nativeScroll();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      view.output(1_100);
      expect(view.node.scrollTop).toBe(600);
      expect(view.writes).toEqual([]);
      expect(view.state.liveTailPinned).toBe(false);
    } finally { view.dispose(); }
  });

  it.each(["blur", "bottom command", "unmount"] as const)("invalidates pending focus on %s", async (action) => {
    const view = renderNativeConversation(false);
    try {
      const focused = document.createElement("button");
      view.page.append(focused);
      act(() => {
        focused.focus();
        view.browserTop(600);
      });
      await act(async () => { await Promise.resolve(); });
      if (action === "blur") act(() => focused.blur());
      if (action === "bottom command") act(() => view.state.scrollMessageStackToBoundary("bottom"));
      if (action === "unmount") view.dispose();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
    } finally { view.dispose(); }
  });

  it.each([
    [true, false], [false, false], [true, true], [false, true],
  ])("preserves FOLLOW after delayed shrink-regrow (pane first=%s, turn ends=%s)", (paneFirst, turnEnds) => {
    const view = renderNativeConversation(paneFirst);
    try {
      if (turnEnds) {
        view.update({ waiting: true });
        view.update({ waiting: false });
        view.frames.drainAnimationFrames();
        view.writes.length = 0;
      }
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.node.dataset.virtualizedBottomBoundaryReveal).not.toBe("true");
      view.browserHeight(600);
      view.browserHeight(1_000);
      view.nativeScroll();
      expect(view.node.scrollTop).toBe(400);
      expect(view.writes).toEqual([]);
      expect(view.userIntents).not.toHaveBeenCalled();
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.state.showNewResponseIndicator).toBe(false);
      view.output(1_100);
      expect(view.node.scrollTop).toBe(900);
      expect(view.state.liveTailPinned).toBe(true);
    } finally { view.dispose(); }
  });

  it.each([true, false])("keeps the idle deadband with delayed native events (pane first=%s)", (paneFirst) => {
    const view = renderNativeConversation(paneFirst);
    try {
      for (const height of [999, 1_000, 998, 1_000]) {
        view.browserHeight(height);
        view.measure();
        view.nativeScroll();
      }
      expect(view.writes).toEqual([]);
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.userIntents).not.toHaveBeenCalled();
    } finally { view.dispose(); }
  });

  it.each(["before", "after"] as const)("preserves FOLLOW with coalesced measurement %s native delivery", (measurement) => {
    const view = renderNativeConversation(false);
    try {
      view.browserHeight(600);
      view.browserHeight(1_000);
      if (measurement === "before") view.measure();
      view.nativeScroll();
      if (measurement === "after") view.measure();
      expect(view.state.liveTailPinned).toBe(true);
      expect(view.node.scrollTop).toBe(400);
      expect(view.writes).toEqual([]);
      expect(view.userIntents).not.toHaveBeenCalled();
      view.output(1_100);
      expect(view.node.scrollTop).toBe(900);
    } finally { view.dispose(); }
  });
});
