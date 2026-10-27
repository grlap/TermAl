// Pins `stateDiff` admission, the pure merges, the transport handler's
// apply/ignore/resync effects, the partial writer in
// transcript-http-adoption.ts that voids the diff baseline, and the repair of
// delegation and wait deltas from a requested cut.
import { describe, expect, it, vi } from "vitest";
import {
  createStateDiffEventHandler,
  createStateDiffRepairCutRequester,
  decideStateDiffAdmission,
  mergeStateDiffRows,
  mergeStateDiffSessions,
  type StateDiffEvent,
  type StateDiffEventHandlerContext,
  type StateDiffRepairCutContext,
} from "./app-live-state-diff";
import { requestFullStateAlignment, requestStateCut } from "./api";
import { createAppLiveStateTransportEventHandlers } from "./app-live-state-transport-events";
import { applyHttpSessionEffects, type HttpSessionDecision } from "./transcript-http-adoption";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import type { Session } from "./types";

function admission(overrides: Partial<Parameters<typeof decideStateDiffAdmission>[0]> = {}) {
  // Baseline 5 (MB), current 7 (C), diff 5 -> 8.
  return decideStateDiffAdmission({
    baselineRevision: 5,
    currentRevision: 7,
    baseRevision: 5,
    revision: 8,
    lastSeenServerInstanceId: "server-a",
    serverInstanceId: "server-a",
    seenServerInstanceIds: new Set(["server-a", "retired"]),
    ...overrides,
  });
}

describe("stateDiff admission", () => {
  it("applies a diff from the baseline that is newer than the current revision", () => {
    expect(admission()).toBe("apply");
  });

  it("ignores a diff no newer than the baseline", () => {
    expect(admission({ revision: 5 })).toBe("ignore");
    expect(admission({ baseRevision: 3, revision: 4 })).toBe("ignore");
  });

  it("applies a diff from the baseline equal to the current revision", () => {
    // In-stream deltas moved the current revision past the baseline; the
    // requested cut lands at the last one's revision and covers it.
    expect(admission({ revision: 7 })).toBe("apply");
    expect(admission({ currentRevision: 9, revision: 9 })).toBe("apply");
  });

  it.each([
    ["no baseline", { baselineRevision: null }],
    ["a base other than the baseline", { baseRevision: 4 }],
    ["a revision behind the current one", { currentRevision: 9, revision: 8 }],
  ] as const)("asks for alignment, not an HTTP resync, for %s", (_, overrides) => {
    expect(admission(overrides)).toBe("align");
  });

  it("ignores a retired server's diff and resyncs for an unknown server", () => {
    expect(admission({ serverInstanceId: "retired" })).toBe("ignore");
    expect(admission({ serverInstanceId: "replacement" })).toBe("resync");
  });
});

describe("stateDiff merges", () => {
  const a = { id: "a", name: "A" };
  const b = { id: "b", name: "B" };
  const c = { id: "c", name: "C" };

  it("replaces changed entries in place and appends new ones in diff order", () => {
    const bChanged = { id: "b", name: "B2" };
    const d = { id: "d", name: "D" };
    const merged = mergeStateDiffSessions([a, b, c], [d, bChanged], []);
    expect(merged).toEqual([a, bChanged, c, d]);
    expect(merged[0]).toBe(a);
    expect(merged[2]).toBe(c);
  });

  it("removes listed ids and follows the order when present", () => {
    expect(mergeStateDiffSessions([a, b, c], [], ["b"])).toEqual([a, c]);
    const d = { id: "d", name: "D" };
    expect(mergeStateDiffSessions([a, b, c], [d], ["b"], ["d", "c", "a"])).toEqual([d, c, a]);
    // An id with no entry is skipped, never invented.
    expect(mergeStateDiffSessions([a], [], [], ["missing", "a"])).toEqual([a]);
  });

  it("keeps the list identity when a diff moves nothing", () => {
    const list = [a, b, c];
    expect(mergeStateDiffSessions(list, [], [])).toBe(list);
    expect(mergeStateDiffSessions(list, [b], [], ["a", "b", "c"])).toBe(list);
    expect(mergeStateDiffSessions(list, [], ["missing"])).toBe(list);
    expect(mergeStateDiffSessions(list, [], [], ["b", "a", "c"])).not.toBe(list);
    expect(mergeStateDiffSessions(list, [{ id: "b", name: "B2" }], [])).not.toBe(list);
  });

  it("lets a header-only diff keep the authority's session list", () => {
    const session = {
      id: "a", name: "A", emoji: "AI", agent: "Codex", workdir: "C:/repo", model: "default",
      status: "idle", preview: "", messages: [], messageCount: 0, messagesLoaded: true,
    } as Session;
    const owner = new TranscriptRepairAuthority(undefined, [session]);
    const before = owner.sessionsRef.current;
    expect(owner.adoptSummaryDiff([], [], undefined)).toBe(before);
    expect(owner.sessionsRef.current).toBe(before);
  });

  it("merges keyed rows with absolute changes and removals", () => {
    const rows = [{ runId: "r1", state: "running" }, { runId: "r2", state: "running" }];
    const merged = mergeStateDiffRows(
      rows,
      { changed: [{ runId: "r2", state: "passed" }, { runId: "r3", state: "running" }], removed: ["r1"] },
      (row) => row.runId,
    );
    expect(merged).toEqual([{ runId: "r2", state: "passed" }, { runId: "r3", state: "running" }]);
  });
});

