// Streaming coverage and repair-admission regressions. Network boundaries are
// deferred, not product decisions: real transport, reducer, classification,
// React list-identity effects and record-store publication run together.
// The schedules prove mechanisms; the historical browser burst has no per-
// request reasons, so similar measured rates alone do not establish attribution.
import { act, cleanup, renderHook } from "@testing-library/react";
import { useMemo, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import { ApiRequestError } from "./api-request";
import { useAppLiveState } from "./app-live-state";
import { withLiveSessionAuthority, type TestLiveStateParams } from "./session-publication-test-fixtures";
import { SESSION_HYDRATION_RETRY_DELAYS_MS } from "./app-live-state-hydration";
import * as hydration from "./app-live-state-hydration";
import { requestSessionHistoryPage, requestSessionHistoryOlderPage, requestSessionHistoryStartPage } from "./session-history-demand";
import * as transport from "./app-live-state-transport";
import type { UseAppLiveStateParams } from "./app-live-state-types";
import * as deltas from "./live-updates";
import * as adoption from "./session-hydration-adoption";
import * as revisions from "./state-revision";
import {
  getSessionRecordSnapshotForTesting,
  resetSessionStoreForTesting,
} from "./session-store";
import { __resetSessionHydrationPerformanceForTests } from "./session-hydration-performance";
import type { Session, StateSessionSummary, TextMessage } from "./types";

const SESSION_ID = "session-5651";
const MESSAGE_ID = "message-681028";
const INSTANCE = "stream-test-instance";
const TOTAL = 1874;

class Stream extends EventTarget {
  static latest: Stream;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readyState = 1;
  constructor() {
    super();
    Stream.latest = this;
  }
  close() {
    this.readyState = 2;
  }
  emit(type: string, payload: unknown) {
    this.dispatchEvent(new MessageEvent(type, { data: JSON.stringify(payload) }));
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function summary(revision: number, total = TOTAL): api.StateResponse {
  const session: StateSessionSummary = {
    id: SESSION_ID, name: "Streaming session", emoji: "AI", agent: "Codex",
    workdir: "C:/workspace", model: "codex", status: "active", preview: "",
    messageCount: total, sessionMutationStamp: revision, queuePaused: false,
  };
  return {
    revision, serverInstanceId: INSTANCE, codex: {}, agentReadiness: [],
    preferences: {
      defaultCodexModel: "default", defaultCodexSandboxMode: "workspace-write",
      defaultCodexApprovalPolicy: "on-request", defaultClaudeModel: "default",
      defaultCursorModel: "default", defaultGeminiModel: "default",
      defaultOpenCodeModel: "default", defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask", defaultClaudeEffort: "default",
    },
    projects: [], orchestrators: [], workspaces: [], sessions: [session],
  };
}

function message(text: string, id = MESSAGE_ID): TextMessage {
  return { id, type: "text", author: "assistant", timestamp: "10:00", text };
}

function tail(revision: number, text: string): Awaited<ReturnType<typeof api.fetchSessionTail>> {
  const messages = Array.from({ length: 19 }, (_, index) =>
    message(`Retained ${index}`, index === 0 ? "message-680851" : `retained-${index}`),
  );
  messages.push(message(text));
  return {
    revision, serverInstanceId: INSTANCE,
    session: { ...summary(revision).sessions[0], messages, messagesLoaded: false },
  };
}

function textDelta(revision: number, textStartByte: number, delta = "x") {
  return {
    type: "textDelta", revision, sessionId: SESSION_ID, messageId: MESSAGE_ID,
    messageIndex: TOTAL - 1, messageCount: TOTAL, sessionMutationStamp: revision,
    textStartByte, delta,
  };
}

function makeParams(): TestLiveStateParams {
  const set = vi.fn();
  return withLiveSessionAuthority({
    adoptionRefs: {
      isMountedRef: { current: true }, latestStateRevisionRef: { current: null },
      lastSeenServerInstanceIdRef: { current: null },
      seenServerInstanceIdsRef: { current: new Set() }, sessionsRef: { current: [] },
      draftsBySessionIdRef: { current: {} }, draftAttachmentsBySessionIdRef: { current: {} },
      codexStateRef: { current: {} }, agentReadinessRef: { current: [] },
      projectsRef: { current: [] }, orchestratorsRef: { current: [] },
      delegationWaitsRef: { current: [] }, workspaceSummariesRef: { current: [] },
      refreshingAgentCommandSessionIdsRef: { current: {} },
      confirmedUnknownModelSendsRef: { current: new Set() },
      activePromptPollCancelRef: { current: null }, activePromptPollSessionIdRef: { current: null },
    },
    stateSetters: {
      setSessions: set, setWorkspace: set, setCodexState: set, setAgentReadiness: set,
      setProjects: set, setOrchestrators: set, setDelegationWaits: set,
      setDelegationChildSessionIds: set, setWorkspaceSummaries: set,
      setDraftsBySessionId: set, setDraftAttachmentsBySessionId: set,
      setSendingSessionIds: set, setStoppingSessionIds: set, setKillingSessionIds: set,
      setKillRevealSessionId: set, setPendingKillSessionId: set,
      setPendingSessionRename: set, setUpdatingSessionIds: set,
      setAgentCommandsBySessionId: set, setRefreshingAgentCommandSessionIds: set,
      setAgentCommandErrors: set, setSessionSettingNotices: set, setSelectedProjectId: set,
      setIsLoading: set, setHasAdoptedStateSnapshot: set,
      setBackendConnectionIssueDetail: set, setBackendConnectionState: set,
    },
    preferenceSetters: {
      setDefaultCodexModel: set, setDefaultCodexSandboxMode: set,
      setDefaultCodexApprovalPolicy: set, setDefaultClaudeModel: set,
      setDefaultCursorModel: set, setDefaultGeminiModel: set, setDefaultOpenCodeModel: set,
      setDefaultKimiModel: set,
      setDefaultKimiApprovalMode: set,
      setDefaultKimiEffort: set,
      setDefaultOpenCodeApprovalMode: set,
      setDefaultCodexReasoningEffort: set,
      setDefaultClaudeApprovalMode: set, setDefaultClaudeEffort: set,
      setRemoteConfigs: set, setTelegramConfig: set, setEngramHostSettings: set,
    },
    applyControlPanelLayout: (workspace) => workspace,
    clearRecoveredBackendRequestError: vi.fn(), reportRequestError: vi.fn(),
    requestBackendReconnectRef: { current: vi.fn() },
    requestActionRecoveryResyncRef: { current: vi.fn() },
    activeSession: null, activeTranscriptSessionId: null, visibleSessionHydrationTargets: [],
  });
}

async function flush(ms = 0) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}

function countObservationScans() {
  const counts = { sessionScans: 0, messageScans: 0 };
  const find = Array.prototype.find;
  const some = Array.prototype.some;
  // Attribute only the extra observation work, not legitimate reducer/render
  // scans. The in-flight control below proves these probes see the real path.
  const inObservation = () => /\bat observeDelta\b/.test(new Error().stack ?? "");
  vi.spyOn(Array.prototype, "find").mockImplementation(function (this: unknown[], predicate, thisArg) {
    if (inObservation()) counts.sessionScans += 1;
    return find.call(this, predicate, thisArg);
  });
  vi.spyOn(Array.prototype, "some").mockImplementation(function (this: unknown[], predicate, thisArg) {
    if (inObservation()) counts.messageScans += 1;
    return some.call(this, predicate, thisArg);
  });
  return counts;
}

function setup(
  observerMode: "normal" | "absent" | "throws" = "normal",
  predicateMode: "normal" | "absent" | "throws" = "normal",
) {
  const params = makeParams();
  const tailRequests: Array<ReturnType<typeof deferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>> & {
    stack: string; at: number; watchdog: boolean;
  }> = [];
  const watchdogStartedAt = Date.now();
  let inWatchdog = false;
  let watchdogFirings = 0;
  const setInterval = window.setInterval.bind(window);
  // Observe the real interval callback without replacing its decisions.
  vi.spyOn(window, "setInterval").mockImplementation((handler, delay, ...args) =>
    setInterval(typeof handler === "function" && delay === 1000 ? () => {
      watchdogFirings++;
      inWatchdog = true;
      try { handler(...args); } finally { inWatchdog = false; }
    } : handler, delay, ...args));
  const historyRequests: Array<ReturnType<typeof deferred<Awaited<ReturnType<typeof api.fetchSessionHistory>>>>> = [];
  // Leave state recovery pending until a schedule explicitly sends a state
  // event. No hidden HTTP snapshot may change the revision baseline.
  vi.spyOn(api, "fetchState").mockImplementation(() => new Promise(() => {}));
  const fetchTail = vi.spyOn(api, "fetchSessionTail").mockImplementation(() => {
    const request = { ...deferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>(),
      stack: new Error().stack ?? "", at: Date.now(), watchdog: inWatchdog };
    tailRequests.push(request);
    return request.promise;
  });
  vi.spyOn(api, "fetchSessionHistory").mockImplementation(() => {
    const request = deferred<Awaited<ReturnType<typeof api.fetchSessionHistory>>>();
    historyRequests.push(request);
    return request.promise;
  });
  const reducer = vi.spyOn(deltas, "applyDeltaToSessions");
  const classify = vi.spyOn(adoption, "classifyFetchedSessionAdoption");
  const revisionAction = vi.spyOn(revisions, "decideDeltaRevisionAction");
  const recoveryCalls: Array<Parameters<Parameters<typeof transport.useAppLiveStateTransport>[0]["startSessionHydration"]>> = [];
  let startHydration!: Parameters<typeof transport.useAppLiveStateTransport>[0]["startSessionHydration"];
  const realTransport = transport.useAppLiveStateTransport;
  vi.spyOn(transport, "useAppLiveStateTransport").mockImplementation((args) => {
    startHydration = args.startSessionHydration;
    return realTransport({ ...args,
      hasPartialTailAppendProof: predicateMode === "absent" ? undefined
        : predicateMode === "throws" ? () => { throw new Error("Predicate failure"); }
          : args.hasPartialTailAppendProof,
      observeHydrationDelta: observerMode === "absent" ? undefined
        : observerMode === "throws" ? () => { throw new Error("Observer failure"); }
          : args.observeHydrationDelta,
      startSessionHydration: (...call) => {
      recoveryCalls.push(call);
      return args.startSessionHydration(...call);
    } });
  });
  let renderedSessions: Session[] = [];
  const rendered = renderHook(({ visible }) => {
    const [sessions, setSessions] = useState<Session[]>([]);
    renderedSessions = sessions;
    params.stateSetters.setSessions = setSessions;
    const activeSession = visible ? sessions.find((entry) => entry.id === SESSION_ID) ?? null : null;
    // Match App: targets depend on the session-list identity, not only loaded.
    const targets = useMemo(() => activeSession
      ? [{ id: activeSession.id, messagesLoaded: activeSession.messagesLoaded }] : [],
    [sessions, visible]);
    return useAppLiveState({ ...params,
      stateSetters: params.stateSetters, activeSession,
      activeTranscriptSessionId: visible ? SESSION_ID : null,
      visibleSessionHydrationTargets: targets,
    });
  }, { initialProps: { visible: false } });
  const emit = (kind: string, payload: unknown) => act(() => Stream.latest.emit(kind, payload));
  act(() => Stream.latest.onopen?.(new Event("open")));
  emit("state", summary(100, TOTAL - 1));
  emit("delta", {
    type: "messageCreated", revision: 101, sessionId: SESSION_ID, messageId: MESSAGE_ID,
    messageIndex: TOTAL - 1, messageCount: TOTAL, sessionMutationStamp: 101,
    message: message("a"), preview: "", status: "active",
  });
  const current = () => params.adoptionRefs.sessionsRef.current[0];
  expect(current()).toMatchObject({ messageStartIndex: 1873, messageCount: TOTAL,
    messagesLoaded: false, hasOlderHistory: true, hasNewerHistory: false });
  expect(current().messages).toEqual([message("a")]);
  expect(fetchTail).not.toHaveBeenCalled();
  return { ...rendered, emit, current, params, fetchTail, tailRequests, historyRequests,
    reducer, classify, revisionAction, recoveryCalls,
    renderedSession: () => renderedSessions.find((entry) => entry.id === SESSION_ID),
    watchdogFirings: () => watchdogFirings,
    async beforeNextBackstop() {
      const elapsed = Date.now() - watchdogStartedAt;
      await flush((Math.floor(elapsed / 3000) + 1) * 3000 - elapsed - 1);
    },
    startHydration: (...args: Parameters<typeof startHydration>) => startHydration(...args),
    async visible(value: boolean) { rendered.rerender({ visible: value }); await flush(16); },
    async resolveTail(index: number, revision: number, text: string) {
      await act(async () => { tailRequests[index].resolve(tail(revision, text)); });
      await flush();
    },
    expectTailCount(count: number) {
      expect(fetchTail.mock.calls).toEqual(Array.from({ length: count }, () => [SESSION_ID, 20]));
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", Stream);
  vi.stubGlobal("fetch", vi.fn(() => { throw new Error("Unexpected real network request"); }));
});

afterEach(() => {
  cleanup();
  resetSessionStoreForTesting();
  __resetSessionHydrationPerformanceForTests();
  vi.clearAllTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const COMPOSITE_WINDOW_MS = 2559;

// Seed proof through the real hook's first tail answer, not raw Session fields.
async function pairedRetryFixture(fullyLoaded = true) {
  const h = setup();
  await h.visible(true);
  const response = tail(101, "a");
  if (fullyLoaded) response.session = { ...response.session,
    messages: [...Array.from({ length: TOTAL - 1 }, (_, index) =>
      message(`Loaded ${index}`, `loaded-${index}`)), message("a")],
    messagesLoaded: true };
  response.session = { ...response.session, bodySeq: 0, bodySeqEpoch: INSTANCE };
  await act(async () => h.tailRequests[0].resolve(response));
  await flush();
  expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)?.appliedSeq).toBe(0);
  h.expectTailCount(1);
  return h;
}

async function startRejectedOwnerRead(h: Awaited<ReturnType<typeof pairedRetryFixture>>) {
  const ahead = summary(102);
  // Keep the queue disposition unchanged so this is owner demand, not the
  // independent master queue-metadata hydration route.
  ahead.sessions[0] = { ...ahead.sessions[0], bodySeq: 1, bodySeqEpoch: INSTANCE,
    queuePaused: h.current().queuePaused === true };
  h.emit("state", ahead);
  await flush();
  h.expectTailCount(2);
  expect(h.tailRequests[1].stack, "the rejected flight must originate at the owner requester")
    .toContain("requestSessionTailRead");
  await act(async () => h.tailRequests[1].reject(new Error("Tail unavailable")));
  expect(h.params.reportRequestError).toHaveBeenCalledTimes(1);
}

describe("owner retry initiation witnesses", () => {
  it.each(["standing", "closed", "loaded"] as const)(
    "shared retry slot keeps the master action when owner demand becomes %s", async (demand) => {
      const h = await pairedRetryFixture(false);
      // An earlier action's metadata already carries the upcoming summary's
      // stamp, but is not a body certificate. This keeps the separate master
      // unseen-stamp partial hydration from taking the owner's first flight.
      act(() => h.params.sessionAuthority.commit([{ ...h.current(),
        sessionMutationStamp: 102 }], "metadata"));
      await startRejectedOwnerRead(h);
      expect(h.current().messagesLoaded).toBe(false);

      // Older-prefetch is a real master origin. The owner repair cleared the
      // tail-loaded flag, so this prefetch first needs the missing tail.
      act(() => requestSessionHistoryPage(SESSION_ID));
      await flush();
      h.expectTailCount(3);
      expect(h.tailRequests[2].stack).not.toContain("requestSessionTailRead");
      expect(h.historyRequests).toHaveLength(0);
      await act(async () => h.tailRequests[2].reject(new Error("Master prefetch unavailable")));
      expect(h.params.reportRequestError).toHaveBeenCalledTimes(2);

      if (demand !== "standing") {
        h.emit("delta", { ...textDelta(102, 1), sessionSeq: 1, bodySeqEpoch: INSTANCE });
        expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(false);
      }
      if (demand === "loaded") {
        // A complete ordinary history extension can finish loading without
        // clearing the pending hook timer or changing the retained tail.
        const resident = h.current();
        const prefix = Array.from({ length: TOTAL - resident.messages.length }, (_, index) =>
          message(`Loaded ${index}`, `loaded-${index}`));
        act(() => h.params.sessionAuthority.commit([{ ...resident,
          messages: [...prefix, ...resident.messages], messagesLoaded: true,
          messageStartIndex: 0, hasOlderHistory: false }], "history"));
        expect(h.current().messagesLoaded).toBe(true);
        expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(false);
      }
      await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0] - 1);
      h.expectTailCount(3);
      const gate = vi.spyOn(hydration, "shouldRequestSessionTailRead");
      await flush(1);
      expect(gate).toHaveBeenCalledTimes(1);
      h.expectTailCount(demand === "loaded" ? 3 : 4);
      if (demand !== "loaded") {
        // The first timer action must be master/optionless, not an owner read.
        expect(h.tailRequests[3].stack).not.toContain("requestSessionTailRead");
        expect(gate.mock.calls[0][0].inFlight).toBe(true);
      } else {
        expect(gate.mock.calls[0][0].inFlight).toBe(false);
      }
      expect(gate.mock.results[0].value).toBe(false);
      expect(h.historyRequests).toHaveLength(0);
      if (demand === "standing") {
        // The absorbed master retry did not charge a second attempt: after
        // this real restart fails, the next delay is still attempt two.
        await act(async () => h.tailRequests[3].reject(new Error("Master restart unavailable")));
        expect(h.params.reportRequestError).toHaveBeenCalledTimes(3);
        await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[1] - 1);
        h.expectTailCount(4);
        await flush(1);
        h.expectTailCount(5);
        expect(h.tailRequests[4].stack).not.toContain("requestSessionTailRead");
      }
    },
  );

  it("W5c: keeps base retry after five stale excluded forced repairs", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    expect(h.current().messagesLoaded).toBe(false);
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)).toBeNull();
    expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(false);

    act(() => h.startHydration(SESSION_ID, { forceTailRepair: true }));
    for (let attempt = 0; attempt < 5; attempt++) {
      const forcedIndex = 1 + attempt * 2;
      h.expectTailCount(forcedIndex + 1);
      expect(h.tailRequests[forcedIndex].stack).not.toContain("requestSessionTailRead");
      await act(async () => h.tailRequests[forcedIndex].resolve(tail(100, "Older")));
      await flush();
      expect(h.classify.mock.results[h.classify.mock.results.length - 1]?.value).toBe("stale");
      expect(h.current().messagesLoaded).toBe(false);
      const delay = SESSION_HYDRATION_RETRY_DELAYS_MS[Math.min(attempt, 3)];
      await flush(delay - 1);
      h.expectTailCount(forcedIndex + 1);
      await flush(1);
      // Stale master endings were uncapped on base, including the fifth.
      h.expectTailCount(forcedIndex + 2);
      if (attempt < 4) {
        // A new transport repair arrives during the optionless timer flight.
        // Its queued forced flight, rather than a test-created tail answer,
        // supplies the next stale forced ending without resetting the budget.
        act(() => h.startHydration(SESSION_ID, { forceTailRepair: true }));
        await act(async () => h.tailRequests[forcedIndex + 1].resolve(tail(100, "Older")));
        await flush();
        expect(h.classify.mock.results[h.classify.mock.results.length - 1]?.value).toBe("stale");
      }
    }
    expect(h.params.reportRequestError).not.toHaveBeenCalled();
  });

  it("W10: serves dirty demand at the next poll after a stale loaded master flight", async () => {
    const h = await pairedRetryFixture();
    const captured = tail(101, "a");
    captured.session = { ...captured.session, bodySeq: 0, bodySeqEpoch: INSTANCE };
    // Master's non-body recovery holds the sole hydration slot. Its answer
    // was captured before the final body change whose frame never arrives.
    act(() => h.startHydration(SESSION_ID, { forceTailRepair: true }));
    h.expectTailCount(2);
    expect(h.tailRequests[1].stack).not.toContain("requestSessionTailRead");
    const ahead = summary(102);
    ahead.sessions[0] = { ...ahead.sessions[0], sessionMutationStamp: 101,
      bodySeq: 1, bodySeqEpoch: INSTANCE, queuePaused: h.current().queuePaused === true };
    h.emit("state", ahead);
    await flush();
    h.emit("lagged", {});
    // Recovery's unchanged stamp leaves the fully loaded window in place.
    h.emit("state", ahead);
    await flush();
    expect(h.current().messagesLoaded).toBe(true);
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)).toBeNull();
    expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(true);
    h.expectTailCount(2);
    const retained = h.current().messages;
    await act(async () => h.tailRequests[1].resolve(captured));
    await flush();
    expect(h.current().messages).toBe(retained);
    // Q6's suppressed handoff is unchanged. The periodic backstop does not
    // depend on another stream event or on master having scheduled a timer.
    h.expectTailCount(2);
    await h.beforeNextBackstop();
    h.expectTailCount(2);
    await flush(1);
    h.expectTailCount(3);
    expect(h.tailRequests[2].watchdog).toBe(true);
    expect(h.tailRequests[2].stack).toContain("requestSessionTailRead");
    const newest = tail(102, "ax");
    newest.session = { ...newest.session, sessionMutationStamp: 101,
      bodySeq: 1, bodySeqEpoch: INSTANCE };
    await act(async () => h.tailRequests[2].resolve(newest));
    await flush(10000);
    h.expectTailCount(3);
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)?.appliedSeq).toBe(1);
    expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(false);
    for (const session of [h.current(), getSessionRecordSnapshotForTesting(SESSION_ID), h.renderedSession()]) {
      expect(session && session.messages[session.messages.length - 1]).toEqual(message("ax"));
    }
  });

  it("W7: retries a stale captured answer after a newer summary", async () => {
    const h = await pairedRetryFixture();
    const ahead = summary(102);
    ahead.sessions[0] = { ...ahead.sessions[0], bodySeq: 1, bodySeqEpoch: INSTANCE,
      queuePaused: h.current().queuePaused === true };
    h.emit("state", ahead);
    await flush();
    h.expectTailCount(2);
    expect(h.tailRequests[1].stack).toContain("requestSessionTailRead");
    const captured = tail(102, "ax");
    captured.session = { ...captured.session, bodySeq: 1, bodySeqEpoch: INSTANCE };
    const final = summary(103);
    final.sessions[0] = { ...final.sessions[0], bodySeq: 2, bodySeqEpoch: INSTANCE,
      queuePaused: h.current().queuePaused === true };
    h.emit("state", final);
    await flush();
    const retained = h.current().messages;
    await act(async () => h.tailRequests[1].resolve(captured));
    await flush();
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)?.appliedSeq,
      "the earlier answer cannot replay through the dropped final delta").toBe(0);
    expect(h.current().messages).toBe(retained);
    h.expectTailCount(2);
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0] - 1);
    h.expectTailCount(2);
    await flush(1);
    h.expectTailCount(3);
    const newest = tail(103, "axx");
    newest.session = { ...newest.session, bodySeq: 2, bodySeqEpoch: INSTANCE };
    await act(async () => h.tailRequests[2].resolve(newest));
    await flush();
    const resident = h.current().messages;
    const stored = getSessionRecordSnapshotForTesting(SESSION_ID)!.messages;
    expect(resident[resident.length - 1]).toEqual(message("axx"));
    expect(stored[stored.length - 1]).toEqual(message("axx"));
  });

  it("W8: serves standing demand on the next poll after fast-cap exhaustion", async () => {
    const h = await pairedRetryFixture();
    await startRejectedOwnerRead(h);
    for (const [index, delay] of SESSION_HYDRATION_RETRY_DELAYS_MS.entries()) {
      await flush(delay);
      h.expectTailCount(index + 3);
      await act(async () => h.tailRequests[index + 2].reject(new Error("Still unavailable")));
    }
    expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(true);
    await h.beforeNextBackstop();
    h.expectTailCount(6);
    await flush(1);
    h.expectTailCount(7);
    expect(h.tailRequests[6].watchdog).toBe(true);
  });

  it("W9: requests one recovery without an immediate tail read after owner 404", async () => {
    const h = await pairedRetryFixture();
    const stateReadsBeforeRecovery = vi.mocked(api.fetchState).mock.calls.length;
    const ahead = summary(102);
    ahead.sessions[0] = { ...ahead.sessions[0], bodySeq: 1, bodySeqEpoch: INSTANCE,
      queuePaused: h.current().queuePaused === true };
    h.emit("state", ahead);
    await flush();
    h.expectTailCount(2);
    await act(async () => h.tailRequests[1].reject(
      new ApiRequestError("request-failed", "Not found", { status: 404 })));
    await flush();
    expect(api.fetchState).toHaveBeenCalledTimes(stateReadsBeforeRecovery + 1);
    expect(h.params.reportRequestError).not.toHaveBeenCalled();
    h.expectTailCount(2);
  });

  it.each(["unchanged", "failed", "pending"] as const)(
    "W11: idle demand survives %s recovery until the next poll", async (recovery) => {
      const h = await pairedRetryFixture();
      const recoveryResponse = deferred<api.StateResponse>();
      vi.mocked(api.fetchState).mockImplementation(() => recoveryResponse.promise);
      const ahead = summary(102);
      ahead.sessions[0] = { ...ahead.sessions[0], status: "idle", bodySeq: 1,
        bodySeqEpoch: INSTANCE, queuePaused: h.current().queuePaused === true };
      h.emit("state", ahead);
      await flush();
      h.expectTailCount(2);
      await act(async () => h.tailRequests[1].reject(
        new ApiRequestError("request-failed", "Not found", { status: 404 })));
      const beforeRecovery = h.current();
      if (recovery === "unchanged") await act(async () => recoveryResponse.resolve(ahead));
      if (recovery === "failed") await act(async () => recoveryResponse.reject(new Error("Recovery unavailable")));
      await flush();
      if (recovery === "unchanged") expect(h.current(), "same-revision recovery is not adopted").toBe(beforeRecovery);
      expect(h.current().status).toBe("idle");
      expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(true);
      h.expectTailCount(2);
      await h.beforeNextBackstop();
      h.expectTailCount(2);
      await flush(1);
      h.expectTailCount(3);
      expect(h.tailRequests[2].watchdog).toBe(true);
      const newest = tail(102, "ax");
      newest.session = { ...newest.session, status: "idle", bodySeq: 1, bodySeqEpoch: INSTANCE };
      await act(async () => h.tailRequests[2].resolve(newest));
      await flush(6000);
      h.expectTailCount(3);
      expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(false);
    },
  );

  it.each(["document-hidden", "navigator-offline"] as const)(
    "W15: serves visible pane demand despite %s", async (policy) => {
      const h = await pairedRetryFixture();
      await startRejectedOwnerRead(h);
      if (policy === "document-hidden") vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
      else vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
      // Settle fast retries by rejecting them; no stream event is needed at
      // cap exhaustion. Browser ticks continue despite the policy flag.
      for (const [index, delay] of SESSION_HYDRATION_RETRY_DELAYS_MS.entries()) {
        await flush(delay);
        await act(async () => h.tailRequests[index + 2].reject(new Error("Still unavailable")));
      }
      await h.beforeNextBackstop();
      h.expectTailCount(6);
      await flush(1);
      h.expectTailCount(7);
      expect(h.tailRequests[6].watchdog).toBe(true);
    },
  );

  it("W13: separates periodic issuance from fast retries across repeated failures", async () => {
    const h = await pairedRetryFixture();
    await startRejectedOwnerRead(h);
    for (const [index, delay] of SESSION_HYDRATION_RETRY_DELAYS_MS.entries()) {
      await flush(delay);
      await act(async () => h.tailRequests[index + 2].reject(new Error("Still unavailable")));
    }
    expect(h.tailRequests.filter(request => request.watchdog)).toHaveLength(0);
    for (let period = 0; period < 3; period++) {
      await h.beforeNextBackstop();
      h.expectTailCount(6 + period);
      await flush(1);
      h.expectTailCount(7 + period);
      const request = h.tailRequests[6 + period];
      expect(request.watchdog).toBe(true);
      await act(async () => request.reject(new Error("Persistent endpoint failure")));
    }
    const polls = h.tailRequests.filter(request => request.watchdog);
    expect(polls).toHaveLength(3);
    expect(polls.map(request => request.at - polls[0].at)).toEqual([0, 3000, 6000]);
    expect(h.watchdogFirings()).toBe(12);
  });

  it("W14: admission checks an early owner read after a joined page requests recovery", async () => {
    const h = await pairedRetryFixture(false);
    let completion!: Promise<boolean>;
    act(() => { completion = requestSessionHistoryOlderPage(SESSION_ID); });
    act(() => {
      requestSessionHistoryPage(SESSION_ID);
      Stream.latest.emit("delta", { ...textDelta(102, 1), sessionSeq: 2, bodySeqEpoch: INSTANCE });
    });
    await flush();
    expect(h.historyRequests).toHaveLength(1);
    h.expectTailCount(1);
    const stateReads = vi.mocked(api.fetchState).mock.calls.length;
    await act(async () => h.historyRequests[0].reject(
      new ApiRequestError("request-failed", "Not found", { status: 404 })));
    await flush();
    expect(await completion).toBe(false);
    expect(api.fetchState).toHaveBeenCalledTimes(stateReads + 1);
    // The joining hydration did not own the page's recovery callback. Its
    // normal slot release may initiate before that resync settles (G2).
    h.expectTailCount(2);
    expect(h.tailRequests[1].stack).toContain("requestSessionTailRead");
    const retained = h.current().messages;
    const stale = tail(101, "a");
    stale.session = { ...stale.session, bodySeq: 0, bodySeqEpoch: INSTANCE };
    await act(async () => h.tailRequests[1].resolve(stale));
    await flush();
    expect(h.current().messages).toBe(retained);
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)).toBeNull();
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0]);
    h.expectTailCount(3);
    const newest = tail(102, "ax");
    newest.session = { ...newest.session, bodySeq: 2, bodySeqEpoch: INSTANCE };
    await act(async () => h.tailRequests[2].resolve(newest));
    await flush();
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)?.appliedSeq).toBe(2);
    expect(h.current().messages[h.current().messages.length - 1]).toEqual(message("ax"));
  });

  it.each(["closed", "hidden", "detached"] as const)(
    "W12: does not issue retry or polling GET after demand becomes %s", async (cause) => {
      const h = await pairedRetryFixture();
      await startRejectedOwnerRead(h);
      if (cause === "closed") {
        h.emit("delta", { ...textDelta(102, 1), sessionSeq: 1, bodySeqEpoch: INSTANCE });
        expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)?.appliedSeq).toBe(1);
      } else if (cause === "hidden") {
        await h.visible(false);
      } else {
        // Commit the detaching window through the actual owner publication gate.
        act(() => h.params.sessionAuthority.commit([{ ...h.current(), hasNewerHistory: true }], "history"));
      }
      expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, cause !== "hidden")).toBe(false);
      await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0]);
      h.expectTailCount(2);
      await flush(10000);
      h.expectTailCount(2);
      expect(h.historyRequests).toHaveLength(0);
    },
  );

  it("issues exactly one timer GET while owner demand stands", async () => {
    const h = await pairedRetryFixture();
    await startRejectedOwnerRead(h);
    expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(true);
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0] - 1);
    h.expectTailCount(2);
    await flush(1);
    h.expectTailCount(3);
    const response = tail(102, "ax");
    response.session = { ...response.session, bodySeq: 1, bodySeqEpoch: INSTANCE };
    await act(async () => h.tailRequests[2].resolve(response));
    await flush(1000);
    h.expectTailCount(3);
    const resident = h.current().messages;
    const stored = getSessionRecordSnapshotForTesting(SESSION_ID)!.messages;
    expect(resident[resident.length - 1]).toEqual(message("ax"));
    expect(stored[stored.length - 1]).toEqual(message("ax"));
  });

  it.each([true, false])("preserves master excluded forced-repair retry (loaded %s)", async (loaded) => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    if (loaded) act(() => h.params.sessionAuthority.commit([{ ...h.current(),
      messagesLoaded: true, messageCount: 20, messageStartIndex: 0, hasOlderHistory: false }], "history"));
    expect(h.params.sessionAuthority.bodyCertificate(SESSION_ID)).toBeNull();
    // Invoke the real hook callback supplied to master's transport recovery.
    act(() => h.startHydration(SESSION_ID, { forceTailRepair: true }));
    h.expectTailCount(2);
    await act(async () => h.tailRequests[1].reject(new Error("Master repair unavailable")));
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0]);
    h.expectTailCount(loaded ? 2 : 3);
    expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(false);
  });

  it.each(["before", "during"] as const)(
    "keeps older prefetch as a page when a hole arrives %s the flight", async (order) => {
      const h = await pairedRetryFixture(false);
      const hole = { ...textDelta(102, 1), sessionSeq: 2, bodySeqEpoch: INSTANCE };
      act(() => {
        if (order === "before") Stream.latest.emit("delta", hole);
        requestSessionHistoryPage(SESSION_ID);
        if (order === "during") Stream.latest.emit("delta", hole);
      });
      await flush();
      expect(h.historyRequests).toHaveLength(1);
      h.expectTailCount(1);
      await act(async () => h.historyRequests[0].resolve({
        revision: 102, serverInstanceId: INSTANCE, sessionMutationStamp: 102,
        messageCount: TOTAL, messageStartIndex: TOTAL - 21,
        messages: [message("Prefetched", "prefetched")], hasMore: true,
        nextBefore: "prefetched", hasNewer: false, nextAfter: null,
      }));
      await flush();
      // No manual rescue: publication/flight completion must serve the demand.
      h.expectTailCount(2);
      expect(h.current().messages[0].id).toBe("prefetched");
      expect(h.params.sessionAuthority.needsTailRead(SESSION_ID, true)).toBe(true);
    },
  );
});

