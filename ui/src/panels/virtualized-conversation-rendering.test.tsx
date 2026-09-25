// Pins page-band measurement scheduling for cold transcript activation.
// Does not test mounted-range selection or scroll restoration policy.
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Message } from "../types";
import { MeasuredPageBand } from "./virtualized-conversation-rendering";
import type { MessagePage } from "./virtualized-conversation-measurement";

const message: Message = {
  id: "message-1",
  type: "text",
  timestamp: "10:00",
  author: "assistant",
  text: "A long-session message",
};

const page: MessagePage = {
  key: "0:1:message-1:message-1",
  pageIndex: 0,
  startIndex: 0,
  endIndex: 1,
  hasTrailingGap: false,
  messages: [message],
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("MeasuredPageBand", () => {
  it.each(["immediate", "restored"] as const)("remeasures unchanged messages before paint when %s rendering expands a preview", (mode) => {
    const requestFrame = vi.fn(() => 1);
    vi.stubGlobal("requestAnimationFrame", requestFrame);
    const observe = vi.fn();
    const disconnect = vi.fn();
    vi.stubGlobal("ResizeObserver", class {
      observe = observe;
      disconnect = disconnect;
    });
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      return { height: this.querySelector("[data-full-table]") ? 4400 : 1000 } as DOMRect;
    });
    const onHeightChange = vi.fn();
    const renderPage = (expanded: boolean) => (
      <MeasuredPageBand
        isActive
        page={page}
        preferImmediateHeavyRender={mode === "immediate" && expanded}
        restoredFullMessageIds={mode === "restored" && expanded ? [message.id] : undefined}
        deferMeasurementUntilNextFrame={false}
        allowDeferredHeavyActivation={false}
        renderMessageCard={(_, immediate) => immediate
          ? <article data-full-table>Full table</article>
          : <article>Preview</article>}
        conversationSearchMatchedItemKeys={new Set()}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />
    );
    const { rerender } = render(renderPage(false));
    expect(onHeightChange).toHaveBeenLastCalledWith(
      page.key, page.pageIndex, 1000, expect.any(HTMLElement), false,
    );
    const observationCount = observe.mock.calls.length;
    onHeightChange.mockClear();
    rerender(renderPage(true));
    expect(onHeightChange).toHaveBeenCalledExactlyOnceWith(
      page.key, page.pageIndex, 4400, expect.any(HTMLElement), false,
    );
    expect(requestFrame).not.toHaveBeenCalled();
    expect(observe).toHaveBeenCalledTimes(observationCount);
    expect(disconnect).not.toHaveBeenCalled();
    onHeightChange.mockClear();
    // A fresh but value-equal list of restored IDs is not another layout change.
    rerender(renderPage(true));
    expect(onHeightChange).not.toHaveBeenCalled();
  });

  it("adopts equal hydrated references without repeating content comparisons or measurements", () => {
    vi.stubGlobal("ResizeObserver", class {
      observe() {}
      disconnect() {}
    });
    const geometryRead = vi.spyOn(Element.prototype, "getBoundingClientRect")
      .mockReturnValue({ height: 40 } as DOMRect);
    // Reading a retired object's field reveals a repeated structural comparison
    // without exposing the component's private cache through a test-only API.
    const retiredTextRead = vi.fn(() => message.text);
    const original = { ...message, get text() { return retiredTextRead(); } };
    const hydrated = { ...message };
    const onHeightChange = vi.fn();
    const renderPage = (item: Message) => (
      <MeasuredPageBand
        isActive
        page={{ ...page, messages: [item] }}
        preferImmediateHeavyRender
        deferMeasurementUntilNextFrame={false}
        allowDeferredHeavyActivation
        renderMessageCard={(value) => <article>{value.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />
    );
    const { rerender } = render(renderPage(original));
    geometryRead.mockClear();
    onHeightChange.mockClear();
    retiredTextRead.mockClear();
    rerender(renderPage(hydrated));
    expect(retiredTextRead).toHaveBeenCalled();
    expect(onHeightChange).not.toHaveBeenCalled();
    expect(geometryRead).not.toHaveBeenCalled();

    retiredTextRead.mockClear();
    rerender(renderPage(hydrated));
    expect(retiredTextRead).not.toHaveBeenCalled();
    expect(onHeightChange).not.toHaveBeenCalled();
    expect(geometryRead).not.toHaveBeenCalled();
  });

  it("remeasures changed text before a frame without reconnecting stable observers", () => {
    const requestFrame = vi.fn(() => 1);
    vi.stubGlobal("requestAnimationFrame", requestFrame);
    const cancelFrame = vi.fn();
    vi.stubGlobal("cancelAnimationFrame", cancelFrame);
    const observe = vi.fn();
    const disconnect = vi.fn();
    let notifyResize: ResizeObserverCallback | undefined;
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: ResizeObserverCallback) {
        notifyResize = callback;
      }
      observe = observe;
      disconnect = disconnect;
    });
    let height = 40;
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(
      () => ({ height }) as DOMRect,
    );
    const onHeightChange = vi.fn();
    const renderPage = (currentPage: MessagePage) => (
      <MeasuredPageBand
        isActive
        page={currentPage}
        preferImmediateHeavyRender
        deferMeasurementUntilNextFrame={false}
        allowDeferredHeavyActivation
        renderMessageCard={(item) => <article>{item.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />
    );
    const { rerender, unmount } = render(renderPage(page));
    const observationCount = observe.mock.calls.length;
    onHeightChange.mockClear();

    // An unrelated render may rebuild the page array without changing content.
    rerender(renderPage({ ...page, messages: [...page.messages] }));
    expect(onHeightChange).not.toHaveBeenCalled();
    height = 140;
    rerender(renderPage({
      ...page,
      messages: [{ ...message, text: "New streamed text occupies more lines" }],
    }));

    expect(onHeightChange).toHaveBeenCalledWith(
      page.key, page.pageIndex, 140, expect.any(HTMLElement), false,
    );
    expect(requestFrame).not.toHaveBeenCalled();
    expect(observe).toHaveBeenCalledTimes(observationCount);
    expect(disconnect).not.toHaveBeenCalled();

    act(() => notifyResize!([], {} as ResizeObserver));
    expect(requestFrame).toHaveBeenCalledTimes(1);
    onHeightChange.mockClear();
    height = 60;
    rerender(renderPage({
      ...page,
      messages: [{ ...message, text: "Shorter replacement" }],
    }));
    expect(onHeightChange).toHaveBeenCalledExactlyOnceWith(
      page.key, page.pageIndex, 60, expect.any(HTMLElement), false,
    );
    expect(cancelFrame).toHaveBeenCalledWith(1);
    unmount();
    act(() => notifyResize!([], {} as ResizeObserver));
    expect(requestFrame).toHaveBeenCalledTimes(1);
  });

  it("lets the estimated cold viewport paint before measuring page geometry", () => {
    const frameCallbacks: FrameRequestCallback[] = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      }),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    vi.stubGlobal(
      "ResizeObserver",
      class ResizeObserverMock {
        observe() {}
        disconnect() {}
      },
    );
    let slotGeometryReads = 0;
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(
      function getBoundingClientRectMock(this: Element) {
        if ((this as HTMLElement).classList.contains("virtualized-message-slot")) {
          slotGeometryReads += 1;
          return { height: 40 } as DOMRect;
        }
        return { height: 0 } as DOMRect;
      },
    );
    const onHeightChange = vi.fn();

    render(
      <MeasuredPageBand
        isActive
        page={page}
        preferImmediateHeavyRender={false}
        deferMeasurementUntilNextFrame
        allowDeferredHeavyActivation={false}
        renderMessageCard={(item) => <article>{item.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />,
    );

    expect(slotGeometryReads).toBe(0);
    expect(onHeightChange).not.toHaveBeenCalled();
    expect(frameCallbacks).toHaveLength(1);

    act(() => frameCallbacks[0]!(performance.now()));

    expect(slotGeometryReads).toBe(0);
    expect(onHeightChange).not.toHaveBeenCalled();
    expect(frameCallbacks).toHaveLength(2);

    act(() => frameCallbacks[1]!(performance.now()));

    expect(slotGeometryReads).toBe(1);
    expect(onHeightChange).toHaveBeenCalledWith(
      page.key,
      page.pageIndex,
      40,
      expect.any(HTMLElement),
      true,
    );
  });

  it("keeps warm page-band measurement synchronous", () => {
    const frameCallbacks: FrameRequestCallback[] = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      }),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    vi.stubGlobal(
      "ResizeObserver",
      class ResizeObserverMock {
        observe() {}
        disconnect() {}
      },
    );
    const geometryRead = vi
      .spyOn(Element.prototype, "getBoundingClientRect")
      .mockReturnValue({ height: 40 } as DOMRect);
    const onHeightChange = vi.fn();

    render(
      <MeasuredPageBand
        isActive
        page={page}
        preferImmediateHeavyRender={false}
        deferMeasurementUntilNextFrame={false}
        allowDeferredHeavyActivation={false}
        renderMessageCard={(item) => <article>{item.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />,
    );

    expect(geometryRead).toHaveBeenCalled();
    expect(onHeightChange).toHaveBeenCalledWith(
      page.key,
      page.pageIndex,
      40,
      expect.any(HTMLElement),
      false,
    );
    expect(frameCallbacks).toHaveLength(0);
  });

  it("routes warm ResizeObserver delivery through page-height measurement", () => {
    const frameCallbacks: FrameRequestCallback[] = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      }),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    let resizeCallback: ResizeObserverCallback | null = null;
    let resizeObserver: ResizeObserver | null = null;
    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
        resizeObserver = this as unknown as ResizeObserver;
      }
      observe() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({
      height: 40,
    } as DOMRect);
    const onHeightChange = vi.fn();

    render(
      <MeasuredPageBand
        isActive
        page={page}
        preferImmediateHeavyRender={false}
        deferMeasurementUntilNextFrame={false}
        allowDeferredHeavyActivation={false}
        renderMessageCard={(item) => <article>{item.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />,
    );
    onHeightChange.mockClear();
    act(() => {
      resizeCallback!([], resizeObserver!);
      resizeCallback!([], resizeObserver!);
    });

    expect(onHeightChange).not.toHaveBeenCalled();
    expect(frameCallbacks).toHaveLength(1);

    act(() => frameCallbacks[0]!(performance.now()));

    expect(onHeightChange).toHaveBeenCalledWith(
      page.key,
      page.pageIndex,
      40,
      expect.any(HTMLElement),
      true,
    );
  });

  it("cancels deferred measurement when the page band unmounts", () => {
    const frameCallbacks: FrameRequestCallback[] = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      }),
    );
    const cancelAnimationFrame = vi.fn();
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrame);
    vi.stubGlobal(
      "ResizeObserver",
      class ResizeObserverMock {
        observe() {}
        disconnect() {}
      },
    );
    const onHeightChange = vi.fn();
    const rendered = render(
      <MeasuredPageBand
        isActive
        page={page}
        preferImmediateHeavyRender={false}
        deferMeasurementUntilNextFrame
        allowDeferredHeavyActivation={false}
        renderMessageCard={(item) => <article>{item.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />,
    );

    expect(frameCallbacks).toHaveLength(1);
    rendered.unmount();

    expect(cancelAnimationFrame).toHaveBeenCalledWith(1);
    expect(onHeightChange).not.toHaveBeenCalled();
  });

  it("coalesces ResizeObserver flush intent into a pending cold measurement", () => {
    const frameCallbacks: FrameRequestCallback[] = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      }),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    let resizeCallback: ResizeObserverCallback | null = null;
    let resizeObserver: ResizeObserver | null = null;
    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
        resizeObserver = this as unknown as ResizeObserver;
      }
      observe() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({
      height: 40,
    } as DOMRect);
    const onHeightChange = vi.fn();

    render(
      <MeasuredPageBand
        isActive
        page={page}
        preferImmediateHeavyRender={false}
        deferMeasurementUntilNextFrame
        allowDeferredHeavyActivation={false}
        renderMessageCard={(item) => <article>{item.id}</article>}
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onSearchItemMount={() => {}}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onHeightChange={onHeightChange}
      />,
    );

    act(() => resizeCallback!([], resizeObserver!));
    expect(frameCallbacks).toHaveLength(1);
    act(() => frameCallbacks[0]!(performance.now()));
    expect(frameCallbacks).toHaveLength(2);
    act(() => frameCallbacks[1]!(performance.now()));

    expect(onHeightChange).toHaveBeenCalledTimes(1);
    expect(onHeightChange).toHaveBeenCalledWith(
      page.key,
      page.pageIndex,
      40,
      expect.any(HTMLElement),
      true,
    );
  });
});
