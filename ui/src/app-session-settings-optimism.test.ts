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
    opencodeEffort: "auto",
    opencodeCurrentEffort: "medium",
    opencodeMode: "auto",
    opencodeCurrentMode: "build",
    status: "idle",
    preview: "Ready",
    messages: [],
    ...overrides,
  };
}

describe("OpenCode optimistic session settings", () => {
  it("projects explicit model, reasoning-variant, and dynamic-mode selections immediately", () => {
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
    expect(
      buildOptimisticSessionSettingsUpdate(session, "opencodeEffort", "high"),
    ).toMatchObject({
      opencodeEffort: "high",
      opencodeCurrentEffort: "high",
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

    const optimisticEffort = buildOptimisticSessionSettingsUpdate(
      previous,
      "opencodeEffort",
      "high",
    );
    expect(
      rollbackOptimisticSessionSettingsUpdate(
        optimisticEffort,
        previous,
        optimisticEffort,
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

describe("Codex Fast optimistic session settings", () => {
  const session: Session = {
    id: "session-codex",
    name: "Codex",
    emoji: "C",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.5",
    modelOptions: [
      {
        label: "GPT-5.5",
        value: "gpt-5.5",
        serviceTiers: [{ id: "priority", label: "Fast" }],
      },
      { label: "GPT-5.4 mini", value: "gpt-5.4-mini" },
    ],
    codexFastMode: false,
    status: "idle",
    preview: "Ready",
    messages: [],
  };

  it("projects and rolls back Fast mode", () => {
    const optimistic = buildOptimisticSessionSettingsUpdate(
      session,
      "codexFastMode",
      "on",
    );
    expect(optimistic.codexFastMode).toBe(true);
    expect(
      rollbackOptimisticSessionSettingsUpdate(optimistic, session, optimistic),
    ).toEqual(session);
  });

  it("clears Fast mode when switching to a model without the tier", () => {
    expect(
      buildOptimisticSessionSettingsUpdate(
        { ...session, codexFastMode: true },
        "model",
        "gpt-5.4-mini",
      ),
    ).toMatchObject({ model: "gpt-5.4-mini", codexFastMode: false });
  });

  it("clears Fast when a loaded catalog omits the target model", () => {
    expect(
      buildOptimisticSessionSettingsUpdate(
        { ...session, codexFastMode: true },
        "model",
        "gpt-5.6-sol",
      ),
    ).toMatchObject({ model: "gpt-5.6-sol", codexFastMode: false });
  });

  it("preserves Fast while the model catalog is unavailable", () => {
    expect(
      buildOptimisticSessionSettingsUpdate(
        { ...session, codexFastMode: true, modelOptions: undefined },
        "model",
        "gpt-5.6-sol",
      ),
    ).toMatchObject({ model: "gpt-5.6-sol", codexFastMode: true });
  });
});
