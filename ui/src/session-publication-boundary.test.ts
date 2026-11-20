// All body-bearing projections share the owner gate and current-owner render.
import { afterEach, expect, it } from "vitest";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import { getSessionRecordSnapshotForTesting, resetSessionStoreForTesting } from "./session-store";
import type { UseAppLiveStateParams } from "./app-live-state-types";
import type { UseAppSessionActionsParams } from "./app-session-actions-types";
import type { Session } from "./types";

function record(text: string, stamp = 1): Session {
  return { id: "a", name: "A", emoji: "AI", agent: "Codex", workdir: "C:/repo",
    model: "default", status: "idle", preview: text, sessionMutationStamp: stamp,
    messageCount: 1, bodySeqEpoch: "local", bodySeq: 0,
    messagesLoaded: true, messageStartIndex: 0, hasOlderHistory: false, hasNewerHistory: false,
    messages: [{ id: "body", type: "text", author: "assistant", timestamp: "now", text }] };
}
afterEach(resetSessionStoreForTesting);

it.each(["metadata", "delta", "history"] as const)("central older-stamp guard covers %s across all sinks", provenance => {
  let list: Session[] = [];
  const original = record("Original", 20);
  const owner = new TranscriptRepairAuthority({ publish: next => { list = next(); },
    drafts: () => ({}), attachments: () => ({}) }, [original]);
  owner.setServerInstance("local"); owner.adoptTail(original);
  owner.commit([{ ...record("New body", 11), status: "active", queuePaused: true }], provenance);
  owner.publish();
  for (const session of [owner.sessionsRef.current[0], getSessionRecordSnapshotForTesting("a")!, list[0]]) {
    expect(session.sessionMutationStamp).toBe(20);
    expect(session.status).toBe("idle");
    expect(session.preview).toBe("Original");
    expect(session.messages).toEqual(record(provenance === "metadata" ? "Original" : "New body").messages);
  }
});
it("queued React updaters use the current owner, and dirty never evicts", () => {
  const pending: Array<() => Session[]> = [];
  const owner = new TranscriptRepairAuthority({ publish: next => pending.push(next),
    drafts: () => ({}), attachments: () => ({}) }, [record("Old")]);
  owner.setServerInstance("local"); owner.adoptTail(record("Old")); owner.publish();
  owner.declareLoss("lagged");
  expect(pending[0]()[0].messages).toEqual(record("Old").messages);
  owner.adoptTail({ ...record("New", 2), bodySeq: 1 });
  expect(pending[0]()[0].messages).toEqual(record("New").messages);
});
it("accepted instance replacement starts a new metadata stamp domain", () => {
  const owner = new TranscriptRepairAuthority(undefined, [record("Old", 100)]);
  owner.setServerInstance("local"); owner.adoptTail(record("Old", 100));
  owner.setServerInstance("replacement");
  owner.adoptCreatedSession({ ...record("Restarted", 1), bodySeqEpoch: "replacement" });
  expect(owner.sessionsRef.current[0].sessionMutationStamp).toBe(1);
});

// These intentional compile errors pin the publication capability boundary.
function compilerBoundary(owner: TranscriptRepairAuthority, live: UseAppLiveStateParams, actions: UseAppSessionActionsParams) {
  // @ts-expect-error Only the owner replaces its resident list.
  owner.sessionsRef.current = [];
  // @ts-expect-error The exported list is read-only.
  owner.sessionsRef.current.push(record("Bad"));
  // @ts-expect-error No live-state React list setter capability.
  live.stateSetters.setSessions([]);
  // @ts-expect-error No action React list setter capability.
  actions.setters.setSessions([]);
}
void compilerBoundary;
