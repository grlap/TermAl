import { describe, expect, it } from "vitest";

import {
  CLAUDE_EFFORT_SLASH_OPTIONS,
  FALLBACK_CLAUDE_EFFORTS,
  sessionModeSlashState,
  sessionModelSlashState,
  slashCommandsForSession,
  type SlashPaletteSession,
} from "./session-slash-palette";

describe("Claude effort slash choices", () => {
  it("exposes xhigh in the slash palette fallback choices", () => {
    expect(CLAUDE_EFFORT_SLASH_OPTIONS.map((option) => option.value)).toEqual([
      "default",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
    expect(FALLBACK_CLAUDE_EFFORTS).toEqual(["low", "medium", "high", "xhigh"]);
  });
});

describe("OpenCode slash choices", () => {
  const session = {
    id: "session-opencode",
    agent: "OpenCode",
    agentCommandsRevision: 0,
    model: "openai/gpt-5.6-sol",
    opencodeModel: "auto",
    opencodeMode: "plan",
    opencodeCurrentMode: "plan",
    modelOptions: [
      { value: "openai/gpt-5.6-sol", label: "GPT-5.6 Sol" },
    ],
    opencodeModeOptions: [
      { value: "build", label: "Build" },
      { value: "plan", label: "Plan" },
    ],
    workdir: "/tmp",
  } as SlashPaletteSession;

  it("exposes model and mode commands", () => {
    expect(
      slashCommandsForSession(session).map((command) => command.id),
    ).toEqual(expect.arrayContaining(["model", "mode"]));
    expect(sessionModeSlashState(session, "")?.items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "plan", isCurrent: true }),
      ]),
    );
  });

  it("marks selected authority and never offers arbitrary manual models", () => {
    const state = sessionModelSlashState(
      session,
      "unlisted",
      "unlisted/provider",
      false,
      null,
    );
    expect(state.items).toEqual([]);

    const allModels = sessionModelSlashState(session, "", "", false, null);
    expect(allModels.items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "auto", isCurrent: true }),
        expect.objectContaining({
          value: "openai/gpt-5.6-sol",
          isCurrent: false,
        }),
      ]),
    );
  });
});
