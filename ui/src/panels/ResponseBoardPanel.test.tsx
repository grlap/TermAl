import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createResponseBoardCard,
  deleteResponseBoardCard,
  fetchResponseBoard,
  updateResponseBoardCard,
  type ResponseBoardCard,
} from "../api";
import {
  RESPONSE_BOARD_MESSAGE_MIME,
  writeResponseBoardMessageDragData,
} from "../response-board";
import {
  RESPONSE_BOARD_ZOOM_STORAGE_KEY,
  ResponseBoardPanel,
} from "./ResponseBoardPanel";

vi.mock("../api", () => ({
  createResponseBoardCard: vi.fn(),
  deleteResponseBoardCard: vi.fn(),
  fetchResponseBoard: vi.fn(),
  updateResponseBoardCard: vi.fn(),
}));

class MemoryDataTransfer {
  dropEffect = "none";
  effectAllowed = "none";
  private readonly values = new Map<string, string>();

  getData(type: string) {
    return this.values.get(type) ?? "";
  }

  setData(type: string, value: string) {
    this.values.set(type, value);
  }
}

const pinnedCard: ResponseBoardCard = {
  id: "card-1",
  x: 120,
  y: 80,
  w: 360,
  h: 420,
  snapshot: {
    id: "message-1",
    type: "text",
    author: "assistant",
    timestamp: "12:34:56",
    text: "Server-owned immutable response",
  },
  sourceSessionId: "session-1",
  sourceMessageId: "message-1",
  sourceMessagePosition: 42,
  sourceSessionName: "Codex research",
  sourceAgent: "Codex",
  createdAt: "2026-07-31T12:34:56Z",
};

function mockSurfaceRect(surface: Element) {
  vi.spyOn(surface, "getBoundingClientRect").mockReturnValue({
    bottom: 650,
    height: 600,
    left: 100,
    right: 900,
    top: 50,
    width: 800,
    x: 100,
    y: 50,
    toJSON: () => ({}),
  });
}

function readBoardTransform(plane: HTMLElement) {
  const match = plane.style.transform.match(
    /^translate\(([-\d.]+)px, ([-\d.]+)px\) scale\(([-\d.]+)\)$/,
  );
  expect(match).toBeTruthy();
  return {
    panX: Number(match?.[1]),
    panY: Number(match?.[2]),
    zoom: Number(match?.[3]),
  };
}