function coverageCase(warm = false) {
  const response = tail(101, "a");
  response.session.messageStartIndex = TOTAL - 20;
  const baseline: Session = { ...response.session,
    messages: warm ? response.session.messages : [message("a")],
    messageStartIndex: warm ? TOTAL - 20 : TOTAL - 1,
    hasOlderHistory: true, hasNewerHistory: false };
  const delta = textDelta(102, 1) as Extract<Parameters<typeof deltas.applyDeltaToSessions>[1], { type: "textDelta" }>;
  const result = deltas.applyDeltaToSessions([baseline], delta);
  if (result.kind !== "applied") throw new Error("Fixture must exercise a real append");
  const current = result.sessions[0];
  const proof: adoption.PartialTailAppendProof = { baseline, latest: baseline, valid: true };
  adoption.advancePartialTailAppendProof(proof, { delta, revisionAction: "apply",
    resultKind: result.kind, previousSession: baseline, nextSession: current, targetPresent: true });
  expect(proof.valid).toBe(true);
  const request: adoption.SessionHydrationRequestContext = { kind: "partialTail",
    messageCount: TOTAL, revision: 101, serverInstanceId: INSTANCE,
    sessionMutationStamp: 101, partialTailAppendProof: proof };
  const classify = (overrides: Partial<Parameters<typeof adoption.classifyFetchedSessionAdoption>[0]> = {}) =>
    adoption.classifyFetchedSessionAdoption({ responseSession: response.session, responseRevision: response.revision,
      responseServerInstanceId: INSTANCE, requestContext: request, currentSession: current,
      currentRevision: 102, currentServerInstanceId: INSTANCE, seenServerInstanceIds: new Set([INSTANCE]),
      ...overrides });
  return { response, baseline, current, proof, request, classify };
}

