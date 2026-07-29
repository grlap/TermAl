import { describe, expect, it } from "vitest";

import {
  buildOptimisticSessionSettingsUpdate,
  rollbackOptimisticSessionSettingsUpdate,
} from "./app-session-settings-optimism";
import type { Session } from "./types";

function makeOpenCodeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-opencode",
    name: "OpenCode",
    emoji: "OC",
    agent: "OpenCode",
    workdir: "/tmp",
    model: "opencode/big-pickle",
    opencodeModel: "auto",
    opencodeMode: "auto",
    opencodeCurrentMode: "build",
    status: "idle",
    preview: "Ready",
    messages: [],
    ...overrides,
  };
}

describe("OpenCode optimistic session settings", () => {
  it("projects explicit model and dynamic-mode selections immediately", () => {
    const session = makeOpenCodeSession();

    expect(
      buildOptimisticSessionSettingsUpdate(
        session,
        "model",
        "openai/gpt-5.6-sol",
      ),
    ).toMatchObject({
      model: "openai/gpt-5.6-sol",
      opencodeModel: "openai/gpt-5.6-sol",
      opencodeMode: "auto",
      opencodeCurrentMode: "build",
    });
    expect(
      buildOptimisticSessionSettingsUpdate(session, "opencodeMode", "plan"),
    ).toMatchObject({
      model: "opencode/big-pickle",
      opencodeModel: "auto",
      opencodeMode: "plan",
      opencodeCurrentMode: "plan",
    });
  });

  it("rolls matching optimistic OpenCode fields back after a failed request", () => {
    const previous = makeOpenCodeSession();
    const optimistic = buildOptimisticSessionSettingsUpdate(
      previous,
      "model",
      "openai/gpt-5.6-sol",
    );
    const current = {
      ...optimistic,
      preview: "Unrelated live-state update",
    };

    expect(
      rollbackOptimisticSessionSettingsUpdate(
        current,
        previous,
        optimistic,
      ),
    ).toEqual({
      ...previous,
      preview: "Unrelated live-state update",
    });

    const optimisticMode = buildOptimisticSessionSettingsUpdate(
      previous,
      "opencodeMode",
      "plan",
    );
    expect(
      rollbackOptimisticSessionSettingsUpdate(
        optimisticMode,
        previous,
        optimisticMode,
      ),
    ).toEqual(previous);
  });

  it("does not overwrite newer server values while rolling back", () => {
    const previous = makeOpenCodeSession();
    const optimistic = buildOptimisticSessionSettingsUpdate(
      previous,
      "model",
      "openai/gpt-5.6-sol",
    );
    const current = {
      ...optimistic,
      model: "anthropic/claude-opus-4-6",
      opencodeModel: "anthropic/claude-opus-4-6",
    };

    expect(
      rollbackOptimisticSessionSettingsUpdate(
        current,
        previous,
        optimistic,
      ),
    ).toBe(current);
  });
});
