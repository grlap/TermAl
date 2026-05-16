import { describe, expect, it } from "vitest";
import {
  deleteConversationMarkerLocally,
  upsertConversationMarkerLocally,
} from "./conversation-marker-session-mutations";
import type { ConversationMarker, Session } from "./types";

function marker(overrides: Partial<ConversationMarker> = {}): ConversationMarker {
  return {
    id: "marker-1",
    sessionId: "session-1",
    kind: "checkpoint",
    name: "Checkpoint",
    body: null,
    color: "#3b82f6",
    messageId: "message-1",
    messageIndexHint: 0,
    endMessageId: null,
    endMessageIndexHint: null,
    createdAt: "2026-05-16T10:00:00Z",
    updatedAt: "2026-05-16T10:00:00Z",
    createdBy: "user",
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

describe("conversation marker session mutations", () => {
  it("inserts a marker and records the optional mutation stamp", () => {
    const nextSession = upsertConversationMarkerLocally(
      session(),
      marker(),
      42,
    );

    expect(nextSession.markers).toEqual([marker()]);
    expect(nextSession.sessionMutationStamp).toBe(42);
  });

  it("updates an existing marker without changing marker order", () => {
    const first = marker({ id: "marker-1", name: "First" });
    const second = marker({ id: "marker-2", name: "Second" });
    const updatedSecond = marker({ id: "marker-2", name: "Updated" });

    const nextSession = upsertConversationMarkerLocally(
      session({ markers: [first, second] }),
      updatedSecond,
    );

    expect(nextSession.markers).toEqual([first, updatedSecond]);
  });

  it("deletes a marker and records the optional mutation stamp", () => {
    const first = marker({ id: "marker-1" });
    const second = marker({ id: "marker-2" });

    const nextSession = deleteConversationMarkerLocally(
      session({ markers: [first, second] }),
      "marker-1",
      77,
    );

    expect(nextSession.markers).toEqual([second]);
    expect(nextSession.sessionMutationStamp).toBe(77);
  });

  it("preserves an existing mutation stamp when the stamp argument is omitted", () => {
    const nextSession = upsertConversationMarkerLocally(
      session({ markers: [marker()], sessionMutationStamp: 12 }),
      marker({ name: "Renamed" }),
    );

    expect(nextSession.sessionMutationStamp).toBe(12);
  });

  it("clears the mutation stamp when null is supplied", () => {
    const nextSession = deleteConversationMarkerLocally(
      session({ markers: [marker()], sessionMutationStamp: 12 }),
      "marker-1",
      null,
    );

    expect(nextSession.sessionMutationStamp).toBeNull();
  });

  it("returns an empty marker list when deleting from a session without markers", () => {
    const nextSession = deleteConversationMarkerLocally(session(), "marker-1");

    expect(nextSession.markers).toEqual([]);
  });
});