describe("partial coverage fences", () => {
  it("adds older coverage and retains current metadata and message identity", () => {
    const c = coverageCase();
    c.current.preview = "Current preview";
    c.current.pendingPrompts = [];
    expect(c.classify()).toBe("partialCoverage");
    const merged = adoption.mergeAppendOnlyPartialTailCoverage(c.response.session, c.current, c.request)!;
    expect(merged.messages).toHaveLength(20);
    expect(merged.messages[19]).toBe(c.current.messages[0]);
    expect(merged.preview).toBe("Current preview");
    expect(merged.pendingPrompts).toBe(c.current.pendingPrompts);
    expect(merged.sessionMutationStamp).toBe(102);
  });

  it("keeps the exact resident session when the fetched window adds no coverage", () => {
    const c = coverageCase(true);
    expect(c.classify()).toBe("partialCoverage");
    expect(adoption.mergeAppendOnlyPartialTailCoverage(c.response.session, c.current, c.request)).toBe(c.current);
  });

  it.each(["duplicate-id", "wrong-id", "wrong-index", "divergent-text", "older-baseline-text",
    "future-text", "older-count", "ahead-count", "older-stamp", "ahead-stamp", "historical-window",
    "authority-owner", "invalid-proof"])("rejects %s without mutating residency", (reason) => {
    const c = coverageCase();
    const response = c.response.session;
    if (reason === "duplicate-id") response.messages[0] = response.messages[19];
    if (reason === "wrong-id") response.messages[19] = message("a", "wrong");
    if (reason === "wrong-index") response.messageStartIndex! -= 1;
    if (reason === "divergent-text") response.messages[19] = message("z");
    if (reason === "older-baseline-text") response.messages[19] = message("");
    if (reason === "future-text") response.messages[19] = message("axfuture");
    if (reason === "older-count") response.messageCount! -= 1;
    if (reason === "ahead-count") response.messageCount! += 1;
    if (reason === "older-stamp") response.sessionMutationStamp = 100;
    if (reason === "ahead-stamp") response.sessionMutationStamp = 103;
    const current = reason === "historical-window" ? { ...c.current, hasNewerHistory: true }
      : reason === "authority-owner" ? { ...c.current, remoteId: "other-authority" } : c.current;
    if (reason === "invalid-proof") c.proof.valid = false;
    const messages = current.messages;
    expect(c.classify({ currentSession: current })).toBe("stale");
    expect(current.messages).toBe(messages);
    expect(adoption.mergeAppendOnlyPartialTailCoverage(response, current, c.request)).toBeNull();
  });

  it("retains restart, superseded-instance and ahead-revision fences", () => {
    const c = coverageCase();
    expect(c.classify({ responseServerInstanceId: "new-instance" })).toBe("restartResync");
    expect(c.classify({ currentServerInstanceId: "new-instance" })).toBe("stale");
    expect(c.classify({ responseRevision: 100 })).toBe("stale");
    expect(c.classify({ responseRevision: 103 })).not.toBe("partialCoverage");
    expect(c.classify({ currentSession: null })).toBe("stale");
    expect(c.classify({ requestContext: { ...c.request, kind: "textRepair" } })).not.toBe("partialCoverage");
  });
});

