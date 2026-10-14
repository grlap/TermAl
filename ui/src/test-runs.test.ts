import { describe, expect, it } from "vitest";
import { applyTestRunDelta, reconcileTestRunSnapshot, sortTestRuns, testRunRestartImpact, testRunSessionMarker, testRunStageLabel } from "./test-runs";
import { makeTestRun, makeTestRunDetail } from "./test-runs-fixtures";

describe("test run presentation", () => {
  it("does not invent detachment for a host-observed provider command", () => {
    const run = makeTestRun({ registrationState: "hostObserved", detached: null, currentStage: null });
    expect(testRunSessionMarker([run], "session-owner")).toBe("test running");
  });
  it("aggregates restart impact across every run and keeps uncertainty distinct from safe", () => {
    const safe = makeTestRun({ state: "failed", restartWouldVoidEvidence: false });
    const unknown = makeTestRun({ runId: "unknown", state: "passed", restartWouldVoidEvidence: null });
    const risk = makeTestRun({ runId: "pending", state: "passed", restartWouldVoidEvidence: true });
    expect(testRunRestartImpact([safe])).toBe(false);
    expect(testRunRestartImpact([safe, unknown])).toBeNull();
    expect(testRunRestartImpact([unknown, risk, safe])).toBe(true);
    expect(testRunRestartImpact([risk, unknown])).toBe(true);
    expect(testRunRestartImpact([makeTestRun({ state: "passed" })])).toBeNull();
  });
  it("replaces only positively correlated observed identities in one live update", () => {
    const observed = makeTestRun({ runId: "observed-original" });
    const other = makeTestRun({ runId: "observed-other" });
    const artifact = makeTestRun({ runId: "launcher", supersededRunIds: [observed.runId],
      trackingProvenances: ["hostObserved", "discovered"], restartWouldVoidEvidence: true });
    expect(applyTestRunDelta([observed, other], { type: "testRunChanged", revision: 2, run: artifact }))
      .toEqual(sortTestRuns([other, artifact]));
    const restored = applyTestRunDelta([artifact], { type: "testRunChanged", revision: 3, run: observed });
    expect(applyTestRunDelta(restored, { type: "testRunRemoved", revision: 4, runId: artifact.runId }))
      .toEqual([observed]);
  });
  it.each(["processGone", "resultsUnreadable", "noPid", "heartbeatStale"] as const)("adopts unknown reason %s without inferring a missing reason", unknownReason => {
    const run = makeTestRun({ state: "unknown" });
    expect(run.unknownReason).toBeUndefined();
    const incoming = { ...run, unknownReason };
    const updated = reconcileTestRunSnapshot([run], [incoming]);
    expect(updated[0]).toBe(incoming);
    expect(reconcileTestRunSnapshot(updated, [{ ...incoming }])).toBe(updated);
    expect(reconcileTestRunSnapshot(updated, [run])[0]?.unknownReason).toBeUndefined();
    expect(applyTestRunDelta([run], { type: "testRunChanged", revision: 2, run: incoming })[0]).toBe(incoming);
  });
  it("reuses equal versioned snapshot summaries independently of JSON field order", () => {
    const run = makeTestRun();
    const previous = [run];
    const reordered = Object.fromEntries(Object.entries(run).reverse()) as unknown as typeof run;
    expect(reconcileTestRunSnapshot(previous, [reordered])).toBe(previous);
    expect(reconcileTestRunSnapshot(previous, [{ ...run, detailVersion: "detail-v2" }])[0]).not.toBe(run);
    expect(reconcileTestRunSnapshot(previous, [{ ...run, stages: [{ name: "new", state: "running", exitCode: null, startedAt: null, endedAt: null }] }])[0]).not.toBe(run);
  });
  it.each([null, undefined])("invalidates equal snapshots without a detail version (%s)", detailVersion => {
    const run = makeTestRun({ detailVersion });
    expect(reconcileTestRunSnapshot([run], [{ ...run }])[0]).not.toBe(run);
  });
  it("sorts newest first, breaks ties by id and puts missing timestamps last", () => {
    const a = makeTestRun({ runId: "a" });
    const b = makeTestRun({ runId: "b" });
    const old = makeTestRun({ runId: "old", startedAt: null });
    expect(sortTestRuns([old, b, a])).toEqual([a, b, old]);
  });
  it("adopts equal-summary events as detail invalidations and removes by id", () => {
    const run = makeTestRun();
    const replacement = { ...run };
    const updated = applyTestRunDelta([run], { type: "testRunChanged", revision: 2, run: replacement });
    expect(updated[0]).toBe(replacement);
    expect(applyTestRunDelta(updated, { type: "testRunRemoved", revision: 3, runId: run.runId })).toEqual([]);
  });
  it("uses foreground/background/notification role precedence and counts only that role", () => {
    const foreground = makeTestRun();
    const background = makeTestRun({ runId: "background", detached: true });
    const notification = makeTestRun({ runId: "notification", ownerSessionId: "other", notifySessionId: "session-owner" });
    expect(testRunSessionMarker([notification, background, foreground], "session-owner")).toBe("running tests · rust-tests");
    expect(testRunSessionMarker([foreground, makeTestRun({ runId: "two" })], "session-owner")).toBe("running tests (2)");
    expect(testRunSessionMarker([background, notification], "session-owner")).toBe("test run in background · rust-tests");
    expect(testRunSessionMarker([notification], "session-owner")).toBe("test run will notify this session · rust-tests");
    expect(testRunSessionMarker([makeTestRun({ detached: null })], "session-owner")).toBe("test run in background · rust-tests");
  });
  it.each(["passed", "failed", "unknown"] as const)("clears markers for %s instead of claiming an agent is waiting", state => {
    expect(testRunSessionMarker([makeTestRun({ state })], "session-owner")).toBeNull();
    expect(testRunSessionMarker([makeTestRun({ state })], "session-coordinator")).toBeNull();
  });
  it("does not infer notification identity from its unresolved raw target", () => {
    expect(testRunSessionMarker([makeTestRun({ notifySessionId: null, notifyTo: "session-coordinator" })], "session-coordinator")).toBeNull();
  });
  it("labels a running stage in unknown or recovered-interrupted runs honestly", () => {
    const stage = makeTestRunDetail().stages[0];
    expect(testRunStageLabel(makeTestRun({ state: "unknown" }), stage)).toBe("interrupted");
    expect(testRunStageLabel(makeTestRun({ state: "failed", interrupted: true }), stage)).toBe("interrupted");
    expect(testRunStageLabel(makeTestRun(), { ...stage, state: "unrun" })).toBe("unrun");
  });
});