function makeDiff(overrides: Partial<StateDiffEvent> = {}): StateDiffEvent {
  return {
    baseRevision: 5,
    revision: 8,
    serverInstanceId: "server-a",
    header: {
      codex: {},
      agentReadiness: [],
      preferences: {},
      projects: [],
      orchestrators: [],
      workspaces: [],
      delegationWaits: [],
      testRunWaits: [],
      pendingEngramMcpRevocationSessionIds: [],
    } as unknown as StateDiffEvent["header"],
    sessions: { changed: [], removed: [] },
    delegations: { changed: [], removed: [] },
    testRuns: { changed: [], removed: [] },
    ...overrides,
  };
}

function diffEvent(overrides: Partial<StateDiffEvent> = {}) {
  return new MessageEvent<string>("stateDiff", { data: JSON.stringify(makeDiff(overrides)) });
}

function makeContext(overrides: Partial<StateDiffEventHandlerContext> = {}): StateDiffEventHandlerContext {
  const sessions = [{ id: "a" }, { id: "b" }] as Session[];
  return {
    adoptStateDiff: vi.fn(() => true),
    beginBadLiveEventRecovery: vi.fn(),
    cancelStaleSendResponseRecoveryPollForSessions: vi.fn(),
    clearInitialStateResyncRetryTimeout: vi.fn(),
    clearReconnectStateResyncTimeoutAfterConfirmedReopen: vi.fn(),
    clearRecoveredBackendRequestError: vi.fn(),
    confirmReconnectRecoveryFromLiveEvent: vi.fn(() => true),
    confirmReconnectRecoveryFromStateEvent: vi.fn(() => true),
    isCancelled: () => false,
    lastLiveTransportActivityAtBySessionId: new Map([["gone", 1]]),
    lastSeenServerInstanceIdRef: { current: "server-a" },
    latestStateRevisionRef: { current: 7 },
    requestStateResync: vi.fn(),
    requestStateDiffAlignment: vi.fn(() => Promise.resolve()),
    seenServerInstanceIdsRef: { current: new Set(["server-a"]) },
    sessionsRef: { current: sessions },
    setBackendConnectionIssueDetail: vi.fn(),
    setIsLoading: vi.fn(),
    startSessionHydration: vi.fn(),
    stateDiffBaselineRevisionRef: { current: 5 },
    stateDiffAlignmentPendingRef: { current: false },
    syncLiveSessionResumeWatchdogBaselines: vi.fn(),
    syncLiveTransportActivityFromState: vi.fn(),
    transportState: {
      delegationRepairAdoptedSinceLastReconnectError: false,
      pendingBadLiveEventRecovery: false,
      sawReconnectOpenSinceLastError: false,
    },
    ...overrides,
  };
}

