import { describe, expect, it } from "vitest";

import {
  CLAUDE_EFFORT_OPTIONS,
  FALLBACK_CLAUDE_EFFORTS,
  areTelegramUiConfigsEqual,
  isDefaultModelPreference,
  normalizeTelegramUiConfig,
  supportedClaudeEffortLevelsForModelOption,
} from "./session-model-utils";

describe("isDefaultModelPreference", () => {
  it.each([
    ["empty string", "", true],
    ["whitespace", "   ", true],
    ["lowercase sentinel", "default", true],
    ["title-case sentinel", "Default", true],
    ["uppercase sentinel", "DEFAULT", true],
    ["padded sentinel", " default ", true],
    ["prefix match", "defaults", false],
    ["custom model", "my-model", false],
  ])("classifies %s", (_label, value, expected) => {
    expect(isDefaultModelPreference(value)).toBe(expected);
  });
});

describe("Claude effort options", () => {
  it("includes xhigh in default-capability effort choices", () => {
    expect(CLAUDE_EFFORT_OPTIONS.map((option) => option.value)).toEqual([
      "default",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
    expect(FALLBACK_CLAUDE_EFFORTS).toEqual(["low", "medium", "high", "xhigh"]);
    expect(supportedClaudeEffortLevelsForModelOption(null, "default")).toEqual([
      "low",
      "medium",
      "high",
      "xhigh",
    ]);
    expect(
      supportedClaudeEffortLevelsForModelOption({ label: "No effort", value: "no-effort" }, "xhigh"),
    ).toEqual(["xhigh"]);
  });
});

describe("normalizeTelegramUiConfig", () => {
  it("fills omitted Telegram app-state fields with UI defaults", () => {
    expect(
      normalizeTelegramUiConfig({
        enabled: true,
        subscribedProjectIds: ["project-1"],
      }),
    ).toEqual({
      enabled: true,
      forwardAssistantReplies: false,
      subscribedProjectIds: ["project-1"],
      defaultProjectId: null,
      defaultSessionId: null,
    });
  });
});

describe("areTelegramUiConfigsEqual", () => {
  it("compares normalized Telegram config values", () => {
    expect(
      areTelegramUiConfigsEqual(
        {
          enabled: false,
          forwardAssistantReplies: false,
          subscribedProjectIds: [],
          defaultProjectId: null,
          defaultSessionId: null,
        },
        null,
      ),
    ).toBe(true);
  });

  it("treats subscribed project ordering as part of the config", () => {
    expect(
      areTelegramUiConfigsEqual(
        {
          subscribedProjectIds: ["project-1", "project-2"],
        },
        {
          subscribedProjectIds: ["project-2", "project-1"],
        },
      ),
    ).toBe(false);
  });
});
