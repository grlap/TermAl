import {
  decideDeltaRevisionAction,
  isServerInstanceMismatch,
  shouldAdoptStateRevision,
  shouldAdoptSnapshotRevision,
} from "./state-revision";

describe("state revision helpers", () => {
  it("adopts the first snapshot and newer snapshots only", () => {
    expect(shouldAdoptStateRevision(null, 0)).toBe(true);
    expect(shouldAdoptStateRevision(4, 4)).toBe(false);
    expect(shouldAdoptStateRevision(4, 3)).toBe(false);
    expect(shouldAdoptStateRevision(4, 5)).toBe(true);
  });

  it("guards forced revision downgrades unless explicitly allowed", () => {
    expect(
      shouldAdoptSnapshotRevision(4, 4, {
        force: true,
        allowRevisionDowngrade: false,
      }),
    ).toBe(true);
    expect(
      shouldAdoptSnapshotRevision(4, 3, {
        force: true,
        allowRevisionDowngrade: false,
      }),
    ).toBe(false);
    expect(
      shouldAdoptSnapshotRevision(4, 3, {
        force: true,
        allowRevisionDowngrade: true,
      }),
    ).toBe(true);
    expect(
      shouldAdoptSnapshotRevision(4, 3, {
        force: true,
      }),
    ).toBe(false);
  });

  it("delegates non-forced snapshot adoption to the standard revision helper", () => {
    expect(shouldAdoptSnapshotRevision(4, 5)).toBe(true);
    expect(
      shouldAdoptSnapshotRevision(4, 4, {
        force: false,
        allowRevisionDowngrade: true,
      }),
    ).toBe(false);
    expect(
      shouldAdoptSnapshotRevision(4, 3, {
        force: false,
        allowRevisionDowngrade: true,
      }),
    ).toBe(false);
  });

  it("allows forced adoption when there is no current revision to downgrade from", () => {
    expect(
      shouldAdoptSnapshotRevision(null, 0, {
        force: true,
        allowRevisionDowngrade: false,
      }),
    ).toBe(true);
  });

  it("applies only contiguous delta revisions", () => {
    expect(decideDeltaRevisionAction(null, 1)).toBe("resync");
    expect(decideDeltaRevisionAction(4, 4)).toBe("ignore");
    expect(decideDeltaRevisionAction(4, 3)).toBe("ignore");
    expect(decideDeltaRevisionAction(4, 5)).toBe("apply");
    expect(decideDeltaRevisionAction(4, 6)).toBe("resync");
  });

  describe("server instance mismatch", () => {
    it("detects a mismatch only when both ids are non-empty and differ", () => {
      expect(isServerInstanceMismatch("a", "b")).toBe(true);
      expect(isServerInstanceMismatch("a", "a")).toBe(false);
    });

    it("treats empty / null ids as unknown (never a restart signal)", () => {
      expect(isServerInstanceMismatch(null, "a")).toBe(false);
      expect(isServerInstanceMismatch("a", null)).toBe(false);
      expect(isServerInstanceMismatch("", "a")).toBe(false);
      expect(isServerInstanceMismatch("a", "")).toBe(false);
      expect(isServerInstanceMismatch(undefined, "a")).toBe(false);
      expect(isServerInstanceMismatch("a", undefined)).toBe(false);
      expect(isServerInstanceMismatch(null, null)).toBe(false);
      expect(isServerInstanceMismatch("", "")).toBe(false);
    });

    it("accepts a revision downgrade when the server instance id changed", () => {
      // Simulates: server restarted, revision rewound from 237 to 213.
      // Without the restart signal this would be rejected as stale;
      // with the id mismatch it's accepted unconditionally.
      expect(
        shouldAdoptSnapshotRevision(237, 213, {
          lastSeenServerInstanceId: "uuid-before-restart",
          nextServerInstanceId: "uuid-after-restart",
        }),
      ).toBe(true);
    });

    it("rejects a late response from a previously seen old server instance", () => {
      expect(
        shouldAdoptSnapshotRevision(237, 213, {
          lastSeenServerInstanceId: "uuid-after-restart",
          nextServerInstanceId: "uuid-before-restart",
          seenServerInstanceIds: new Set([
            "uuid-before-restart",
            "uuid-after-restart",
          ]),
          force: true,
          allowRevisionDowngrade: true,
        }),
      ).toBe(false);
    });

    it("accepts a revision downgrade for an unseen replacement instance", () => {
      expect(
        shouldAdoptSnapshotRevision(237, 213, {
          lastSeenServerInstanceId: "uuid-before-restart",
          nextServerInstanceId: "uuid-after-restart",
          seenServerInstanceIds: new Set(["uuid-before-restart"]),
        }),
      ).toBe(true);
    });

    it("still rejects a stale revision on the same server instance", () => {
      // No restart — just an out-of-order stale response. Keep the
      // monotonic guard.
      expect(
        shouldAdoptSnapshotRevision(5, 3, {
          lastSeenServerInstanceId: "uuid-steady",
          nextServerInstanceId: "uuid-steady",
        }),
      ).toBe(false);
    });

    it("treats an empty incoming instance id as unknown, preserving the monotonic guard", () => {
      // Fallback SSE payload / older server — do NOT infer a restart.
      expect(
        shouldAdoptSnapshotRevision(5, 3, {
          lastSeenServerInstanceId: "uuid-known",
          nextServerInstanceId: "",
        }),
      ).toBe(false);
    });

    it("restart signal wins over the explicit allowRevisionDowngrade: false gate", () => {
      // Safety-net poll used to pass { force: true, allowRevisionDowngrade: true }
      // unconditionally; after the unification it passes no options,
      // but the restart branch must still fire on genuine restarts.
      expect(
        shouldAdoptSnapshotRevision(5, 3, {
          force: true,
          allowRevisionDowngrade: false,
          lastSeenServerInstanceId: "uuid-before",
          nextServerInstanceId: "uuid-after",
        }),
      ).toBe(true);
    });
  });
});
