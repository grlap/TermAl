import {
  decideDeltaRevisionAction,
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
});
