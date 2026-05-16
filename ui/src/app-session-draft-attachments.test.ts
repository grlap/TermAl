import { describe, expect, it } from "vitest";
import type { DraftImageAttachment } from "./app-utils";
import {
  appendDraftAttachments,
  draftAttachmentsMatchingId,
  draftAttachmentsWithoutId,
  removeDraftAttachmentFromState,
} from "./app-session-draft-attachments";

function attachment(id: string): DraftImageAttachment {
  return {
    base64Data: id,
    byteSize: id.length,
    fileName: `${id}.png`,
    id,
    mediaType: "image/png",
    previewUrl: `blob:${id}`,
  };
}

describe("app session draft attachment helpers", () => {
  it("appends attachments for a session without mutating existing state", () => {
    const first = attachment("first");
    const second = attachment("second");
    const current = { "session-1": [first] };

    const next = appendDraftAttachments(current, "session-1", [second]);

    expect(next).toEqual({ "session-1": [first, second] });
    expect(next).not.toBe(current);
    expect(next["session-1"]).not.toBe(current["session-1"]);
    expect(current).toEqual({ "session-1": [first] });
  });

  it("finds and removes matching attachment ids from attachment arrays", () => {
    const first = attachment("first");
    const second = attachment("second");
    const attachments = [first, second];

    expect(draftAttachmentsMatchingId(attachments, "second")).toEqual([second]);
    expect(draftAttachmentsWithoutId(attachments, "second")).toEqual([first]);
  });

  it("removes one attachment from state", () => {
    const first = attachment("first");
    const second = attachment("second");
    const current = {
      "session-1": [first, second],
      "session-2": [attachment("other")],
    };

    expect(removeDraftAttachmentFromState(current, "session-1", "second")).toEqual({
      "session-1": [first],
      "session-2": [attachment("other")],
    });
  });

  it("deletes empty session entries and preserves state when nothing changes", () => {
    const first = attachment("first");
    const current = { "session-1": [first] };

    expect(removeDraftAttachmentFromState(current, "session-1", "first")).toEqual(
      {},
    );
    expect(removeDraftAttachmentFromState(current, "session-1", "missing")).toBe(
      current,
    );
    expect(removeDraftAttachmentFromState(current, "missing", "first")).toBe(
      current,
    );
  });
});
