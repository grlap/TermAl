import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

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
import { ResponseBoardPanel } from "./ResponseBoardPanel";

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

describe("ResponseBoardPanel", () => {
  beforeEach(() => {
    vi.mocked(fetchResponseBoard).mockReset();
    vi.mocked(createResponseBoardCard).mockReset();
    vi.mocked(updateResponseBoardCard).mockReset();
    vi.mocked(deleteResponseBoardCard).mockReset();
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
});
