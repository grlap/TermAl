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
        onUserInputSubmit={() => {}}
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
        onUserInputSubmit={() => {}}
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
});
