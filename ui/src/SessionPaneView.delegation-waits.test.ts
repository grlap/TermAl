import { describe, expect, it } from "vitest";

import { delegationWaitIndicatorPrompt } from "./SessionPaneView";
import type { DelegationWaitRecord } from "./api";

function makeWait(
  id: string,
  overrides: Partial<DelegationWaitRecord> = {},
): DelegationWaitRecord {
  return {
    id,
    parentSessionId: "parent-session",
    delegationIds: [`delegation-${id}`],
    mode: "all",
    createdAt: "2026-05-09T00:00:00Z",
    ...overrides,
  };
}

describe("delegationWaitIndicatorPrompt", () => {
  it("includes the first title when multiple waits are pending", () => {
    expect(
      delegationWaitIndicatorPrompt([
        makeWait("one", {
          delegationIds: ["delegation-1", "delegation-2"],
          title: "review fan-in",
        }),
        makeWait("two", {
          mode: "any",
          title: "backend release gate",
        }),
      ]),
    ).toBe(
      "Waiting on 2 delegation waits covering 3 delegated sessions: review fan-in (+1 more)",
    );
  });

  it("keeps the generic multi-wait label when no wait has a title", () => {
    expect(
      delegationWaitIndicatorPrompt([
        makeWait("one", { title: "  " }),
        makeWait("two", { title: null }),
      ]),
    ).toBe("Waiting on 2 delegation waits covering 2 delegated sessions");
  });
});