describe("stateDiff transport handler", () => {
  it("applies an admitted diff and refreshes transport activity for every known session", () => {
    const context = makeContext();
    createStateDiffEventHandler(context)(diffEvent());

    expect(context.adoptStateDiff).toHaveBeenCalledExactlyOnceWith(makeDiff());
    expect(context.requestStateResync).not.toHaveBeenCalled();
    const sessions = context.sessionsRef.current;
    expect(context.syncLiveTransportActivityFromState).toHaveBeenCalledExactlyOnceWith(sessions, expect.any(Number));
    expect(context.syncLiveSessionResumeWatchdogBaselines).toHaveBeenCalledExactlyOnceWith(sessions, expect.any(Number));
    expect(context.cancelStaleSendResponseRecoveryPollForSessions).toHaveBeenCalledWith(["a", "b"]);
    expect([...context.lastLiveTransportActivityAtBySessionId.keys()]).toEqual([]);
    expect(context.confirmReconnectRecoveryFromStateEvent).toHaveBeenCalledOnce();
  });

  it("ignores a diff no newer than the baseline without adopting or touching activity", () => {
    const context = makeContext();
    createStateDiffEventHandler(context)(diffEvent({ baseRevision: 4, revision: 5 }));

    expect(context.adoptStateDiff).not.toHaveBeenCalled();
    expect(context.requestStateResync).not.toHaveBeenCalled();
    expect(context.syncLiveTransportActivityFromState).not.toHaveBeenCalled();
    expect(context.confirmReconnectRecoveryFromLiveEvent).toHaveBeenCalledOnce();
  });

  it.each([
    ["no baseline", { stateDiffBaselineRevisionRef: { current: null } }, {}],
    ["a foreign base", {}, { baseRevision: 6 }],
    ["a diff behind the current revision", { latestStateRevisionRef: { current: 9 } }, { revision: 8 }],
  ] as const)("ignores and asks once for alignment with %s", (_, contextOverrides, diffOverrides) => {
    const context = makeContext(contextOverrides as Partial<StateDiffEventHandlerContext>);
    const handle = createStateDiffEventHandler(context);
    handle(diffEvent(diffOverrides));
    handle(diffEvent(diffOverrides));

    expect(context.requestStateDiffAlignment).toHaveBeenCalledOnce();
    expect(context.stateDiffAlignmentPendingRef.current).toBe(true);
    expect(context.requestStateResync).not.toHaveBeenCalled();
    expect(context.adoptStateDiff).not.toHaveBeenCalled();
    expect(context.syncLiveTransportActivityFromState).not.toHaveBeenCalled();
  });

  it("lets a later diff ask again after a failed alignment request", async () => {
    let fail: (error: Error) => void = () => {};
    const context = makeContext({
      stateDiffBaselineRevisionRef: { current: null },
      requestStateDiffAlignment: vi.fn(() => new Promise((_, reject) => {
        fail = reject;
      })),
    });
    const handle = createStateDiffEventHandler(context);
    handle(diffEvent());
    handle(diffEvent());
    expect(context.requestStateDiffAlignment).toHaveBeenCalledOnce();
    expect(context.requestStateResync).not.toHaveBeenCalled();

    fail(new Error("offline"));
    await Promise.resolve();
    await Promise.resolve();
    expect(context.stateDiffAlignmentPendingRef.current).toBe(false);
    expect(context.requestStateResync).toHaveBeenCalledExactlyOnceWith({ rearmOnFailure: true });
    handle(diffEvent());
    expect(context.requestStateDiffAlignment).toHaveBeenCalledTimes(2);
  });

  it("recovers over HTTP when the alignment request is rejected", async () => {
    const context = makeContext({
      stateDiffBaselineRevisionRef: { current: null },
      requestStateDiffAlignment: vi.fn(() => Promise.reject(new Error("503"))),
    });
    createStateDiffEventHandler(context)(diffEvent());
    await Promise.resolve();
    await Promise.resolve();

    expect(context.requestStateResync).toHaveBeenCalledExactlyOnceWith({ rearmOnFailure: true });
    expect(context.stateDiffAlignmentPendingRef.current).toBe(false);
  });

  it("recovers over HTTP when the alignment request throws synchronously", () => {
    const context = makeContext({
      stateDiffBaselineRevisionRef: { current: null },
      requestStateDiffAlignment: vi.fn(() => {
        throw new Error("no fetch");
      }),
    });
    createStateDiffEventHandler(context)(diffEvent());

    expect(context.requestStateResync).toHaveBeenCalledExactlyOnceWith({ rearmOnFailure: true });
    expect(context.stateDiffAlignmentPendingRef.current).toBe(false);
  });

  it("still resyncs over HTTP for an unknown server instance", () => {
    const context = makeContext();
    createStateDiffEventHandler(context)(diffEvent({ serverInstanceId: "replacement" }));

    expect(context.requestStateResync).toHaveBeenCalledExactlyOnceWith({ rearmOnFailure: true });
    expect(context.requestStateDiffAlignment).not.toHaveBeenCalled();
  });

  it("leaves no baseline and resyncs when applying a diff throws", () => {
    const context = makeContext({
      adoptStateDiff: vi.fn(() => {
        throw new Error("apply failed");
      }),
    });
    createStateDiffEventHandler(context)(diffEvent());

    expect(context.stateDiffBaselineRevisionRef.current).toBeNull();
    expect(context.requestStateResync).toHaveBeenCalledExactlyOnceWith({ rearmOnFailure: true });
    expect(context.syncLiveTransportActivityFromState).not.toHaveBeenCalled();
  });

  it("never consumes or clears the lagged force-adopt flag", () => {
    const forceAdoptNextStateEventRef = { current: true };
    const laggedRecoveryBaselineRevisionRef = { current: 4 };
    const clearForceAdoptNextStateEvent = vi.fn();
    const shouldForceAdoptNextStateEvent = vi.fn(() => true);
    const base = makeContext();
    const { handleStateDiffEvent } = createAppLiveStateTransportEventHandlers({
      ...base,
      clearForceAdoptNextStateEvent,
      forceAdoptNextStateEventRef,
      laggedRecoveryBaselineRevisionRef,
      shouldForceAdoptNextStateEvent,
    } as unknown as Parameters<typeof createAppLiveStateTransportEventHandlers>[0]);

    handleStateDiffEvent(diffEvent());
    handleStateDiffEvent(diffEvent({ baseRevision: 3, revision: 4 }));
    handleStateDiffEvent(diffEvent({ baseRevision: 6, revision: 9 }));

    expect(base.adoptStateDiff).toHaveBeenCalledOnce();
    expect(forceAdoptNextStateEventRef.current).toBe(true);
    expect(laggedRecoveryBaselineRevisionRef.current).toBe(4);
    expect(clearForceAdoptNextStateEvent).not.toHaveBeenCalled();
    expect(shouldForceAdoptNextStateEvent).not.toHaveBeenCalled();
  });
});