async function runComposite({
  latencyMs,
  unseenSummaries,
  hideAt,
}: {
  latencyMs: number;
  unseenSummaries: boolean | "first";
  hideAt?: number;
}) {
  const h = setup();
  await h.visible(true);
  await h.resolveTail(0, 101, "a");
  const startedAt = Date.now();
  let revision = 101;
  let stamp = 101;
  let text = "a";
  let activeRequests = 0;
  let maximumActiveRequests = 0;
  let deltaCount = 0;
  let summaryCount = 0;
  const completions: number[] = [];
  const originalFetch = h.fetchTail.getMockImplementation()!;
  h.fetchTail.mockImplementation((...args) => {
    const result = originalFetch(...args);
    const request = h.tailRequests[h.tailRequests.length - 1];
    // An internally coherent server snapshot, captured when GET starts and
    // delivered later. Live deltas can advance its own session in the meantime.
    const response = tail(revision, text);
    response.session.sessionMutationStamp = stamp;
    activeRequests += 1;
    maximumActiveRequests = Math.max(maximumActiveRequests, activeRequests);
    window.setTimeout(() => {
      activeRequests -= 1;
      completions.push(Date.now() - startedAt);
      request.resolve(response);
    }, latencyMs);
    return result;
  });

  // 84 coherent text deltas (one every 30ms), six state events, fixed count.
  // Advance/flush one millisecond at a time so React commits/effects can run
  // between network callbacks, rather than batching the entire 2.559s trace.
  for (let elapsed = 0; elapsed <= COMPOSITE_WINDOW_MS; elapsed += 1) {
    if (elapsed === hideAt) h.rerender({ visible: false });
    if (elapsed > 0 && elapsed <= 2520 && elapsed % 30 === 0) {
      revision += 1;
      stamp = revision;
      h.emit("delta", textDelta(revision, text.length));
      text += "x";
      deltaCount += 1;
    }
    if (elapsed % 426 === 0 && summaryCount < 6) {
      revision += 1;
      // Unseen authority is a separate server mutation, not a duplicate of
      // the already-applied text delta. It need not change this text/count.
      if (unseenSummaries === true || (unseenSummaries === "first" && summaryCount === 0)) {
        stamp = revision;
      }
      const state = summary(revision);
      state.sessions[0].sessionMutationStamp = stamp;
      h.emit("state", state);
      summaryCount += 1;
    }
    if (elapsed < COMPOSITE_WINDOW_MS) await flush(1);
  }

  expect(deltaCount).toBe(84);
  expect(summaryCount).toBe(6);
  expect(h.current().messageCount).toBe(TOTAL);
  expect(h.current().messages[h.current().messages.length - 1]?.type).toBe("text");
  expect((h.current().messages[h.current().messages.length - 1] as TextMessage).text).toBe(text);
  expect(h.reducer.mock.results.every((entry) => entry.value.kind === "applied")).toBe(true);
  expect(h.recoveryCalls).toEqual([]);
  expect(h.historyRequests).toHaveLength(0);
  expect(maximumActiveRequests).toBeLessThanOrEqual(1);
  h.expectTailCount(h.tailRequests.length);
  const launches = h.tailRequests.slice(1).map((request) => request.at - startedAt);
  const outcomes = h.classify.mock.results.slice(1).map((entry) => entry.value);
  // The warm-up tail is deliberately excluded from measured counts/rates.
  const effectLaunches = h.tailRequests.slice(1)
    .filter((request) => request.stack.includes("commitHookEffectListMount"))
    .map((request) => request.at - startedAt);
  const timerRequests = h.tailRequests.slice(1)
    .filter((request) => !request.stack.includes("commitHookEffectListMount"));
  // Distinguish React effect entry from fake-clock callback entry without
  // hard-coding product line numbers into executable assertions.
  expect(timerRequests.every((request) => request.stack.includes("callTimer"))).toBe(true);
  const timerLaunches = timerRequests.map((request) => request.at - startedAt);
  return { h, launches, completions, outcomes, maximumActiveRequests,
    effectLaunches, timerLaunches };
}

