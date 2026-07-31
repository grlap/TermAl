import { describe, expect, it } from "vitest";
import {
  RESPONSE_BOARD_MESSAGE_MIME,
  nextResponseBoardCardPosition,
  readResponseBoardMessageDragData,
  writeResponseBoardMessageDragData,
} from "./response-board";

class MemoryDataTransfer {
  effectAllowed = "none";
  private readonly values = new Map<string, string>();

  getData(type: string) {
    return this.values.get(type) ?? "";
  }

  setData(type: string, value: string) {
    this.values.set(type, value);
  }
}

describe("response-board drag payload", () => {
  it("serializes source ids only and rejects extra content", () => {
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );

    const encoded = dataTransfer.getData(RESPONSE_BOARD_MESSAGE_MIME);
    expect(JSON.parse(encoded)).toEqual({
      sessionId: "session-1",
      messageId: "message-1",
    });
    expect(encoded).not.toContain("snapshot");
    expect(dataTransfer.effectAllowed).toBe("copy");
    expect(
      readResponseBoardMessageDragData(dataTransfer as unknown as DataTransfer),
    ).toEqual({ sessionId: "session-1", messageId: "message-1" });

    dataTransfer.setData(
      RESPONSE_BOARD_MESSAGE_MIME,
      JSON.stringify({
        sessionId: "session-1",
        messageId: "message-1",
        content: "client supplied text",
      }),
    );
    expect(
      readResponseBoardMessageDragData(dataTransfer as unknown as DataTransfer),
    ).toBeNull();
  });

  it("chooses the first unoccupied grid position for keyboard pinning", () => {
    expect(
      nextResponseBoardCardPosition([
        { x: 64, y: 64 },
        { x: 456, y: 64 },
        { x: 1_000, y: 900 },
      ]),
    ).toEqual({ x: 848, y: 64 });
  });
});