describe("partial HTTP session adoption", () => {
  function decision(revision: HttpSessionDecision["revision"]): HttpSessionDecision {
    return { outcome: "adopted", admission: "tail", revision, instance: { kind: "leave" } };
  }

  it("voids the diff baseline only when it advances the revision", () => {
    const refs = {
      latestRevision: { current: 5 as number | null },
      instance: { current: "server-a" as string | null },
      seenInstances: { current: new Set(["server-a"]) },
      stateDiffBaseline: { current: 5 as number | null },
    };
    applyHttpSessionEffects(decision({ kind: "leave" }), refs);
    expect(refs.stateDiffBaseline.current).toBe(5);
    applyHttpSessionEffects(decision({ kind: "set", value: 6 }), refs);
    expect(refs.latestRevision.current).toBe(6);
    expect(refs.stateDiffBaseline.current).toBeNull();
  });

  it("voids the diff baseline when it moves to another server instance", () => {
    const refs = {
      latestRevision: { current: 5 as number | null },
      instance: { current: "server-a" as string | null },
      seenInstances: { current: new Set(["server-a"]) },
      stateDiffBaseline: { current: 5 as number | null },
    };
    applyHttpSessionEffects({ ...decision({ kind: "leave" }), instance: { kind: "remember", value: "server-a" } }, refs);
    expect(refs.stateDiffBaseline.current).toBe(5);
    applyHttpSessionEffects({ ...decision({ kind: "leave" }), instance: { kind: "remember", value: "server-b" } }, refs);
    expect(refs.stateDiffBaseline.current).toBeNull();
  });
});

