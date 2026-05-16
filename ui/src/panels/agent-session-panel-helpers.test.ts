// Owns: focused coverage for pure AgentSessionPanel helper extraction.
// Does not own: panel rendering, composer events, or backend request behavior.
// Split from: ui/src/panels/AgentSessionPanel.test.tsx.

import { describe, expect, it } from "vitest";

import {
  findNewPendingCreatedConversationMarker,
  isSpaceKey,
  spawnDelegationOptionsFromResolvedCommand,
  type PendingCreatedConversationMarker,
} from "./agent-session-panel-helpers";
import type { ConversationMarker } from "../types";

const marker = (id: string, name: string): ConversationMarker => ({
  color: "blue",
  createdAt: "10:00",
  createdBy: "user",
  id,
  kind: "custom",
  messageId: "message-1",
  messageIndexHint: 0,
  name,
  sessionId: "session-1",
  updatedAt: "10:00",
});

describe("agent session panel helpers", () => {
  it("finds the first marker that is new for a pending creation", () => {
    const pending: PendingCreatedConversationMarker = {
      existingMarkerIds: new Set(["existing"]),
      localId: 1,
      messageId: "message-1",
      name: "Target",
    };

    expect(
      findNewPendingCreatedConversationMarker(
        [
          marker("existing", "Target"),
          marker("used", "Target"),
          marker("wrong-name", "Other"),
          marker("created", "Target"),
        ],
        pending,
        new Set(["used"]),
      )?.id,
    ).toBe("created");
  });

  it("returns undefined when a resolved command carries no delegation options", () => {
    expect(
      spawnDelegationOptionsFromResolvedCommand({
        kind: "promptTemplate",
        name: "test",
        source: "local",
        visiblePrompt: "run tests",
      }),
    ).toBeUndefined();
  });

  it("builds delegation options from resolved command metadata", () => {
    expect(
      spawnDelegationOptionsFromResolvedCommand({
        delegation: {
          mode: "reviewer",
          title: "Review",
          writePolicy: { kind: "readOnly" },
        },
        kind: "promptTemplate",
        name: "review",
        source: "local",
        title: "Fallback",
        visiblePrompt: "review",
      }),
    ).toEqual({
      mode: "reviewer",
      title: "Review",
      writePolicy: { kind: "readOnly" },
    });
  });

  it("recognizes legacy and modern space-key event shapes", () => {
    expect(isSpaceKey({ key: " " })).toBe(true);
    expect(isSpaceKey({ key: "Spacebar" })).toBe(true);
    expect(isSpaceKey({ code: "Space", key: "Unidentified" })).toBe(true);
    expect(isSpaceKey({ key: "Unidentified", keyCode: 32 })).toBe(true);
    expect(isSpaceKey({ key: "Enter" })).toBe(false);
  });
});
