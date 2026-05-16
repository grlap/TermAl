import { afterEach, describe, expect, it } from "vitest";
import {
  setDraftAttachmentRefs,
  setDraftRefValue,
  syncActionComposerDraftSlice,
} from "./app-session-draft-sync";
import {
  getComposerSessionSnapshotForTesting,
  resetSessionStoreForTesting,
  upsertSessionStoreSession,
} from "./session-store";
import type { DraftImageAttachment } from "./app-utils";
import type { Session } from "./types";

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

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "Session 1",
    emoji: "S",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.4",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

describe("app session draft sync helpers", () => {
  afterEach(() => {
    resetSessionStoreForTesting();
  });

  it("updates draft refs only when the value changes", () => {
    const draftsBySessionIdRef = { current: { "session-1": "hello" } };
    const original = draftsBySessionIdRef.current;

    setDraftRefValue(draftsBySessionIdRef, "session-1", "hello");
    expect(draftsBySessionIdRef.current).toBe(original);

    setDraftRefValue(draftsBySessionIdRef, "session-1", "updated");
    expect(draftsBySessionIdRef.current).toEqual({ "session-1": "updated" });
    expect(draftsBySessionIdRef.current).not.toBe(original);
  });

  it("updates and clears draft attachment refs", () => {
    const firstAttachment = attachment();
    const draftAttachmentsBySessionIdRef = {
      current: { "session-1": [firstAttachment] },
    };
    const originalAttachments = draftAttachmentsBySessionIdRef.current["session-1"];

    setDraftAttachmentRefs(
      draftAttachmentsBySessionIdRef,
      "session-1",
      [attachment({ id: "attachment-2" })],
    );
    expect(draftAttachmentsBySessionIdRef.current["session-1"]).toEqual([
      expect.objectContaining({ id: "attachment-2" }),
    ]);
    expect(draftAttachmentsBySessionIdRef.current["session-1"]).not.toBe(
      originalAttachments,
    );

    setDraftAttachmentRefs(draftAttachmentsBySessionIdRef, "session-1", []);
    expect(draftAttachmentsBySessionIdRef.current).toEqual({});
  });

  it("syncs refs and publishes the composer draft slice", () => {
    const draftsBySessionIdRef = { current: {} as Record<string, string> };
    const draftAttachmentsBySessionIdRef = {
      current: {} as Record<string, DraftImageAttachment[]>,
    };
    upsertSessionStoreSession({
      session: session(),
      committedDraft: "",
      draftAttachments: [],
    });

    syncActionComposerDraftSlice(
      { draftsBySessionIdRef, draftAttachmentsBySessionIdRef },
      "session-1",
      "draft",
      [attachment()],
    );

    expect(draftsBySessionIdRef.current).toEqual({ "session-1": "draft" });
    expect(draftAttachmentsBySessionIdRef.current["session-1"]).toHaveLength(1);
    expect(getComposerSessionSnapshotForTesting("session-1")).toMatchObject({
      committedDraft: "draft",
      draftAttachments: [expect.objectContaining({ id: "attachment-1" })],
    });
  });
});
