import { describe, expect, it } from "vitest";
import {
  applyDelegationWaitConsumed,
  applyDelegationWaitCreated,
  areDelegationWaitRecordsEqual,
} from "./app-live-state-delegation-waits";
import type { DelegationWaitRecord } from "./api";

function wait(overrides: Partial<DelegationWaitRecord> = {}): DelegationWaitRecord {
  return {
    id: "wait-1",
    parentSessionId: "session-1",
    delegationIds: ["delegation-1", "delegation-2"],
    mode: "all",
    createdAt: "2026-05-16T10:00:00Z",
    title: "Review",
    ...overrides,
  };
}

describe("app live-state delegation-wait helpers", () => {
  it("compares wait records by order and normalizes absent titles", () => {
    expect(
      areDelegationWaitRecordsEqual(
        [wait({ title: null })],
        [wait({ title: undefined })],
      ),
    ).toBe(true);
    expect(
      areDelegationWaitRecordsEqual(
        [wait({ delegationIds: ["delegation-1", "delegation-2"] })],
        [wait({ delegationIds: ["delegation-2", "delegation-1"] })],
      ),
    ).toBe(false);
  });

  it("appends new waits and replaces existing waits by id", () => {
    const first = wait();
    const second = wait({ id: "wait-2", title: "Second" });
    const replacement = wait({ title: "Updated" });

    expect(applyDelegationWaitCreated([first], second)).toEqual([first, second]);
    expect(applyDelegationWaitCreated([first, second], replacement)).toEqual([
      replacement,
      second,
    ]);
  });

  it("removes consumed waits and leaves missing waits as a copy", () => {
    const first = wait();
    const second = wait({ id: "wait-2" });

    expect(applyDelegationWaitConsumed([first, second], "wait-1")).toEqual([second]);
    const unchanged = applyDelegationWaitConsumed([first], "missing-wait");
    expect(unchanged).toEqual([first]);
    expect(unchanged).not.toBe([first]);
  });
});
