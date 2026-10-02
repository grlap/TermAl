import { describe, expect, it } from "vitest";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import { applyDeltaToSessions } from "./live-updates";
import type { Session } from "./types";

function session(id: string): Session {
  return { id, name: id, emoji: "AI", agent: "Codex", workdir: "C:/workspace",
    model: "codex", status: "idle", preview: "", messages: [], messagesLoaded: true };
}

describe("local attached-tail slice", () => {
  const text = (id: string, body: string) => ({ id, type: "text" as const,
    author: "assistant" as const, timestamp: "10:00", text: body });
  const loaded = (): Session => ({ ...session("local"), bodySeq: 0,
    bodySeqEpoch: "local-instance", sessionMutationStamp: 1,
    messages: [text("head", "Older history"), text("middle", "Middle"), text("last", "Cut")],
    messageCount: 3, messageStartIndex: 0, hasOlderHistory: false, hasNewerHistory: false });

  it("replaces a fully loaded stale suffix and retains only the id-aligned head", () => {
    const initial = loaded();
    const owner = new TranscriptRepairAuthority(undefined, [initial]);
    owner.setServerInstance("local-instance");
    const tail = { ...initial, bodySeq: 1, sessionMutationStamp: 2,
      messages: [text("middle", "Middle"), text("last", "Complete final paragraph")],
      messageStartIndex: 1, messagesLoaded: false, hasOlderHistory: true };
    owner.adoptTail(tail);
    const adopted = owner.sessionsRef.current[0];
    expect(adopted.messages).toEqual([initial.messages[0], ...tail.messages]);
    expect(adopted.messages[0]).toBe(initial.messages[0]);
    expect(adopted.messageStartIndex).toBe(0);
    expect(adopted.messagesLoaded).toBe(true);
    expect(adopted.hasOlderHistory).toBe(false);
  });

  it.each(["different-boundary", "no-overlap"])("does not retain a head with %s", boundary => {
    const initial = loaded();
    const owner = new TranscriptRepairAuthority(undefined, [initial]);
    owner.setServerInstance("local-instance");
    const tail = { ...initial, bodySeq: 1, sessionMutationStamp: 2,
      messages: [text("replacement", "Complete final paragraph")],
      messageStartIndex: boundary === "no-overlap" ? 5 : 2,
      messageCount: boundary === "no-overlap" ? 6 : 3,
      messagesLoaded: false, hasOlderHistory: true };
    owner.adoptTail(tail);
    expect(owner.sessionsRef.current[0].messages).toEqual(tail.messages);
    expect(owner.sessionsRef.current[0].messageStartIndex).toBe(tail.messageStartIndex);
    expect(owner.sessionsRef.current[0].hasOlderHistory).toBe(true);
  });

  it("keeps resident bodies visible when lagged makes the attached stream dirty", () => {
    const owner = new TranscriptRepairAuthority(undefined, [loaded()]);
    owner.setServerInstance("local-instance");
    owner.adoptTail(loaded());
    const resident = owner.sessionsRef.current[0];
    owner.declareLoss("lagged");
    expect(owner.sessionsRef.current[0].messages).toBe(resident.messages);
    expect(owner.sessionsRef.current[0].messagesLoaded).toBe(true);
  });

  it.each(["remote", "foreign-epoch", "unpaired"] as const)(
    "preserves master's fully loaded transcript on an excluded %s forced partial tail", kind => {
      const initial = { ...loaded(),
        ...(kind === "remote" ? { remoteId: "upstream" } : {}),
        bodySeqEpoch: kind === "foreign-epoch" ? "foreign-instance" : kind === "unpaired" ? undefined : "local-instance",
        bodySeq: kind === "unpaired" ? undefined : 0 };
      const owner = new TranscriptRepairAuthority(undefined, [initial]);
      owner.setServerInstance("local-instance");
      owner.adoptTail({ ...initial, sessionMutationStamp: 2,
        messages: [text("middle", "Middle"), text("last", "New excluded tail")],
        messageStartIndex: 1, messagesLoaded: false, hasOlderHistory: true }, { outcome: "partial",
          requestContext: { kind: "partialTail", revision: 1, serverInstanceId: "local-instance",
            sessionMutationStamp: 1, messageCount: 3 } });
      expect(owner.sessionsRef.current[0].messages).toBe(initial.messages);
      // Master retains the bodies but marks the changed-stamp window unloaded.
      expect(owner.sessionsRef.current[0].messagesLoaded).toBe(false);
      expect(owner.bodyCertificate("local")).toBeNull();
    });

  it.each([false, true])("invalidates replacement history proof when detached is %s", detached => {
    const owner = new TranscriptRepairAuthority(undefined, [loaded()]);
    owner.setServerInstance("local-instance");
    owner.adoptTail(loaded());
    const current = owner.sessionsRef.current[0];
    owner.commit([{ ...current, messages: [text("last", "Captured older body")],
      messageStartIndex: 2, messagesLoaded: false, hasOlderHistory: true,
      hasNewerHistory: detached }], "history");
    expect(owner.bodyCertificate("local")).toBeNull();
    expect(owner.needsTailRead("local", true)).toBe(!detached);
  });

  it("rejects older metadata at the publication gate after a newer paired read", () => {
    const owner = new TranscriptRepairAuthority(undefined, [loaded()]);
    owner.setServerInstance("local-instance");
    owner.adoptTail(loaded());
    const captured = { ...owner.sessionsRef.current[0], status: "active" as const,
      sessionMutationStamp: 11, preview: "Old preview", queuePaused: true };
    const newest = { ...captured, status: "idle" as const, sessionMutationStamp: 20,
      bodySeq: 1, preview: "Complete preview", queuePaused: false };
    owner.adoptTail(newest);
    owner.commit([captured], "metadata");
    expect(owner.sessionsRef.current[0]).toMatchObject({ status: "idle", sessionMutationStamp: 20,
      preview: "Complete preview", queuePaused: false });
  });
});

