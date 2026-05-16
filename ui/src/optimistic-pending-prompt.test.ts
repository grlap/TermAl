import { describe, expect, it } from "vitest";
import { createOptimisticPendingPrompt } from "./optimistic-pending-prompt";
import type { DraftImageAttachment } from "./app-utils";

function attachment(overrides: Partial<DraftImageAttachment> = {}): DraftImageAttachment {
  return {
    id: "attachment-1",
    base64Data: "abc123",
    byteSize: 12,
    fileName: "image.png",
    mediaType: "image/png",
    previewUrl: "blob:preview",
    ...overrides,
  };
}

describe("optimistic pending prompts", () => {
  it("creates local-only prompt metadata without retaining image payload data", () => {
    const prompt = createOptimisticPendingPrompt(
      "session-1",
      "hello",
      "expanded",
      [attachment()],
      new Date("2026-05-16T10:30:00Z"),
    );

    expect(prompt).toMatchObject({
      attachments: [
        {
          byteSize: 12,
          fileName: "image.png",
          mediaType: "image/png",
        },
      ],
      expandedText: "expanded",
      localOnly: true,
      text: "hello",
    });
    expect(prompt.id).toMatch(/^optimistic-send-session-1-/);
    expect(JSON.stringify(prompt)).not.toContain("base64Data");
    expect(JSON.stringify(prompt)).not.toContain("previewUrl");
  });

  it("generates distinct ids for same-session prompts created in the same millisecond", () => {
    const now = new Date("2026-05-16T10:30:00Z");

    const first = createOptimisticPendingPrompt("session-1", "one", null, [], now);
    const second = createOptimisticPendingPrompt("session-1", "two", null, [], now);

    expect(first.id).not.toBe(second.id);
  });
});
