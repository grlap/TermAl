import {
  decideDeltaRevisionAction,
  shouldAdoptStateRevision,
} from "./state-revision";

describe("state revision helpers", () => {
  it("adopts the first snapshot and newer snapshots only", () => {
    expect(shouldAdoptStateRevision(null, 0)).toBe(true);
    expect(shouldAdoptStateRevision(4, 4)).toBe(false);
    expect(shouldAdoptStateRevision(4, 3)).toBe(false);
    expect(shouldAdoptStateRevision(4, 5)).toBe(true);
  });

  it("applies only contiguous delta revisions", () => {
    expect(decideDeltaRevisionAction(null, 1)).toBe("resync");
    expect(decideDeltaRevisionAction(4, 4)).toBe("ignore");
    expect(decideDeltaRevisionAction(4, 3)).toBe("ignore");
    expect(decideDeltaRevisionAction(4, 5)).toBe("apply");
    expect(decideDeltaRevisionAction(4, 6)).toBe("resync");
  });
});
