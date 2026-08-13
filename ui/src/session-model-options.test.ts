import { describe, expect, it } from "vitest";

import {
  codexFastCapability,
  codexFastModeAfterModelChange,
  matchingSessionModelOption,
} from "./session-model-options";

describe("matchingSessionModelOption", () => {
  it("returns null when model options are unavailable", () => {
    expect(matchingSessionModelOption(undefined, "default")).toBeNull();
  });

  it("matches model options case-insensitively after trimming labels and values", () => {
    const option = matchingSessionModelOption(
      [
        {
          label: "  Default (recommended)  ",
          value: "  default  ",
        },
      ],
      "default",
    );

    expect(option).toEqual({
      label: "  Default (recommended)  ",
      value: "  default  ",
    });
  });

  it("matches against the option label when the value differs", () => {
    const option = matchingSessionModelOption(
      [
        {
          label: "  Auto  ",
          value: "claude-sonnet-4-5",
        },
      ],
      "auto",
    );

    expect(option).toEqual({
      label: "  Auto  ",
      value: "claude-sonnet-4-5",
    });
  });

  it("returns null for a blank requested model", () => {
    expect(matchingSessionModelOption([], "   ")).toBeNull();
  });
});

describe("codexFastCapability", () => {
  it("distinguishes unknown, supported, and authoritatively unsupported catalogs", () => {
    expect(
      codexFastCapability({ model: "gpt-5.6-sol", modelOptions: undefined }),
    ).toEqual({ status: "unknown", tier: null });
    expect(
      codexFastCapability({
        model: "gpt-5.6-sol",
        modelOptions: [{ value: "other-model", label: "Other model" }],
      }),
    ).toEqual({ status: "unknown", tier: null });

    const tier = { id: "priority", label: "Fast" };
    expect(
      codexFastCapability({
        model: "gpt-5.6-sol",
        modelOptions: [
          { value: "gpt-5.6-sol", label: "GPT-5.6 Sol", serviceTiers: [tier] },
        ],
      }),
    ).toEqual({ status: "supported", tier });
    expect(
      codexFastCapability({
        model: "gpt-5.3-codex-spark",
        modelOptions: [
          { value: "gpt-5.3-codex-spark", label: "Spark", serviceTiers: [] },
        ],
      }),
    ).toEqual({ status: "unsupported", tier: null });
  });

  it("uses exact catalog values rather than case-insensitive labels", () => {
    const tier = { id: "priority", label: "Fast" };
    const session = {
      model: "gpt-5.6-sol",
      modelOptions: [
        { value: "gpt-5.6-sol", label: "GPT-5.6 Sol", serviceTiers: [tier] },
      ],
    };

    expect(codexFastCapability(session, "gpt-5.6-sol")).toEqual({
      status: "supported",
      tier,
    });
    expect(codexFastCapability(session, "GPT-5.6 Sol")).toEqual({
      status: "unknown",
      tier: null,
    });
  });
});

describe("codexFastModeAfterModelChange", () => {
  const supportingTier = { id: "priority", label: "Fast" };

  it("preserves active Fast only while the catalog is empty or the target advertises it", () => {
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: undefined,
        },
        "gpt-5.6-sol",
      ),
    ).toBe(true);
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: [],
        },
        "gpt-5.6-sol",
      ),
    ).toBe(true);
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: [
            {
              value: "gpt-5.6-sol",
              label: "GPT-5.6 Sol",
              serviceTiers: [supportingTier],
            },
          ],
        },
        "gpt-5.6-sol",
      ),
    ).toBe(true);
  });

  it("normalizes a catalog label before applying exact Fast capability", () => {
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: [
            {
              value: "gpt-5.6-sol",
              label: "GPT-5.6 Sol",
              serviceTiers: [supportingTier],
            },
          ],
        },
        "GPT-5.6 Sol",
      ),
    ).toBe(true);
  });

  it("clears Fast for targets omitted by or unsupported in a loaded catalog", () => {
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: [{ value: "gpt-5.5", label: "GPT-5.5" }],
        },
        "gpt-5.6-sol",
      ),
    ).toBe(false);
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: [{ value: "gpt-5.6-sol", label: "GPT-5.6 Sol" }],
        },
        "gpt-5.6-sol",
      ),
    ).toBe(false);
  });

  it("never enables Fast when the current session authority is Standard", () => {
    expect(
      codexFastModeAfterModelChange(
        {
          codexFastMode: false,
          model: "gpt-5.5",
          modelOptions: [
            {
              value: "gpt-5.6-sol",
              label: "GPT-5.6 Sol",
              serviceTiers: [supportingTier],
            },
          ],
        },
        "gpt-5.6-sol",
      ),
    ).toBe(false);
  });
});
