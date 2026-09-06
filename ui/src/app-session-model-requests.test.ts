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

  it("uses configured app defaults for Cursor, Gemini, and OpenCode sessions", () => {
    expect(
      requestedModelForNewSession("Cursor", defaultModels),
    ).toBe("cursor-fast");
    expect(
      requestedModelForNewSession("Gemini", defaultModels),
    ).toBe("gemini-2.5-pro");
    expect(
      requestedModelForNewSession("OpenCode", defaultModels),
    ).toBe("openai/gpt-5.6-sol");
  });

  it("omits default sentinel model values for picker-backed agents", () => {
    expect(
      requestedModelForNewSession("Codex", {
        ...defaultModels,
        Codex: " DEFAULT ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("Claude", {
        ...defaultModels,
        Claude: " default ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("Cursor", {
        ...defaultModels,
        Cursor: " default ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("Gemini", {
        ...defaultModels,
        Gemini: " default ",
      }),
    ).toBeUndefined();
    expect(
      requestedModelForNewSession("OpenCode", {
        ...defaultModels,
        OpenCode: " default ",
      }),
    ).toBeUndefined();
  });

  it("sends configured default models for picker-backed agents", () => {
    expect(requestedModelForNewSession("Codex", defaultModels)).toBe(
      "gpt-5.5",
    );
    expect(requestedModelForNewSession("Claude", defaultModels)).toBe(
      "claude-sonnet-4.5",
    );
    expect(requestedModelForNewSession("Cursor", defaultModels)).toBe(
      "cursor-fast",
    );
    expect(requestedModelForNewSession("Gemini", defaultModels)).toBe(
      "gemini-2.5-pro",
    );
    expect(
      requestedModelForNewSession("OpenCode", defaultModels),
    ).toBe("openai/gpt-5.6-sol");
  });

});
