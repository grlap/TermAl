import { describe, expect, it, vi } from "vitest";

import { confirmAuthoritativeSnapshotReconnectRecovery } from "./app-live-state-transport";

describe("confirmAuthoritativeSnapshotReconnectRecovery", () => {
  it.each([true, false])(
    "propagates shared reconnect finish result %s",
    (finishResult) => {
      const calls: string[] = [];
      const reconnectState = {
        confirmAuthoritativeSnapshot: vi.fn(() => {
          calls.push("confirm");
        }),
      };
      const finishReconnectRecoveryConfirmation = vi.fn(() => {
        calls.push("finish");
        return finishResult;
      });

      expect(
        confirmAuthoritativeSnapshotReconnectRecovery({
          reconnectState,
          finishReconnectRecoveryConfirmation,
        }),
      ).toBe(finishResult);
      expect(calls).toEqual(["confirm", "finish"]);
      expect(reconnectState.confirmAuthoritativeSnapshot).toHaveBeenCalledTimes(
        1,
      );
      expect(finishReconnectRecoveryConfirmation).toHaveBeenCalledTimes(1);
    },
  );
});