describe("alignment request", () => {
  it("posts a JSON body, never a CORS simple request", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    try {
      await requestFullStateAlignment();
    } finally {
      vi.unstubAllGlobals();
    }
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith(
      "/api/events/full-state",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      }),
    );
  });

  it("asks for a cut with a JSON mode on the same route", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    try {
      await requestStateCut();
    } finally {
      vi.unstubAllGlobals();
    }
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith(
      "/api/events/full-state",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: '{"mode":"cut"}',
      }),
    );
  });
});

/** A cut request the test settles by hand. */
function deferredCut() {
  const settles: Array<{ resolve: () => void; reject: (error: Error) => void }> = [];
  const requestStateCut = vi.fn(
    () =>
      new Promise<void>((resolve, reject) => {
        settles.push({ resolve: () => resolve(), reject });
      }),
  );
  return { requestStateCut, settles };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

function makeRepairContext(
  overrides: Partial<StateDiffRepairCutContext> = {},
): StateDiffRepairCutContext {
  return {
    isCancelled: () => false,
    isInChain: () => true,
    requestStateCut: vi.fn(() => Promise.resolve()),
    requestHttpRepair: vi.fn(),
    ...overrides,
  };
}

describe("repair-cut requester", () => {
  it("asks an in-chain repair for a cut, not an HTTP repair", async () => {
    const context = makeRepairContext();
    createStateDiffRepairCutRequester(context)(8);
    await flushMicrotasks();

    expect(context.requestStateCut).toHaveBeenCalledOnce();
    expect(context.requestHttpRepair).not.toHaveBeenCalled();
  });

  it("repairs over HTTP when the client is not in chain", () => {
    const context = makeRepairContext({ isInChain: () => false });
    createStateDiffRepairCutRequester(context)(8);

    expect(context.requestStateCut).not.toHaveBeenCalled();
    expect(context.requestHttpRepair).toHaveBeenCalledExactlyOnceWith(8);
  });

  it("falls back to the HTTP repair when the cut request fails", async () => {
    const rejected = makeRepairContext({
      requestStateCut: vi.fn(() => Promise.reject(new Error("503"))),
    });
    createStateDiffRepairCutRequester(rejected)(8);
    await flushMicrotasks();
    expect(rejected.requestHttpRepair).toHaveBeenCalledExactlyOnceWith(8);

    const thrown = makeRepairContext({
      requestStateCut: vi.fn(() => {
        throw new Error("no fetch");
      }),
    });
    createStateDiffRepairCutRequester(thrown)(8);
    expect(thrown.requestHttpRepair).toHaveBeenCalledExactlyOnceWith(8);
  });

  // The server may take the outstanding request's cut before a later
  // repair's commit; that repair is kept and asks once more, never dropped.
  it("keeps repairs asked for while a cut is outstanding and asks once more", async () => {
    const { requestStateCut, settles } = deferredCut();
    const context = makeRepairContext({ requestStateCut });
    const request = createStateDiffRepairCutRequester(context);
    request(8);
    request(9);
    request(10);
    expect(requestStateCut).toHaveBeenCalledOnce();

    settles[0].resolve();
    await flushMicrotasks();
    expect(requestStateCut).toHaveBeenCalledTimes(2);

    settles[1].resolve();
    await flushMicrotasks();
    expect(requestStateCut).toHaveBeenCalledTimes(2);
    expect(context.requestHttpRepair).not.toHaveBeenCalled();
  });

  it("covers a kept repair over HTTP when the outstanding cut fails", async () => {
    const { requestStateCut, settles } = deferredCut();
    const context = makeRepairContext({ requestStateCut });
    const request = createStateDiffRepairCutRequester(context);
    request(8);
    request(10);
    settles[0].reject(new Error("offline"));
    await flushMicrotasks();

    expect(context.requestHttpRepair).toHaveBeenCalledExactlyOnceWith(10);
    expect(requestStateCut).toHaveBeenCalledOnce();
  });

  it("repairs a kept request over HTTP once the client has left the chain", async () => {
    const { requestStateCut, settles } = deferredCut();
    let inChain = true;
    const context = makeRepairContext({ requestStateCut, isInChain: () => inChain });
    const request = createStateDiffRepairCutRequester(context);
    request(8);
    request(9);
    inChain = false;
    settles[0].resolve();
    await flushMicrotasks();

    expect(requestStateCut).toHaveBeenCalledOnce();
    expect(context.requestHttpRepair).toHaveBeenCalledExactlyOnceWith(9);
  });

  it("does nothing once cancelled", async () => {
    let cancelled = false;
    const rejected = makeRepairContext({
      isCancelled: () => cancelled,
      requestStateCut: vi.fn(() => Promise.reject(new Error("closed"))),
    });
    const request = createStateDiffRepairCutRequester(rejected);
    request(8);
    cancelled = true;
    request(9);
    await flushMicrotasks();

    expect(rejected.requestStateCut).toHaveBeenCalledOnce();
    expect(rejected.requestHttpRepair).not.toHaveBeenCalled();
  });
});

describe("authoritative-repair delta transport", () => {
  function delegationDeltaEvent(revision: number) {
    return new MessageEvent<string>("delta", {
      data: JSON.stringify({ type: "delegationCompleted", revision, delegationId: "d1" }),
    });
  }

  function makeHandlers(overrides: Partial<StateDiffEventHandlerContext> = {}) {
    const base = makeContext(overrides);
    const extra = {
      applyDelegationWaitDeltaLocally: vi.fn(),
      confirmReconnectRecoveryFromDeltaEvent: vi.fn(() => true),
      requestStateCut: vi.fn(() => Promise.resolve()),
      setLastDelegationRepairRequestedRevision: vi.fn(),
    };
    const handlers = createAppLiveStateTransportEventHandlers({
      ...base,
      ...extra,
    } as unknown as Parameters<typeof createAppLiveStateTransportEventHandlers>[0]);
    return { base, extra, handlers };
  }

  it("repairs an in-chain delegation delta from a requested cut", async () => {
    const { base, extra, handlers } = makeHandlers();
    handlers.handleDeltaEvent(delegationDeltaEvent(8));
    await flushMicrotasks();

    expect(extra.applyDelegationWaitDeltaLocally).toHaveBeenCalledOnce();
    expect(extra.requestStateCut).toHaveBeenCalledOnce();
    expect(base.requestStateResync).not.toHaveBeenCalled();
    expect(extra.setLastDelegationRepairRequestedRevision).not.toHaveBeenCalled();
    expect(base.latestStateRevisionRef.current).toBe(7);
    expect(base.stateDiffBaselineRevisionRef.current).toBe(5);
  });

  it.each([
    ["no baseline", { stateDiffBaselineRevisionRef: { current: null } }],
    ["an alignment pending", { stateDiffAlignmentPendingRef: { current: true } }],
    [
      "a bad live event awaiting repair",
      {
        transportState: {
          delegationRepairAdoptedSinceLastReconnectError: false,
          pendingBadLiveEventRecovery: true,
          sawReconnectOpenSinceLastError: false,
        },
      },
    ],
  ] as const)("keeps the HTTP repair with %s", (_, overrides) => {
    const { base, extra, handlers } = makeHandlers(
      overrides as Partial<StateDiffEventHandlerContext>,
    );
    handlers.handleDeltaEvent(delegationDeltaEvent(8));

    expect(extra.requestStateCut).not.toHaveBeenCalled();
    expect(extra.setLastDelegationRepairRequestedRevision).toHaveBeenCalledExactlyOnceWith(8);
    expect(base.requestStateResync).toHaveBeenCalledExactlyOnceWith({
      allowAuthoritativeRollback: true,
      forceAdoptEqualOrNewerRevision: 8,
      rearmOnFailure: true,
    });
  });

  it("applies the requested cut's diff at the current revision and keeps the chain", () => {
    // A session delta moved the current revision to 8 after the baseline at
    // 5; the cut requested for the delegation delta lands at 8 too.
    const context = makeContext({ latestStateRevisionRef: { current: 8 } });
    createStateDiffEventHandler(context)(diffEvent({ baseRevision: 5, revision: 8 }));

    expect(context.adoptStateDiff).toHaveBeenCalledOnce();
    expect(context.requestStateDiffAlignment).not.toHaveBeenCalled();
    expect(context.requestStateResync).not.toHaveBeenCalled();
  });
});
