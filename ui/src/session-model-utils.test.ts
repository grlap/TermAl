import { describe, expect, it } from "vitest";

import {
  areTelegramUiConfigsEqual,
  isDefaultModelPreference,
  normalizeTelegramUiConfig,
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