describe("streaming tail-repair hypothesis schedules", () => {
  it("keeps the visible window on a gapped append and fills the gap through bounded repair", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    expect(h.current().messages).toHaveLength(20);

    const coherentMessage = message("Coherent append", "coherent-append");
    h.emit("delta", {
      type: "messageCreated", revision: 102, sessionId: SESSION_ID,
      messageId: coherentMessage.id, messageIndex: TOTAL, messageCount: TOTAL + 1,
      sessionMutationStamp: 102, message: coherentMessage, preview: "", status: "active",
    });
    await flush(16);
    expect(h.current().messages).toHaveLength(21);
    h.expectTailCount(1);
    expect(h.recoveryCalls).toEqual([]);

    const visibleMessages = h.current().messages;
    const newest = message("After two missing messages", "gapped-append");
    h.emit("delta", {
      type: "messageCreated", revision: 103, sessionId: SESSION_ID,
      messageId: newest.id, messageIndex: TOTAL + 3, messageCount: TOTAL + 4,
      sessionMutationStamp: 103, message: newest, preview: newest.text, status: "active",
    });
    expect(h.current().messages).toBe(visibleMessages);
    expect(h.current().messageStartIndex).toBe(TOTAL - 20);
    expect(h.current().hasNewerHistory).toBe(false);
    expect(h.reducer.mock.results[h.reducer.mock.results.length - 1].value.kind)
      .toBe("appliedNeedsResync");
    await flush(16);
    expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages)
      .toEqual(visibleMessages);
    h.expectTailCount(2);

    const repairedMessages = [
      ...visibleMessages,
      message("Missing one", "gap-one"),
      message("Missing two", "gap-two"),
      newest,
    ];
    // The real endpoint returns only twenty tail messages. Reconciliation
    // must keep the overlapping resident head, not replace it with that tail.
    await act(async () => h.tailRequests[1].resolve({
      revision: 103, serverInstanceId: INSTANCE,
      session: { ...h.current(), messages: repairedMessages.slice(-20) },
    }));
    await flush(1000);
    expect(h.current().messages).toEqual(repairedMessages);
    expect(h.current().messageStartIndex).toBe(TOTAL - 20);
    expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages)
      .toEqual(repairedMessages);
    h.expectTailCount(2);
    expect(h.historyRequests).toHaveLength(0);
  });

  it("preserves visible history while a gap stays unresolved and coalesces further appends", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    const visibleMessages = h.current().messages;
    expect(visibleMessages).toHaveLength(20);

    for (let index = 0; index < 8; index += 1) {
      const newest = message(`New ${index}`, `gapped-${index}`);
      h.emit("delta", {
        type: "messageCreated", revision: 102 + index, sessionId: SESSION_ID,
        messageId: newest.id, messageIndex: TOTAL + 2 + index,
        messageCount: TOTAL + 3 + index, sessionMutationStamp: 102 + index,
        message: newest, preview: newest.text, status: "active",
      });
      await flush(16);
      expect(h.current().messages).toBe(visibleMessages);
      expect(h.current().messageStartIndex).toBe(TOTAL - 20);
      expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages)
        .toEqual(visibleMessages);
      // One warm-up and one pending repair. Further events can queue a
      // coalesced follow-up, never launch one HTTP request per message.
      h.expectTailCount(2);
    }
    // Deliberately never resolve the repair: preserving the old cards is
    // required even when no successful response arrives to hide the defect.
    await flush(1000);
    expect(h.current().messages).toBe(visibleMessages);
    expect(h.current().hasNewerHistory).toBe(false);
    h.expectTailCount(2);
    expect(h.historyRequests).toHaveLength(0);
  });

  it.each(["absent", "throws"] as const)("keeps the observation fence when the proof predicate %s", async (mode) => {
    const h = setup("normal", mode);
    const counts = countObservationScans();
    h.emit("delta", textDelta(102, 1));
    expect(counts).toEqual({ sessionScans: 2, messageScans: 1 });
    await h.visible(true);
    h.emit("delta", textDelta(103, 2));
    expect(counts).toEqual({ sessionScans: 4, messageScans: 2 });
    await h.resolveTail(0, 102, "ax");
    expect(h.classify.mock.results[0].value).toBe("partialCoverage");
    expect(h.current().messages).toHaveLength(20);
    expect(h.current().messages[19]).toEqual(message("axx"));
  });

  it.each([false, true])("scopes observation admission by session without losing cross-session gap invalidation (gap %s)", async (gap) => {
    const h = setup();
    const state = summary(101);
    state.sessions.push({ ...state.sessions[0], id: "other-session", messageCount: 0 });
    // Install the second session explicitly: ordinary SSE correctly ignores
    // an equal-revision snapshot and would leave its following delta unknown.
    act(() => { expect(h.result.current.adoptState(state, { force: true })).toBe(true); });
    expect(h.params.adoptionRefs.sessionsRef.current.some((entry) => entry.id === "other-session")).toBe(true);
    await h.visible(true);
    const counts = countObservationScans();
    const revision = gap ? 103 : 102;
    h.emit("delta", {
      type: "messageCreated", revision, sessionId: "other-session", messageId: "other-message",
      messageIndex: 0, messageCount: 1, sessionMutationStamp: revision,
      message: message("other", "other-message"), preview: "", status: "active",
    });
    if (!gap) expect(counts).toEqual({ sessionScans: 0, messageScans: 0 });
    h.emit("delta", textDelta(revision + 1, 1));
    await h.resolveTail(0, 101, "a");
    expect(h.classify.mock.results[0].value).toBe(gap ? "stale" : "partialCoverage");
    expect(h.current().messages).toHaveLength(gap ? 1 : 20);
    expect(h.current().messages[h.current().messages.length - 1]).toEqual(message("ax"));
  });

  it("does no observation scans per delta without an in-flight tail proof", async () => {
    const h = setup();
    const counts = countObservationScans();
    for (let index = 0; index < 4; index += 1) {
      h.emit("delta", textDelta(102 + index, 1 + index));
      expect(counts).toEqual({ sessionScans: 0, messageScans: 0 });
    }
    expect(h.current().messages).toEqual([message("axxxx")]);
    h.expectTailCount(0);
  });

  it("constructs observations while a proof is tracked and stops after coverage settles", async () => {
    const h = setup();
    await h.visible(true);
    const counts = countObservationScans();
    for (let index = 0; index < 4; index += 1) {
      h.emit("delta", textDelta(102 + index, 1 + index));
      expect(counts).toEqual({ sessionScans: 2 * (index + 1), messageScans: index + 1 });
    }
    await h.resolveTail(0, 101, "a");
    expect(h.classify.mock.results[0].value).toBe("partialCoverage");
    counts.sessionScans = 0;
    counts.messageScans = 0;
    for (let index = 0; index < 4; index += 1) {
      h.emit("delta", textDelta(106 + index, 5 + index));
      expect(counts).toEqual({ sessionScans: 0, messageScans: 0 });
    }
    expect(h.current().messages).toHaveLength(20);
    expect(h.current().messages[19]).toEqual(message("axxxxxxxx"));
    h.expectTailCount(1);
  });

  it("does not bypass the retry deadline through a delta render after declining gapped coverage", async () => {
    const h = setup();
    await h.visible(true);
    const response = tail(100, "older");
    response.session.messageCount = TOTAL - 4;
    response.session.messages[19] = message("older", "older-last");
    await act(async () => h.tailRequests[0].resolve(response));
    expect(h.classify.mock.results[0].value).toBe("stale");
    h.emit("delta", textDelta(102, 1));
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0] - 1);
    h.expectTailCount(1);
    await flush(1);
    h.expectTailCount(2);
    h.emit("delta", textDelta(103, 2));
    await h.resolveTail(1, 102, "ax");
    expect(h.classify.mock.results[1].value).toBe("partialCoverage");
    expect(h.current().messages).toHaveLength(20);
    expect(h.current().messages[h.current().messages.length - 1]).toEqual(message("axx"));
    await flush(1000);
    h.expectTailCount(2);
    expect(h.historyRequests).toHaveLength(0);
  });

  it.each(["partial", "partialCoverage"] as const)(
    "cancels a pending retry when forced tail repair supplies %s before the deadline", async (outcome) => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 100, "older");
    expect(h.classify.mock.results[0].value).toBe("stale");
    h.emit("delta", { ...textDelta(102, 1), type: "textReplace", messageId: "missing", text: "fixed" });
    await flush();
    h.expectTailCount(2);
    expect(h.recoveryCalls[h.recoveryCalls.length - 1]?.[1]?.forceTailRepair).toBe(true);
    if (outcome === "partialCoverage") h.emit("delta", textDelta(103, 1));
    await h.resolveTail(1, 102, "a");
    expect(h.classify.mock.results[1].value).toBe(outcome);
    await flush(1000);
    h.expectTailCount(2);
    expect(h.historyRequests).toHaveLength(0);
  });

  it("keeps the second retry deadline across repeated passive delta renders", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 100, "older");
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0]);
    h.expectTailCount(2);
    await h.resolveTail(1, 100, "older");
    const declinedAt = Date.now();
    for (let index = 0; index < 4; index += 1) {
      h.emit("delta", textDelta(102 + index, 1 + index));
      await flush(20);
      h.expectTailCount(2);
    }
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[1] - (Date.now() - declinedAt) - 1);
    h.expectTailCount(2);
    await flush(1);
    h.expectTailCount(3);
    expect(h.tailRequests[2].at - declinedAt).toBe(SESSION_HYDRATION_RETRY_DELAYS_MS[1]);
    await h.resolveTail(2, 105, "axxxx");
    expect(h.classify.mock.results.map((entry) => entry.value)).toEqual(["stale", "stale", "partial"]);
    await flush(1000);
    h.expectTailCount(3);
    expect(h.historyRequests).toHaveLength(0);
  });

  it("admits explicit history demand before a pending retry deadline", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 100, "older");
    expect(h.classify.mock.results[0].value).toBe("stale");
    act(() => { void requestSessionHistoryStartPage(SESSION_ID); });
    await flush();
    expect(h.historyRequests).toHaveLength(1);
    h.expectTailCount(1);
  });

  it("admits restart recovery before the previous instance's retry deadline", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 100, "older");
    expect(h.classify.mock.results[0].value).toBe("stale");
    act(() => {
      expect(h.result.current.adoptState({ ...summary(1), serverInstanceId: "restarted" },
        { allowUnknownServerInstance: true })).toBe(true);
    });
    await flush();
    h.expectTailCount(2);
    const response = { ...tail(1, "new"), serverInstanceId: "restarted" };
    await act(async () => h.tailRequests[1].resolve(response));
    expect(h.classify.mock.results[1].value).toBe("partial");
    await flush(1000);
    h.expectTailCount(2);
    expect(h.historyRequests).toHaveLength(0);
  });

  it.each(["absent", "throws"] as const)("keeps transport delivery inert with an %s append observer", async (mode) => {
    const h = setup(mode);
    await h.visible(true);
    h.emit("delta", textDelta(102, 1));
    await flush(16);
    expect(h.current().messages).toEqual([message("ax")]);
    expect(h.current().sessionMutationStamp).toBe(102);
    expect(h.params.adoptionRefs.latestStateRevisionRef.current).toBe(102);
    expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages).toEqual([message("ax")]);
    expect(h.recoveryCalls).toEqual([]);
    await h.resolveTail(0, 101, "a");
    expect(h.classify.mock.results[0].value).toBe("stale");
    expect(h.current().messages).toEqual([message("ax")]);
  });

  it("retains forty new cards during one round trip without outrunning tail coverage", async () => {
    const h = setup();
    await h.visible(true);
    for (let index = 0; index < 40; index += 1) {
      const id = `burst-${index}`;
      h.emit("delta", { type: "messageCreated", revision: 102 + index,
        sessionId: SESSION_ID, messageId: id, messageIndex: TOTAL + index,
        messageCount: TOTAL + index + 1, sessionMutationStamp: 102 + index,
        message: message(`Burst ${index}`, id), preview: "", status: "active" });
      await flush(1);
    }
    const before = h.current();
    expect(before.messages).toHaveLength(41);
    await h.resolveTail(0, 101, "a");
    expect(h.classify.mock.results[0].value).toBe("partialCoverage");
    expect(h.current().messages).toHaveLength(60);
    expect(h.current().messages.slice(19)).toEqual(before.messages);
    before.messages.forEach((record, index) => expect(h.current().messages[index + 19]).toBe(record));
    expect(h.current()).toMatchObject({ messageStartIndex: 1854, messageCount: TOTAL + 40,
      sessionMutationStamp: 141 });
    await flush(250);
    h.expectTailCount(1);
  });

  it.each(["replacement", "unknown-summary", "history-demand", "lagged", "malformed", "gap", "missing-target"])(
    "does not rebase delayed coverage after %s", async (reason) => {
      const h = setup();
      await h.visible(true);
      if (reason === "replacement") {
        h.emit("delta", { ...textDelta(102, 1), type: "textReplace", text: "ax" });
        h.emit("delta", textDelta(103, 2));
      } else if (reason === "unknown-summary") {
        h.emit("state", summary(102));
        h.emit("delta", textDelta(103, 1));
      } else if (reason === "gap") {
        h.emit("delta", textDelta(103, 1));
      } else if (reason === "missing-target") {
        h.emit("delta", { ...textDelta(102, 1), messageId: "missing" });
      } else {
        h.emit("delta", textDelta(102, 1));
        if (reason === "history-demand") act(() => { void requestSessionHistoryStartPage(SESSION_ID); });
        else if (reason === "malformed") h.emit("delta", null);
        else h.emit("lagged", {});
      }
      const before = h.current();
      await h.resolveTail(0, 101, "a");
      expect(h.classify.mock.results[0].value).toBe("stale");
      expect(h.current()).toBe(before);
    },
  );

  it.each([0, 3])("fences an older non-overlapping tail instead of dropping the newest card (gap %i)", async (gap) => {
    const h = setup();
    await h.visible(true);
    const before = h.current();
    const response = tail(100, "older");
    response.session.messageCount = TOTAL - 1 - gap;
    response.session.messages[19] = message("older", "older-last");
    await act(async () => h.tailRequests[0].resolve(response));
    await flush();
    expect(h.classify.mock.results[0].value).toBe("stale");
    expect(h.current()).toBe(before);
    expect(h.current().messages).toEqual([message("a")]);
  });

  it("keeps a delta-grown window larger than twenty when a bounded tail adopts", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    for (let index = 0; index < 30; index += 1) {
      const id = `appended-${index}`;
      h.emit("delta", { type: "messageCreated", revision: 102 + index,
        sessionId: SESSION_ID, messageId: id, messageIndex: TOTAL + index,
        messageCount: TOTAL + index + 1, sessionMutationStamp: 102 + index,
        message: message(`Appended ${index}`, id), preview: "", status: "active" });
      await flush(30);
    }
    expect(h.current().messages).toHaveLength(50);
    h.emit("state", summary(132, TOTAL + 30));
    await flush(16);
    h.expectTailCount(2);
    const before = h.current();
    await act(async () => h.tailRequests[1].resolve({ revision: 132,
      serverInstanceId: INSTANCE, session: { ...before,
        messages: before.messages.slice(-20), messagesLoaded: false } }));
    await flush();
    expect(h.classify.mock.results[1].value).toBe("partial");
    expect(h.current().messages).toEqual(before.messages);
    expect(h.current().messages).toBe(before.messages);
    expect(h.current().messageStartIndex).toBe(before.messageStartIndex);
    expect(h.current().messageCount).toBe(before.messageCount);
    expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages).toEqual(before.messages);
  });

  it("fills a sparse transcript from a delayed tail while stamp-advancing output continues", async () => {
    const h = setup();
    await h.visible(true);
    for (let index = 0; index < 4; index += 1) {
      h.emit("delta", textDelta(102 + index, 1 + index));
      await flush(30);
    }
    await h.resolveTail(0, 101, "a");
    // Liveness AND safety: cover the missing nineteen records before the
    // stream goes quiet, without rolling back text or authority metadata.
    expect(h.current().messages).toHaveLength(20);
    expect(h.current().messages[0].id).toBe("message-680851");
    expect(h.current().messages[19]).toEqual(message("axxxx"));
    expect(h.current()).toMatchObject({ messageStartIndex: 1854,
      messageCount: TOTAL, sessionMutationStamp: 105 });
    expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages).toHaveLength(20);
    for (let index = 0; index < 8; index += 1) {
      h.emit("delta", textDelta(106 + index, 5 + index));
      await flush(30);
      expect(h.current().messages).toHaveLength(20);
    }
    h.expectTailCount(1);
    expect(h.current().messages[19]).toEqual(message("a" + "x".repeat(12)));
  });

  it.each([50, 80, 90, 100, 120, 200])(
    "composite: six unknown-summary repairs settle despite streaming, latency %i ms",
    async (latencyMs) => {
      const result = await runComposite({ latencyMs, unseenSummaries: true });
      expect(result.launches).toHaveLength(6);
      expect(result.completions).toHaveLength(6);
      expect(result.outcomes).toEqual(Array(6).fill("partialCoverage"));
      expect(result.maximumActiveRequests).toBe(1);
      expect(result.timerLaunches).toEqual([]);
      expect(result.effectLaunches).toHaveLength(6);
      expect(result.completions).toEqual(result.launches.map((at) => at + latencyMs));
      expect(result.h.current().messages).toHaveLength(20);
    },
  );

  it("composite: one unseen summary repairs once, with no delta-render reentry", async () => {
    const result = await runComposite({ latencyMs: 90, unseenSummaries: "first" });
    expect(result.launches).toHaveLength(1);
    expect(result.completions).toHaveLength(1);
    expect(result.outcomes).toEqual(["partialCoverage"]);
    expect(result.effectLaunches).toHaveLength(1);
    expect(result.timerLaunches).toEqual([]);
  });

  it("composite control: six matching summaries do not disturb an authoritative tail", async () => {
    const result = await runComposite({ latencyMs: 100, unseenSummaries: false });
    expect(result.launches).toEqual([]);
  });

  it("composite visibility: hiding halfway leaves no retry after coverage settles", async () => {
    const result = await runComposite({ latencyMs: 100, unseenSummaries: true, hideAt: 1280 });
    expect(result.launches).toHaveLength(4);
    expect(result.completions).toHaveLength(4);
    expect(result.launches.filter((at) => at >= 1280)).toEqual([]);
    expect(result.effectLaunches.every((at) => at < 1280)).toBe(true);
    expect(result.timerLaunches).toEqual([]);
  });

  it("authority control: global revision advances alone permit a matching older-revision tail", async () => {
    const h = setup();
    await h.visible(true);
    const state = summary(102);
    state.sessions[0].sessionMutationStamp = 101;
    h.emit("state", state);
    await h.resolveTail(0, 101, "a");
    expect(h.classify.mock.results[0].value).toBe("partial");
    expect(h.current().messages).toHaveLength(20);
    await flush(250);
    h.expectTailCount(1);
  });

  it("H1: an ahead tail is fenced until summary repair; covered text deltas are then ignored", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 103, "abc");
    expect(h.classify.mock.results[0].value).toBe("stateResync");
    expect(h.current().messages).toEqual([message("a")]);
    h.emit("state", summary(103));
    // This used to launch through a render about 16 ms after the summary.
    // Passive, render-driven admission now deliberately waits for the pending
    // repair deadline. The force/history/restart tests above pin their separate
    // immediate admission; this must not become a general hydration cooldown.
    await flush(SESSION_HYDRATION_RETRY_DELAYS_MS[0] - 1);
    h.expectTailCount(1);
    await flush(1);
    h.expectTailCount(2);
    await h.resolveTail(1, 103, "abc");
    expect(h.current().messages).toHaveLength(20);
    const before = h.reducer.mock.calls.length;
    h.emit("delta", textDelta(102, 1, "b"));
    h.emit("delta", textDelta(103, 2, "c"));
    await flush(100);
    expect(h.reducer.mock.calls).toHaveLength(before);
    expect(h.revisionAction.mock.results.slice(-2).map((entry) => entry.value)).toEqual(["ignore", "ignore"]);
    expect(h.recoveryCalls).toEqual([]);
    h.expectTailCount(2);
  });

  it("H1 lagging-response variant: coherent deltas settle coverage even after hiding", async () => {
    const h = setup();
    await h.visible(true);
    await h.visible(false);
    h.emit("delta", textDelta(102, 1));
    await h.resolveTail(0, 101, "a");
    expect(h.classify.mock.results[0].value).toBe("partialCoverage");
    expect(h.current().messages[19]).toEqual(message("ax"));
    expect(h.current().messageCount).toBe(TOTAL);
    expect(h.recoveryCalls).toEqual([]);
    expect(h.current().messages).toHaveLength(20);
    expect(h.current().messageStartIndex).toBe(1854);
    expect(getSessionRecordSnapshotForTesting(SESSION_ID)?.messages).toHaveLength(20);
    await flush(1000);
    h.expectTailCount(1);
  });

  it("H2: applied text deltas across revision gaps request history text repair, not tail20", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    await h.visible(false);
    let text = "a";
    for (let index = 0; index < 3; index += 1) {
      const revision = 103 + index * 2;
      h.emit("delta", textDelta(revision, text.length));
      text += "x";
      expect(h.reducer.mock.results[h.reducer.mock.results.length - 1].value.kind).toBe("applied");
      expect(h.revisionAction.mock.results[h.revisionAction.mock.results.length - 1].value).toBe("resync");
      const response = tail(revision, text);
      await act(async () => h.historyRequests[index].resolve({
        revision, serverInstanceId: INSTANCE, sessionMutationStamp: revision,
        messageCount: TOTAL, messages: response.session.messages,
        hasMore: true, nextBefore: "message-680851",
      }));
      await flush();
    }
    expect(h.historyRequests).toHaveLength(3);
    expect(h.recoveryCalls).toEqual(Array.from({ length: 3 }, () => [SESSION_ID, {
      allowDivergentTextRepairAfterNewerRevision: true, forceTailRepair: false,
      queueAfterCurrent: false,
    }]));
    h.expectTailCount(1);
  });

  it("H3 control: coherent text deltas and matching summaries do not rearm the hydrated tail", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    for (let index = 0; index < 4; index += 1) {
      const revision = 102 + index * 2;
      h.emit("delta", textDelta(revision, 1 + index));
      const state = summary(revision);
      state.revision = revision + 1;
      h.emit("state", state);
      await flush(16);
    }
    expect(h.recoveryCalls).toEqual([]);
    h.expectTailCount(1);
  });

  it("H3: summaries advancing the unseen session stamp rearm one visible tail per advance with count unchanged", async () => {
    const h = setup();
    await h.visible(true);
    await h.resolveTail(0, 101, "a");
    for (let index = 0; index < 3; index += 1) {
      const revision = 102 + index;
      h.emit("state", summary(revision));
      await flush(16);
      h.expectTailCount(index + 2);
      await h.resolveTail(index + 1, revision, "a" + "x".repeat(index + 1));
      expect(h.current().messageCount).toBe(TOTAL);
      expect(h.current().messages).toHaveLength(20);
    }
    expect(h.recoveryCalls).toEqual([]);
    expect(h.classify.mock.results.map((entry) => entry.value)).toEqual(Array(4).fill("partial"));
    await flush(100);
    h.expectTailCount(4);
    // The same unseen-stamp update alone does not fetch for an inactive pane.
    // Becoming visible consumes the rearmed latch and causes fetch number 5.
    await h.visible(false);
    h.emit("state", summary(105));
    await flush(100);
    h.expectTailCount(4);
    await h.visible(true);
    h.expectTailCount(5);
    await h.resolveTail(4, 105, "axxxx");
    expect(h.current().messageCount).toBe(TOTAL);
  });
});
