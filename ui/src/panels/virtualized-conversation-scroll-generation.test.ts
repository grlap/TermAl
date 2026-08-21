// Owns focused tests for generation-based arbitration between user navigation
// and delayed mounted-range restores.
// Does not own React event wiring, page measurement, or rendered scroll bands.

import { describe, expect, it } from "vitest";

import {
  mountedPrependRestoreIsCurrent,
  type MountedPrependRestore,
} from "./virtualized-conversation-mounted-range";
import { nativeScrollAdvancesUserScrollGeneration } from "./virtualized-conversation-scroll-events";

function restoreAtGeneration(
  userScrollGeneration: number,
): MountedPrependRestore {
  return {
    anchor: null,
    scrollHeight: 2_000,
    scrollTop: 800,
    writeIntent: "mounted-range",
    userScrollGeneration,
  };
}

describe("mounted prepend restore generation", () => {
  it("keeps a prepend or idle-compaction restore while user position ownership is unchanged", () => {
    expect(mountedPrependRestoreIsCurrent(restoreAtGeneration(4), 4)).toBe(
      true,
    );
  });

  it("rejects an idle-compaction restore when a later wheel takes ownership", () => {
    expect(mountedPrependRestoreIsCurrent(restoreAtGeneration(4), 5)).toBe(
      false,
    );
  });

  it("rejects a prepend restore captured before a later PageUp navigation", () => {
    expect(mountedPrependRestoreIsCurrent(restoreAtGeneration(8), 9)).toBe(
      false,
    );
  });

  it("does not treat a height-changing prepend reflow as user navigation", () => {
    expect(
      nativeScrollAdvancesUserScrollGeneration({
        currentScrollHeight: 12_000,
        previousScrollHeight: 2_000,
        scrollDelta: 600,
      }),
    ).toBe(false);
  });

  it("recognizes native thumb or inertia movement when layout height is stable", () => {
    expect(
      nativeScrollAdvancesUserScrollGeneration({
        currentScrollHeight: 12_000,
        previousScrollHeight: 12_000,
        scrollDelta: -600,
      }),
    ).toBe(true);
  });
});