describe("transcript repair authority", () => {
  it("keeps the unpaired unhydrated reducer's known-stamp fallback on master behavior", () => {
    const initial = { ...session("a"), sessionMutationStamp: 20,
      messagesLoaded: false, messageCount: 1 };
    const authority = new TranscriptRepairAuthority(undefined, [initial]);
    authority.setServerInstance("local-instance");
    const result = applyDeltaToSessions(authority.sessionsRef.current, {
      type: "messageUpdated", revision: 2, sessionId: "a", messageId: "next",
      messageIndex: 1, messageCount: 2, sessionMutationStamp: 11, status: "active",
      preview: "Master-admitted delta", message: { id: "next", type: "text",
        author: "assistant", timestamp: "now", text: "New body" },
    });
    expect(result.kind).not.toBe("needsResync");
    if (result.kind !== "needsResync") authority.commit(result.sessions, "delta");
    expect(authority.sessionsRef.current[0]).toMatchObject({ status: "active",
      sessionMutationStamp: 11, preview: "Master-admitted delta", messageCount: 2 });
  });

  it.each([undefined, null])("admits an unknown incoming stamp %s instead of treating it as zero", stamp => {
    const initial = { ...session("a"), sessionMutationStamp: 1,
      bodySeq: 0, bodySeqEpoch: "local-instance" };
    const authority = new TranscriptRepairAuthority(undefined, [initial]);
    authority.setServerInstance("local-instance");
    authority.adoptTail(initial);
    authority.adoptSummaries([{ ...initial, messageCount: 0, queuePaused: false,
      status: "active", preview: "New unstamped snapshot", sessionMutationStamp: stamp }]);
    expect(authority.sessionsRef.current[0]).toMatchObject({ status: "active",
      preview: "New unstamped snapshot", sessionMutationStamp: stamp });
  });

  it.each(["never-paired", "remote", "foreign-epoch"] as const)("keeps master metadata admission for %s", kind => {
    const initial = { ...session("a"), sessionMutationStamp: 20,
      ...(kind === "remote" ? { remoteId: "upstream" } : {}),
      bodySeq: 0, bodySeqEpoch: kind === "foreign-epoch" ? "upstream-instance" : "local-instance" };
    const authority = new TranscriptRepairAuthority(undefined, [initial]);
    authority.setServerInstance("local-instance");
    if (kind !== "never-paired") authority.adoptCreatedSession(initial);
    authority.adoptSummaries([{ ...initial, messageCount: 0, queuePaused: false,
      status: "active", sessionMutationStamp: 11 }]);
    expect(authority.sessionsRef.current[0]).toMatchObject({ status: "active", sessionMutationStamp: 11 });
  });

  it.each(["legacy", "certified", "lost"] as const)("keeps the resident list reference for an unchanged %s publication", state => {
    const initial = [session("a"), session("b")];
    if (state !== "legacy") {
      initial[0] = { ...initial[0], bodySeq: 1, bodySeqEpoch: "local-instance" };
    }
    const authority = new TranscriptRepairAuthority(undefined, initial);
    authority.setServerInstance("local-instance");
    if (state !== "legacy") authority.adoptTail(initial[0]);
    if (state === "lost") authority.declareLoss("lagged");
    const resident = authority.sessionsRef.current;
    expect(authority.commit(resident, "metadata")).toBe(resident);
    const supplied = [...resident];
    expect(authority.commit(supplied, "delta")).toBe(supplied);
    const renamed = authority.commit([{ ...resident[0], name: "Renamed" }, resident[1]], "metadata");
    expect(renamed).not.toBe(resident);
    expect(renamed[0].name).toBe("Renamed");
    expect(renamed[1]).toBe(resident[1]);
  });
});
