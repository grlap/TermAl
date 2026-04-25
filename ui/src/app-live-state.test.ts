import { describe, expect, it } from "vitest";

import { resolveAdoptStateSessionOptions } from "./app-live-state";

describe("resolveAdoptStateSessionOptions", () => {
  it("preserves an explicit mutation-stamp fast-path disable without a server instance change", () => {
    expect(
      resolveAdoptStateSessionOptions(
        { disableMutationStampFastPath: true },
        false,
      ).disableMutationStampFastPath,
    ).toBe(true);
  });

  it("disables the mutation-stamp fast path when the server instance changes", () => {
    expect(
      resolveAdoptStateSessionOptions(
        { disableMutationStampFastPath: false },
        true,
      ).disableMutationStampFastPath,
    ).toBe(true);
  });

  it("keeps the mutation-stamp fast path enabled by default", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, false)
        .disableMutationStampFastPath,
    ).toBe(false);
  });
});
