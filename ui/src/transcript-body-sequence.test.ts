import { describe, expect, it } from "vitest";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import { MAX_KEPT_BODY_DELTAS, bodySequenceProof } from "./transcript-body-sequence";
import type { Session, TextDeltaEvent } from "./types";

function session(text = "A", seq = 0, epoch = "local"): Session {
  return { id: "a", name: "A", emoji: "AI", agent: "Codex", model: "default",
    workdir: "C:/repo", status: "active", preview: text, bodySeq: seq, bodySeqEpoch: epoch,
    messageCount: 1, messageStartIndex: 0, messagesLoaded: true, hasNewerHistory: false,
    messages: [{ id: "m", type: "text", author: "assistant", timestamp: "now", text }] };
}
function delta(seq: number, start: number, suffix: string): TextDeltaEvent {
  return { type: "textDelta", revision: seq + 100, bodySeqEpoch: "local", sessionSeq: seq,
    sessionId: "a", messageId: "m", messageIndex: 0, messageCount: 1,
    textStartByte: start, delta: suffix, sessionMutationStamp: seq + 10 };
}
function owner() {
  const result = new TranscriptRepairAuthority(undefined, [session()]);
  result.setServerInstance("local");
  result.adoptTail(session());
  return result;
}

describe("local attached-tail sequence", () => {
  it("raw DTO fields are observation, not a certificate", () => {
    const result = new TranscriptRepairAuthority(undefined, [session()]);
    result.setServerInstance("local");
    expect(result.bodyCertificate("a")).toBeNull();
    result.adoptSummaries([{ ...session(), messageCount: 1, queuePaused: false }]);
    expect(result.needsTailRead("a", true)).toBe(true);
    expect(result.bodyCertificate("a")).toBeNull();
  });
  it("places only the next frame, ignores old frames and marks a hole dirty without eviction", () => {
    const result = owner();
    expect(result.receiveBodyDelta(delta(1, 1, "B"))).toBe("applied");
    const resident = result.sessionsRef.current[0];
    expect(result.receiveBodyDelta(delta(1, 1, "B"))).toBe("late");
    expect(result.sessionsRef.current[0]).toBe(resident);
    expect(result.receiveBodyDelta(delta(3, 3, "D"))).toBe("repair");
    expect(result.sessionsRef.current[0].messages).toBe(resident.messages);
    expect(result.needsTailRead("a", true)).toBe(true);
    expect(result.bodyCertificate("a")).toBeNull();
  });
  it("replays the contiguous suffix above an in-flight tail before certification", () => {
    const result = owner();
    const answer = session("AB", 1);
    result.receiveBodyDelta(delta(1, 1, "B"));
    result.receiveBodyDelta(delta(2, 2, "C"));
    expect(result.adoptTail(answer)).not.toBeNull();
    expect(result.sessionsRef.current[0].messages).toEqual(session("ABC").messages);
    expect(result.bodyCertificate("a")?.appliedSeq).toBe(2);
  });
  it("refuses missing and unplaceable kept replay without removing resident bodies", () => {
    const result = owner(), messages = result.sessionsRef.current[0].messages;
    result.receiveBodyDelta(delta(2, 2, "C"));
    expect(result.adoptTail(session("A", 0))).toBeNull();
    expect(result.sessionsRef.current[0].messages).toBe(messages);
    expect(result.receiveBodyDelta(delta(1, 99, "B"))).toBe("repair");
    expect(result.adoptTail(session("A", 0))).toBeNull();
    expect(result.adoptTail(session("ABC", 2))).not.toBeNull();
  });
  it("bounded kept evidence refuses an old read and accepts a fresh covering tail", () => {
    const result = owner();
    for (let seq = 1; seq <= MAX_KEPT_BODY_DELTAS + 1; seq++) {
      result.receiveBodyDelta(delta(seq, 99, "X"));
    }
    expect(result.adoptTail(session("A", 0))).toBeNull();
    expect(result.adoptTail(session("Current", MAX_KEPT_BODY_DELTAS + 1))).not.toBeNull();
    expect(result.needsTailRead("a", true)).toBe(false);
  });
  it.each(["remote", "foreign", "unpaired"] as const)("does not certify %s sessions", kind => {
    const read = { ...session(), remoteId: kind === "remote" ? "upstream" : undefined,
      bodySeqEpoch: kind === "foreign" ? "upstream" : kind === "unpaired" ? undefined : "local" };
    const result = new TranscriptRepairAuthority(undefined, [read]);
    result.setServerInstance("local");
    result.adoptCreatedSession(read);
    expect(result.bodyCertificate("a")).toBeNull();
    expect(result.needsTailRead("a", true)).toBe(false);
    expect(result.receiveBodyDelta(delta(1, 1, "B"))).toBe("legacy");
  });
  it("drops certification on detachment and resets the local domain on restart", () => {
    const result = owner();
    result.commit([{ ...result.sessionsRef.current[0], hasNewerHistory: true }], "history");
    expect(result.bodyCertificate("a")).toBeNull();
    expect(result.needsTailRead("a", true)).toBe(false);
    result.setServerInstance("replacement");
    result.adoptCreatedSession(session("New", 0, "replacement"));
    expect(result.bodyCertificate("a")).toEqual({ epoch: "replacement", appliedSeq: 0 });
    expect(result.receiveBodyDelta(delta(1, 1, "Old"))).toBe("legacy");
  });
  it.each([undefined, -1, 0.5, NaN, Infinity])("requires pair-or-neither and a valid sequence (%s)", seq => {
    expect(bodySequenceProof({ bodySeq: seq, bodySeqEpoch: "local" })).toBeNull();
  });
});
