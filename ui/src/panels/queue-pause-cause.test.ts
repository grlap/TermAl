import { describe, expect, it } from "vitest";
import { queuePausedCardText, resolveQueuePauseCause } from "./queue-pause-cause";

describe("ordinary queue pause text", () => {
  it("names an explicit Stop and preserves the ordinary Resume guidance", () => {
    expect(queuePausedCardText(resolveQueuePauseCause("Turn stopped by user."), "Codex", 1)).toEqual({
      heading: "Codex was stopped; the queue is paused",
      detail: null,
      waiting: "1 prompt waiting. Send a new prompt or resume the queue to continue.",
    });
  });

  it("keeps a provider error as descriptive text", () => {
    const preview = "The provider is overloaded.";
    expect(queuePausedCardText(resolveQueuePauseCause(preview), "Claude", 2)).toEqual({
      heading: "The queue is paused",
      detail: preview,
      waiting: "2 prompts waiting. Send a new prompt or resume the queue to continue.",
    });
  });

  it("does not interpret historical Engram text as an active retry owner", () => {
    const preview = "Engram: waiting for admission; retrying automatically, attempt 3, next at 2026-10-05T07:40:00Z.";
    expect(resolveQueuePauseCause(preview)).toEqual({ kind: "other", preview });
    const card = queuePausedCardText(resolveQueuePauseCause(preview), "Codex", 1);
    expect(card.heading).toBe("The queue is paused");
    expect(card.detail).toBe(preview);
    expect(card.waiting).toContain("resume the queue");
  });

  it("omits absent preview text", () => {
    expect(queuePausedCardText(resolveQueuePauseCause(""), "Codex", 1).detail).toBeNull();
  });
});