describe("ResponseBoardPanel", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    window.localStorage.removeItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY);
    vi.mocked(fetchResponseBoard).mockReset();
    vi.mocked(createResponseBoardCard).mockReset();
    vi.mocked(updateResponseBoardCard).mockReset();
    vi.mocked(deleteResponseBoardCard).mockReset();
  });

  afterEach(() => {
    window.localStorage.removeItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY);
  });

  it("mounts beside a session fixture and replaces an ids-only drop with the server snapshot", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [] });
    vi.mocked(createResponseBoardCard).mockResolvedValue(pinnedCard);
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );

    const { container } = render(
      <div className="response-board-test-workspace">
        <section data-testid="session-pane-fixture">Session transcript</section>
        <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />
      </div>,
    );

    expect(await screen.findByText("Drop an agent response anywhere on the board.")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface");
    expect(surface).toBeTruthy();
    fireEvent.drop(surface as Element, {
      dataTransfer,
      clientX: 300,
      clientY: 180,
    });

    await waitFor(() => expect(createResponseBoardCard).toHaveBeenCalledOnce());
    const request = vi.mocked(createResponseBoardCard).mock.calls[0]?.[0];
    expect(request).toMatchObject({
      sessionId: "session-1",
      messageId: "message-1",
    });
    expect(Number.isFinite(request?.x)).toBe(true);
    expect(Number.isFinite(request?.y)).toBe(true);
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    expect(screen.getByText("Codex research")).toBeTruthy();
    expect(dataTransfer.getData(RESPONSE_BOARD_MESSAGE_MIME)).not.toContain(
      "Server-owned immutable response",
    );
  });

  it("debounces explicit move geometry and removes cards through the API", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
      vi.mocked(updateResponseBoardCard).mockImplementation(
        async (_cardId, geometry) => ({ ...pinnedCard, ...geometry }),
      );
      vi.mocked(deleteResponseBoardCard).mockResolvedValue();
      const { container } = render(
        <div className="response-board-test-workspace">
          <section data-testid="session-pane-fixture">Session transcript</section>
          <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />
        </div>,
      );
      await act(async () => Promise.resolve());

      const header = container.querySelector(".response-board-card-header");
      const card = container.querySelector(".response-board-card");
      expect(header).toBeTruthy();
      expect(card).toBeTruthy();
      fireEvent.pointerDown(header as Element, {
        button: 0,
        pointerId: 7,
        clientX: 100,
        clientY: 100,
      });
      fireEvent.pointerMove(card as Element, {
        pointerId: 7,
        clientX: 180,
        clientY: 150,
      });
      expect(updateResponseBoardCard).not.toHaveBeenCalled();
      await act(async () => {
        vi.advanceTimersByTime(250);
        await Promise.resolve();
      });
      expect(updateResponseBoardCard).toHaveBeenCalledWith("card-1", {
        x: 200,
        y: 130,
        w: 360,
        h: 420,
      });

      fireEvent.click(
        screen.getByRole("button", {
          name: "Remove response from Codex research",
        }),
      );
      await act(async () => Promise.resolve());
      expect(deleteResponseBoardCard).toHaveBeenCalledWith("card-1");
      expect(screen.queryByText("Server-owned immutable response")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("sizes the board from its cards without forcing an oversized scroll plane", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });

    const { container } = render(
      <div className="response-board-test-workspace">
        <section data-testid="session-pane-fixture">Session transcript</section>
        <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />
      </div>,
    );

    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const plane = container.querySelector(
      ".response-board-plane",
    ) as HTMLElement | null;
    expect(plane).toBeTruthy();
    expect(plane?.style.width).toBe("552px");
    expect(plane?.style.height).toBe("572px");
    expect(plane?.style.minWidth).toBe("100%");
    expect(plane?.style.minHeight).toBe("100%");
  });

  it("suppresses selection only while an empty-canvas pan gesture is active", async () => {
    window.localStorage.setItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY, "0.5");
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
    const removeAllRanges = vi.fn();
    vi.spyOn(window, "getSelection").mockReturnValue({
      removeAllRanges,
    } as unknown as Selection);

    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    const cardBody = container.querySelector(".response-board-card-body") as HTMLElement;

    fireEvent.pointerDown(surface, {
      button: 0,
      pointerId: 11,
      clientX: 200,
      clientY: 180,
    });
    expect(removeAllRanges).toHaveBeenCalledOnce();
    expect(surface).toHaveClass("is-panning");
    fireEvent.pointerMove(surface, {
      pointerId: 11,
      clientX: 240,
      clientY: 200,
    });
    expect(readBoardTransform(plane)).toEqual({
      panX: 40,
      panY: 20,
      zoom: 0.5,
    });
    fireEvent.pointerUp(surface, { pointerId: 11 });
    expect(surface).not.toHaveClass("is-panning");

    fireEvent.pointerDown(cardBody, {
      button: 0,
      pointerId: 12,
      clientX: 220,
      clientY: 200,
    });
    expect(removeAllRanges).toHaveBeenCalledOnce();
    expect(surface).not.toHaveClass("is-panning");
  });

  it("keeps the logical point under the cursor stationary during ctrl-wheel zoom", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    mockSurfaceRect(surface);

    const zoomEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      ctrlKey: true,
      clientX: 300,
      clientY: 250,
      deltaY: 100,
    });
    act(() => {
      surface.dispatchEvent(zoomEvent);
    });

    const transform = readBoardTransform(plane);
    expect(zoomEvent.defaultPrevented).toBe(true);
    expect(transform.zoom).toBeLessThan(1);
    expect(transform.panX + 200 * transform.zoom).toBeCloseTo(200, 5);
    expect(transform.panY + 200 * transform.zoom).toBeCloseTo(200, 5);
    expect(Number(window.localStorage.getItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY))).toBeCloseTo(
      transform.zoom,
      5,
    );
  });

  it("supports Fn-wheel zoom without intercepting an unmodified wheel", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    mockSurfaceRect(surface);

    const ordinaryWheelEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
      deltaY: -100,
    });
    act(() => {
      surface.dispatchEvent(ordinaryWheelEvent);
    });
    expect(ordinaryWheelEvent.defaultPrevented).toBe(false);
    expect(readBoardTransform(plane)).toEqual({ panX: 0, panY: 0, zoom: 1 });

    const fnZoomEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
      deltaY: -100,
    });
    Object.defineProperty(fnZoomEvent, "getModifierState", {
      value: (modifier: string) => modifier === "Fn",
    });
    act(() => {
      surface.dispatchEvent(fnZoomEvent);
    });

    expect(fnZoomEvent.defaultPrevented).toBe(true);
    expect(readBoardTransform(plane).zoom).toBeGreaterThan(1);
  });

  it("divides card drag deltas by the persisted view scale", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY, "0.5");
    try {
      vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
      vi.mocked(updateResponseBoardCard).mockImplementation(
        async (_cardId, geometry) => ({ ...pinnedCard, ...geometry }),
      );
      const { container } = render(
        <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
      );
      await act(async () => Promise.resolve());

      const header = container.querySelector(".response-board-card-header") as HTMLElement;
      const card = container.querySelector(".response-board-card") as HTMLElement;
      fireEvent.pointerDown(header, {
        button: 0,
        pointerId: 13,
        clientX: 100,
        clientY: 100,
      });
      fireEvent.pointerMove(card, {
        pointerId: 13,
        clientX: 150,
        clientY: 125,
      });
      await act(async () => {
        vi.advanceTimersByTime(250);
        await Promise.resolve();
      });

      expect(updateResponseBoardCard).toHaveBeenCalledWith("card-1", {
        x: 220,
        y: 130,
        w: 360,
        h: 420,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("supports keyboard and toolbar zoom controls with a 25%-200% range and reset", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [] });
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Drop an agent response anywhere on the board.")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    mockSurfaceRect(surface);

    fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
    expect(screen.getByRole("button", { name: "Reset board zoom to 100%" })).toHaveTextContent(
      "83%",
    );
    fireEvent.keyDown(surface, { ctrlKey: true, key: "+" });
    expect(screen.getByRole("button", { name: "Reset board zoom to 100%" })).toHaveTextContent(
      "100%",
    );

    const zoomIn = screen.getByRole("button", { name: "Zoom in" });
    for (let index = 0; index < 20; index += 1) {
      fireEvent.click(zoomIn);
    }
    expect(screen.getByRole("button", { name: "Reset board zoom to 100%" })).toHaveTextContent(
      "200%",
    );
    fireEvent.click(screen.getByRole("button", { name: "Reset board zoom to 100%" }));
    expect(screen.getByRole("button", { name: "Reset board zoom to 100%" })).toHaveTextContent(
      "100%",
    );
  });

  it("converts drops back into logical coordinates at the active zoom", async () => {
    window.localStorage.setItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY, "0.5");
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [] });
    vi.mocked(createResponseBoardCard).mockResolvedValue(pinnedCard);
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Drop an agent response anywhere on the board.")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    mockSurfaceRect(surface);

    const dropEvent = new Event("drop", { bubbles: true, cancelable: true });
    Object.defineProperties(dropEvent, {
      clientX: { value: 300 },
      clientY: { value: 180 },
      dataTransfer: { value: dataTransfer },
    });
    act(() => {
      surface.dispatchEvent(dropEvent);
    });
    await waitFor(() =>
      expect(createResponseBoardCard).toHaveBeenCalledWith({
        sessionId: "session-1",
        messageId: "message-1",
        x: 220,
        y: 232,
      }),
    );
  });
});
