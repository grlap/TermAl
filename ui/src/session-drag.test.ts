import { describe, expect, it } from "vitest";

import {
  attachSessionDragData,
  dataTransferHasSessionDragType,
  readSessionDragData,
  SESSION_DRAG_MIME_TYPE,
} from "./session-drag";

describe("session drag helpers", () => {
  it("writes and reads the custom session drag payload", () => {
    const store = new Map<string, string>();
    const dataTransfer = {
      getData: (type: string) => store.get(type) ?? "",
      setData: (type: string, value: string) => {
        store.set(type, value);
      },
      types: [] as string[],
    };

    attachSessionDragData(dataTransfer, "session-1", "Session 1");
    dataTransfer.types = Array.from(store.keys());

    expect(store.get("text/plain")).toBe("TermAl session Session 1");
    expect(dataTransferHasSessionDragType(dataTransfer)).toBe(true);
    expect(readSessionDragData(dataTransfer)).toEqual({ sessionId: "session-1" });
  });

  it("rejects malformed or missing payloads", () => {
    const invalid = {
      getData: (_type: string) => "{bad json",
      types: [SESSION_DRAG_MIME_TYPE],
    };
    const missing = {
      getData: (_type: string) => "",
      types: [] as string[],
    };

    expect(readSessionDragData(invalid)).toBeNull();
    expect(readSessionDragData(missing)).toBeNull();
    expect(dataTransferHasSessionDragType(missing)).toBe(false);
  });
});
