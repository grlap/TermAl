import { describe, expect, it } from "vitest";
import {
  buildUnknownModelConfirmationKeySet,
  setContainsOnlyValuesFrom,
} from "./app-live-state-model-confirmations";
import type { Session } from "./types";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "Session 1",
    emoji: "T",
    agent: "Codex",
    workdir: "/tmp/termal-test",
    model: "gpt-5.5",
    modelOptions: [{ label: "GPT-5.4", value: "gpt-5.4" }],
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

describe("app live-state model confirmation helpers", () => {
  it("builds confirmation keys only for sessions with unknown live models", () => {
    const keys = buildUnknownModelConfirmationKeySet([
      session({ id: "unknown-codex", model: "gpt-5.5" }),
      session({
        id: "known-codex",
        model: "gpt-5.4",
      }),
      session({
        id: "no-live-list",
        modelOptions: undefined,
      }),
    ]);

    expect([...keys]).toEqual(["unknown-codex:gpt-5.5"]);
  });

  it("checks whether a set contains only allowed values", () => {
    expect(
      setContainsOnlyValuesFrom(
        new Set(["session-1:gpt-5.5"]),
        new Set(["session-1:gpt-5.5", "session-2:gpt-5.5"]),
      ),
    ).toBe(true);
    expect(
      setContainsOnlyValuesFrom(
        new Set(["session-1:gpt-5.5", "stale:gpt-4"]),
        new Set(["session-1:gpt-5.5"]),
      ),
    ).toBe(false);
  });
});
