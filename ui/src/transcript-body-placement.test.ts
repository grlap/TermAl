// Placement discriminates actual body work from reducer metadata fallback.
import { expect, it } from "vitest";
import { applyBodyDeltaToRange } from "./transcript-body-placement";
import type { Session, Message } from "./types";
const message = (id: string): Message => ({ id, type: "text", author: "assistant", timestamp: "now", text: id });
const session: Session = { id: "a", name: "A", emoji: "AI", agent: "Codex", model: "default",
  workdir: "C:/repo", status: "active", preview: "", messages: [message("m")],
  messageCount: 1, messageStartIndex: 0, messagesLoaded: true, hasNewerHistory: false };
it("places a structural insertion in the attached window", () => {
  const next = applyBodyDeltaToRange(session, { type: "messageCreated", revision: 1, sessionId: "a",
    messageId: "new", messageIndex: 0, messageCount: 2, message: message("new"), status: "active", preview: "" });
  expect(next?.messages.map(m => m.id)).toEqual(["new", "m"]);
});
it("refuses inconsistent identity/count and divergent text offsets", () => {
  expect(applyBodyDeltaToRange(session, { type: "textDelta", revision: 1, sessionId: "a",
    messageId: "m", messageIndex: 0, messageCount: 1, textStartByte: 99, delta: "suffix" })).toBeNull();
  expect(applyBodyDeltaToRange(session, { type: "messageCreated", revision: 1, sessionId: "a",
    messageId: "new", messageIndex: 0, messageCount: 9, message: message("new"), status: "active", preview: "" })).toBeNull();
});
