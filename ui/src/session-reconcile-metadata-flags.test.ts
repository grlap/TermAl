import { describe, expect, it } from "vitest";

import {
  reconcileSingleSession,
  reconcileSingleStateSessionSummary,
} from "./session-reconcile";
import type { Session } from "./types";

const flags = [
  "queuePaused",
  "providerCapacityWaiting",
  "sourceTrackingRecovery",
] as const;
type Projection = "full" | "partial tail" | "summary";

function session(messagesLoaded: boolean, stamp: number | undefined): Session {
  return {
    id: "session-a", name: "A", emoji: "", agent: "Codex", workdir: "/repo",
    model: "model", status: "idle", preview: "", messages: [], messageCount: 0,
    messagesLoaded, promptHistory: ["retained history"], sessionMutationStamp: stamp,
    queuePaused: false, providerCapacityWaiting: false, sourceTrackingRecovery: false,
  };
}

function adopt(previous: Session, next: Session, projection: Projection): Session {
  if (projection === "summary") {
    const {
      messages: _messages, messagesLoaded: _loaded, promptHistory: _history,
      promptHistoryRedacted: _redacted, pendingPrompts: _pending, ...metadata
    } = next;
    return reconcileSingleStateSessionSummary(previous, {
      ...metadata, messageCount: next.messageCount ?? 0, queuePaused: Boolean(next.queuePaused),
    });
  }
  return reconcileSingleSession(previous, next, {
    adoptPartialMessages: projection === "partial tail",
  });
}

describe("authoritative session metadata flags", () => {
  for (const flag of flags) {
    for (const stamp of [undefined, 42]) {
      for (const projection of ["full", "partial tail", "summary"] as const) {
        const context = `${flag}, ${projection}, ${stamp === undefined ? "absent" : "equal"} stamp`;

        for (const value of [false, true]) {
          it(`adopts ${!value} -> ${value}: ${context}`, () => {
            const previous = { ...session(projection !== "partial tail", stamp), [flag]: !value };
            const next = { ...previous, [flag]: value };
            const merged = adopt(previous, next, projection);

            expect(merged).not.toBe(previous);
            expect(merged[flag]).toBe(value);
            expect(merged.messages).toBe(previous.messages);
            expect(merged.promptHistory).toBe(previous.promptHistory);
          });

          it(`retains sharing for unchanged ${value}: ${context}`, () => {
            const previous = { ...session(projection !== "partial tail", stamp), [flag]: value };
            expect(adopt(previous, { ...previous }, projection)).toBe(previous);
          });
        }

        it(`treats omitted and false flags alike: ${context}`, () => {
          const previous = session(projection !== "partial tail", stamp);
          const next = { ...previous, [flag]: undefined };
          expect(adopt(previous, next, projection)).toBe(previous);
        });
      }
    }
  }
});
