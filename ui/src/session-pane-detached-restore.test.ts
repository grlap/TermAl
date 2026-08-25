import { afterEach, describe, expect, it, vi } from "vitest";

import {
  captureDetachedPaneScrollPosition,
  createDetachedScrollRestoreController,
  preserveDetachedPaneScrollAnchor,
} from "./session-pane-detached-restore";

function installAnimationFrameQueue() {
  let nextFrameId = 1;
  const frames = new Map<number, FrameRequestCallback>();
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      frames.set(frameId, callback);
      return frameId;
    }),
  );
  vi.stubGlobal(
    "cancelAnimationFrame",
    vi.fn((frameId: number) => frames.delete(frameId)),
  );
  return frames;
}

function createScrollNode({
  clientHeight = 200,
  scrollHeight = 1_000,
  scrollTop = 100,
}: {
  clientHeight?: number;
  scrollHeight?: number;
  scrollTop?: number;
} = {}) {
  const node = document.createElement("section");
  let currentScrollHeight = scrollHeight;
  Object.defineProperties(node, {
    clientHeight: { configurable: true, value: clientHeight },
    scrollHeight: {
      configurable: true,
      get: () => currentScrollHeight,
    },
    scrollTop: { configurable: true, writable: true, value: scrollTop },
  });
  return {
    node,
    setScrollHeight(nextScrollHeight: number) {
      currentScrollHeight = nextScrollHeight;
    },
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("detached message-stack restore controller", () => {
  it("captures the first visible virtualized message as the detached anchor", () => {
    const { node } = createScrollNode({ scrollTop: 320 });
    const virtualizedList = document.createElement("div");
    virtualizedList.className = "virtualized-message-list";
    const hiddenSlot = document.createElement("div");
    hiddenSlot.className = "virtualized-message-slot";
    hiddenSlot.dataset.messageId = "message-hidden";
    const visibleSlot = document.createElement("div");
    visibleSlot.className = "virtualized-message-slot";
    visibleSlot.dataset.messageId = "message-visible";
    virtualizedList.append(hiddenSlot, visibleSlot);
    node.append(virtualizedList);
    node.getBoundingClientRect = () =>
      ({ top: 100, bottom: 300 } as DOMRect);
    hiddenSlot.getBoundingClientRect = () =>
      ({ top: 20, bottom: 80 } as DOMRect);
    visibleSlot.getBoundingClientRect = () =>
      ({ top: 124, bottom: 204 } as DOMRect);

    expect(captureDetachedPaneScrollPosition(node)).toEqual({
      anchor: {
        messageId: "message-visible",
        viewportOffsetPx: 24,
      },
      shouldStick: false,
      top: 320,
    });
  });

  it("preserves an existing anchor while absolute restore convergence advances", () => {
    expect(
      preserveDetachedPaneScrollAnchor(
        {
          anchor: { messageId: "message-42", viewportOffsetPx: 18 },
          shouldStick: false,
          top: 400,
        },
        460,
      ),
    ).toEqual({
      anchor: { messageId: "message-42", viewportOffsetPx: 18 },
      shouldStick: false,
      top: 460,
    });
  });

  it("owns its native write until the matching event is consumed", () => {
    const frames = installAnimationFrameQueue();
    const { node } = createScrollNode();
    const controller = createDetachedScrollRestoreController();
    const savedTargets: number[] = [];
    const notifications: number[] = [];
    const host = {
      getCurrentKey: () => "pane:session",
      getNode: () => node,
      isTailFollowAttached: () => false,
      notifyPositionRestore: (currentNode: HTMLElement) => {
        notifications.push(currentNode.scrollTop);
      },
      publishReachablePosition: vi.fn(),
      publishSavedTarget: (top: number) => savedTargets.push(top),
      publishUnloadedNewerHistory: vi.fn(),
    };

    controller.schedule({ host, key: "pane:session", targetTop: 600 });

    expect(node.scrollTop).toBe(600);
    expect(savedTargets).toEqual([600]);
    expect(notifications).toEqual([600]);
    expect(
      controller.consumeNativeScroll({
        key: "pane:session",
        node,
        publishSavedTarget: (top) => savedTargets.push(top),
      }),
    ).toBe(true);

    const verificationFrame = frames.entries().next().value;
    expect(verificationFrame).toBeDefined();
    if (!verificationFrame) {
      return;
    }
    frames.delete(verificationFrame[0]);
    verificationFrame[1](1000 / 60);
    node.scrollTop = 625;

    expect(
      controller.consumeNativeScroll({
        key: "pane:session",
        node,
        publishSavedTarget: (top) => savedTargets.push(top),
      }),
    ).toBe(false);
  });

  it("retries a clamped target when virtualized geometry grows", () => {
    const frames = installAnimationFrameQueue();
    const { node, setScrollHeight } = createScrollNode({
      scrollHeight: 600,
      scrollTop: 120,
    });
    const controller = createDetachedScrollRestoreController();
    const savedTargets: number[] = [];
    const host = {
      getCurrentKey: () => "pane:session",
      getNode: () => node,
      isTailFollowAttached: () => false,
      notifyPositionRestore: vi.fn(),
      publishReachablePosition: vi.fn(),
      publishSavedTarget: (top: number) => savedTargets.push(top),
      publishUnloadedNewerHistory: vi.fn(),
    };

    controller.schedule({ host, key: "pane:session", targetTop: 600 });
    expect(node.scrollTop).toBe(400);

    setScrollHeight(900);
    const retryFrame = frames.entries().next().value;
    expect(retryFrame).toBeDefined();
    if (!retryFrame) {
      return;
    }
    frames.delete(retryFrame[0]);
    retryFrame[1](1000 / 60);

    expect(node.scrollTop).toBe(600);
    expect(savedTargets).toEqual([600, 600]);
    expect(host.notifyPositionRestore).toHaveBeenCalledTimes(2);
  });

  it("rejects a retained frame after cancellation", () => {
    const frames = installAnimationFrameQueue();
    const { node, setScrollHeight } = createScrollNode({
      scrollHeight: 600,
      scrollTop: 120,
    });
    const controller = createDetachedScrollRestoreController();
    const host = {
      getCurrentKey: () => "pane:session",
      getNode: () => node,
      isTailFollowAttached: () => false,
      notifyPositionRestore: vi.fn(),
      publishReachablePosition: vi.fn(),
      publishSavedTarget: vi.fn(),
      publishUnloadedNewerHistory: vi.fn(),
    };

    controller.schedule({ host, key: "pane:session", targetTop: 600 });
    const retainedFrame = frames.values().next().value;
    expect(retainedFrame).toBeDefined();
    if (!retainedFrame) {
      return;
    }
    controller.cancel("pane:session");
    setScrollHeight(900);
    retainedFrame(1000 / 60);

    expect(node.scrollTop).toBe(400);
    expect(host.notifyPositionRestore).toHaveBeenCalledTimes(1);
  });
});
