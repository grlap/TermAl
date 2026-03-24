import { describe, expect, it } from "vitest";

import { matchingSessionModelOption } from "./session-model-options";

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
