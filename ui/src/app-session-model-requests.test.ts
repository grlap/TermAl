import { describe, expect, it } from "vitest";
import {
  configuredDefaultModelForAgent,
  requestedModelForNewSession,
  type AppSessionDefaultModels,
} from "./app-session-model-requests";

const defaultModels: AppSessionDefaultModels = {
  Claude: "claude-sonnet-4.5",
  Codex: "gpt-5.5",
  Cursor: "cursor-fast",
  Gemini: "gemini-2.5-pro",
  OpenCode: "openai/gpt-5.6-sol",
};

describe("app session model request helpers", () => {
  it("resolves configured default models by agent", () => {
    expect(configuredDefaultModelForAgent("Claude", defaultModels)).toBe(
      "claude-sonnet-4.5",
    );
    expect(configuredDefaultModelForAgent("Codex", defaultModels)).toBe(
      "gpt-5.5",
    );
    expect(configuredDefaultModelForAgent("Cursor", defaultModels)).toBe(
      "cursor-fast",
    );
    expect(configuredDefaultModelForAgent("Gemini", defaultModels)).toBe(
      "gemini-2.5-pro",
    );
    expect(configuredDefaultModelForAgent("OpenCode", defaultModels)).toBe(
      "openai/gpt-5.6-sol",
    );
  });

  it("uses dialog model text for agents without the session model picker", () => {
    const legacyAgent = "Legacy" as never;

    expect(
      requestedModelForNewSession(legacyAgent, "  dialog-model  ", defaultModels),
    ).toBe("dialog-model");
    expect(
      requestedModelForNewSession(legacyAgent, "   ", defaultModels),
    ).toBeUndefined();
  });

  it("uses configured app defaults for Cursor, Gemini, and OpenCode sessions", () => {
    expect(
      requestedModelForNewSession("Cursor", "  cursor-dialog  ", defaultModels),
    ).toBe("cursor-fast");
    expect(
      requestedModelForNewSession("Gemini", "  gemini-dialog  ", defaultModels),
    ).toBe("gemini-2.5-pro");
    expect(
      requestedModelForNewSession("OpenCode", "  opencode-dialog  ", defaultModels),
    ).toBe("openai/gpt-5.6-sol");
  });

  it("omits default sentinel model values for picker-backed agents", () => {
    expect(
      requestedModelForNewSession("Codex", "ignored", {
        ...defaultModels,
        Codex: " DEFAULT ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("Claude", "ignored", {
        ...defaultModels,
        Claude: " default ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("Cursor", "ignored", {
        ...defaultModels,
        Cursor: " default ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("Gemini", "ignored", {
        ...defaultModels,
        Gemini: " default ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("OpenCode", "ignored", {
        ...defaultModels,
        OpenCode: " default ",
      }),
    ).toBeUndefined();
  });

  it("sends configured default models for picker-backed agents", () => {
    expect(requestedModelForNewSession("Codex", "ignored", defaultModels)).toBe(
      "gpt-5.5",
    );
    expect(requestedModelForNewSession("Claude", "ignored", defaultModels)).toBe(
      "claude-sonnet-4.5",
    );
    expect(requestedModelForNewSession("Cursor", "ignored", defaultModels)).toBe(
      "cursor-fast",
    );
    expect(requestedModelForNewSession("Gemini", "ignored", defaultModels)).toBe(
      "gemini-2.5-pro",
    );
    expect(
      requestedModelForNewSession("OpenCode", "ignored", defaultModels),
    ).toBe("openai/gpt-5.6-sol");
  });

  it("uses Auto when legacy app state omitted the OpenCode default", () => {
    const { OpenCode: _omitted, ...legacyDefaults } = defaultModels;

    expect(configuredDefaultModelForAgent("OpenCode", legacyDefaults)).toBe("default");
    expect(
      requestedModelForNewSession("OpenCode", "ignored", legacyDefaults),
    ).toBeUndefined();
  });
});
