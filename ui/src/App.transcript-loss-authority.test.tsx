// Public App regressions for loss authority at real writer/navigation boundaries.
// Only network replies and dispatch timing are controlled; ownership, actions,
// transport, reconciliation, record-store subscriptions and panes stay real.
import { act, cleanup, fireEvent } from "@testing-library/react";
import { startTransition } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import * as live from "./app-live-state";
import * as actions from "./app-session-actions";
import * as authorityModule from "./transcript-repair-authority";
import { getSessionRecordSnapshotForTesting } from "./session-store";
import { requestSessionHistoryAroundPage, requestSessionHistoryNewerPage, requestSessionHistoryOlderPage, requestSessionHistoryTailPage } from "./session-history-demand";
import { SESSION_HISTORY_PAGE_MESSAGE_COUNT } from "./session-tail-policy";
import { SESSION_HYDRATION_RETRY_DELAYS_MS } from "./app-live-state-hydration";
import { buildVirtualizedMessageLayout, estimateConversationMessageHeight } from "./panels/conversation-virtualization";
import type { EngramControlMessage, Session, TextMessage } from "./types";
import {
  advanceTimers, createDeferred, createScheduledAnimationFrameMocks, EventSourceMock,
  flushUiWork, latestEventSource, makeStateResponse, makeWorkspaceLayoutResponse,
  mockScrollToAndApplyTop, renderAppWithProjectAndSession, settleAsyncUi,
  stubElementScrollGeometry, withVerifiedNoReactActWarnings,
} from "./app-test-harness";

const ID = "session-1";
const INSTANCE = "test-instance";
const COUNT = 1000;
const originalScrollTo = HTMLElement.prototype.scrollTo;
const body = (index: number, text = `Canonical body ${index}`): TextMessage => ({
  id: `history-${index}`, type: "text", author: "assistant", timestamp: "10:00", text,
});

// Fresh DTO, not a spread of the resident client Session. Like the wire,
// absent empty queue/markers/history/option lists are actually absent keys.
function wireSession(revision: number, messages: TextMessage[], count = COUNT): Session {
  return { id: ID, name: "Session 1", emoji: "S", agent: "Codex",
    workdir: "/projects/termal", projectId: "project-termal", model: "gpt-5.4",
    reasoningEffort: "medium", approvalPolicy: "never", sandboxMode: "workspace-write",
    status: "active", preview: "", queuePaused: false,
    messageCount: count, sessionMutationStamp: revision, bodySeqEpoch: INSTANCE, bodySeq: revision,
    messages, messagesLoaded: messages.length === count };
}

function tailReply(revision: number, count = COUNT) {
  const first = Math.max(0, count - 20);
  return { revision, serverInstanceId: INSTANCE,
    session: wireSession(revision, Array.from({ length: count - first }, (_, i) => body(first + i)), count) };
}

function recoveryState(revision: number) {
  return makeStateResponse({ revision, serverInstanceId: INSTANCE,
    sessions: [wireSession(revision, []) as Session & { messageCount: number; queuePaused: boolean }],
    projects: [], workspaces: [], orchestrators: [] });
}

// The fake endpoint alone implements the Rust endpoint's centering/clamps.
// No client repair code is stubbed, and every request uses its actual anchor.
function aroundReply(around: number, revision: number, limit = SESSION_HISTORY_PAGE_MESSAGE_COUNT) {
  const provisionalStart = Math.max(0, around - Math.floor(limit / 2));
  const end = Math.min(provisionalStart + limit, COUNT);
  const start = Math.max(0, end - limit);
  return { revision, bodySeqEpoch: INSTANCE, bodySeq: revision, serverInstanceId: INSTANCE, sessionMutationStamp: revision,
    messageCount: COUNT, messageStartIndex: start, hasMore: start > 0,
    hasNewer: end < COUNT, nextBefore: start > 0 ? `history-${start}` : null,
    nextAfter: end < COUNT ? `history-${end - 1}` : null,
    messages: Array.from({ length: end - start }, (_, i) => body(start + i)) };
}

function startReply(start: number, revision: number, limit = SESSION_HISTORY_PAGE_MESSAGE_COUNT) {
  const end = Math.min(start + limit, COUNT);
  return { revision, bodySeqEpoch: INSTANCE, bodySeq: revision, serverInstanceId: INSTANCE, sessionMutationStamp: revision,
    messageCount: COUNT, messageStartIndex: start, hasMore: start > 0, hasNewer: end < COUNT,
    messages: Array.from({ length: end - start }, (_, i) => body(start + i)) };
}

function layoutBackedGeometry() {
  const layout = () => buildVirtualizedMessageLayout(
    (getSessionRecordSnapshotForTesting(ID)?.messages ?? []).map(message => estimateConversationMessageHeight(message)),
  );
  const restoreGeometry = stubElementScrollGeometry({ clientHeight: 200, scrollHeight: () => Math.max(200, layout().totalHeight) });
  mockScrollToAndApplyTop();
  const realRect = HTMLElement.prototype.getBoundingClientRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function(this: HTMLElement): DOMRect {
    const slot = this.classList.contains("virtualized-message-slot") ? this
      : this.classList.contains("virtualized-message-page") ? this.querySelector<HTMLElement>("[data-message-id]") : null;
    const stack = this.closest<HTMLElement>(".message-stack");
    const messages = getSessionRecordSnapshotForTesting(ID)?.messages ?? [];
    const index = messages.findIndex(message => message.id === slot?.dataset.messageId);
    if (slot && stack && index >= 0) {
      const top = layout().tops[index] - stack.scrollTop;
      const height = this === slot ? estimateConversationMessageHeight(messages[index])
        : Array.from(this.querySelectorAll<HTMLElement>("[data-message-id]"))
          .reduce((sum, node) => {
            const message = messages.find(entry => entry.id === node.dataset.messageId);
            return sum + (message ? estimateConversationMessageHeight(message) + 12 : 0);
          }, 0);
      return { top, bottom: top + height, height, left: 0, right: 1000, width: 1000, x: 0, y: top, toJSON: () => ({}) };
    }
    if (this.classList.contains("message-stack")) return { top: 0, bottom: 200, height: 200, left: 0, right: 1000, width: 1000, x: 0, y: 0, toJSON: () => ({}) };
    return realRect.call(this);
  });
  return { layout, restoreGeometry };
}

function observeApp(sessionId = ID) {
  let liveParams!: Parameters<typeof live.useAppLiveState>[0];
  let liveResult!: ReturnType<typeof live.useAppLiveState>;
  let actionParams!: Parameters<typeof actions.useAppSessionActions>[0];
  let actionResult!: ReturnType<typeof actions.useAppSessionActions>;
  let pauseListDispatch = false;
  let listPublicationCount = 0;
  let listDispatch!: (next: () => Session[]) => void;
  const deferredDispatches: Array<() => void> = [];
  const RealAuthority = authorityModule.TranscriptRepairAuthority;
  // Delay the identical owner-issued React updater, not a product decision.
  vi.spyOn(authorityModule, "TranscriptRepairAuthority").mockImplementation(
    function(sinks: ConstructorParameters<typeof RealAuthority>[0], initial?: Session[]) {
      if (!sinks) return new RealAuthority(sinks, initial);
      listDispatch = sinks.publish;
      return new RealAuthority({ ...sinks, publish: next => {
        listPublicationCount++;
        if (pauseListDispatch) deferredDispatches.push(() => listDispatch(next));
        else listDispatch(next);
      } }, initial);
    } as unknown as typeof RealAuthority,
  );
  const realLive = live.useAppLiveState;
  const realActions = actions.useAppSessionActions;
  vi.spyOn(live, "useAppLiveState").mockImplementation(params => {
    liveParams = params;
    liveResult = realLive(params);
    return liveResult;
  });
  vi.spyOn(actions, "useAppSessionActions").mockImplementation(params => {
    actionParams = params;
    actionResult = realActions(params);
    return actionResult;
  });
  const ref = () => liveParams.adoptionRefs.sessionsRef.current.find(s => s.id === sessionId)!;
  const store = () => getSessionRecordSnapshotForTesting(sessionId)!;
  const rendered = () => actionParams.lookups.sessionLookup.get(sessionId)!;
  return { ref, store, rendered, live: () => liveResult, actions: () => actionResult,
    supportingSlices: () => ({ projects: liveParams.adoptionRefs.projectsRef.current,
      workspaces: liveParams.adoptionRefs.workspaceSummariesRef.current,
      orchestrators: liveParams.adoptionRefs.orchestratorsRef.current }),
    revision: () => liveParams.adoptionRefs.latestStateRevisionRef.current,
    sessions: () => liveParams.adoptionRefs.sessionsRef.current,
    listPublications: () => listPublicationCount,
    pauseList() { pauseListDispatch = true; },
    releaseList() {
      pauseListDispatch = false;
      startTransition(() => { for (const dispatch of deferredDispatches.splice(0)) dispatch(); });
    },
    expectBodies(messages: TextMessage[], loaded: boolean) {
      for (const projection of [ref(), store(), rendered()]) {
        expect(projection.messages).toEqual(messages);
        expect(projection.messagesLoaded).toBe(loaded);
      }
    },
  };
}

async function seed(observed: ReturnType<typeof observeApp>, session: Session, revision = 2) {
  // Enter through E5's actual creation/display bootstrap. Replies are bounded
  // wire DTOs, never an extra manual read racing the automatic certificate.
  const count = session.messageCount ?? session.messages.length;
  const tail = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
    revision, serverInstanceId: INSTANCE,
    session: { ...session, messages: session.messages.slice(-20), messagesLoaded: count <= 20 },
  });
  const history = vi.spyOn(api, "fetchSessionHistory").mockImplementation(async (_id, options) => {
    const start = options.start!;
    const end = Math.min(count, start + options.limit!);
    const messages = session.messages.slice(start, end);
    return { revision, serverInstanceId: INSTANCE, bodySeq: session.bodySeq, bodySeqEpoch: session.bodySeqEpoch,
      sessionMutationStamp: session.sessionMutationStamp ?? revision, messageCount: count,
      messageStartIndex: start, hasMore: start > 0, hasNewer: end < count, messages,
      nextBefore: start > 0 ? messages[0]?.id : null, nextAfter: end < count ? messages[messages.length - 1]?.id : null };
  });
  try {
    await act(async () => {
      expect(observed.live().adoptCreatedSessionResponse({
        sessionId: ID, session, revision, serverInstanceId: INSTANCE,
      })).toBe("adopted");
      await flushUiWork();
    });
    await settleAsyncUi();
    expect(observed.ref().bodySeqEpoch).toBe(session.bodySeqEpoch);
    expect(observed.ref().bodySeq).toBe(session.bodySeq);
  } finally { history.mockRestore(); tail.mockRestore(); }
}

it.each([undefined, null])("publishes a newer unknown-stamp snapshot %s after initial stamp one across all sinks", async stamp => {
  const observed = observeApp();
  const context = await renderAppWithProjectAndSession();
  try {
    expect(observed.ref().sessionMutationStamp).toBe(1);
    const { bodySeq: _seq, bodySeqEpoch: _epoch, ...unpaired } = wireSession(2, [], 0);
    await act(async () => {
      expect(observed.live().adoptState(makeStateResponse({ revision: 2, serverInstanceId: INSTANCE,
        projects: [], workspaces: [], orchestrators: [],
        sessions: [{ ...unpaired, messageCount: 0, queuePaused: false,
          status: "active", preview: "New unstamped state", sessionMutationStamp: stamp }],
      }))).toBe(true);
      await flushUiWork();
    });
    await settleAsyncUi();
    for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
      expect(projection.status).toBe("active");
      expect(projection.preview).toBe("New unstamped state");
      expect(projection.sessionMutationStamp).toBe(stamp);
    }
    expect(document.querySelector(".session-activity-strip")).toHaveAttribute("data-state", "working");
  } finally { context.cleanup(); }
});

beforeEach(() => {
  const frames = createScheduledAnimationFrameMocks();
  vi.stubGlobal("requestAnimationFrame", frames.requestAnimationFrameMock);
  vi.stubGlobal("cancelAnimationFrame", frames.cancelAnimationFrameMock);
  HTMLElement.prototype.scrollTo = vi.fn() as typeof HTMLElement.prototype.scrollTo;
  EventSourceMock.instances = [];
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
  vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
  vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(makeWorkspaceLayoutResponse());
});

afterEach(async () => {
  await act(async () => { cleanup(); await flushUiWork(); });
  window.localStorage.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalScrollTo) HTMLElement.prototype.scrollTo = originalScrollTo;
  else delete (HTMLElement.prototype as Partial<HTMLElement>).scrollTo;
});

describe("App transcript loss ownership", () => {

  it("keeps certified local bodies visible through lagged recovery and rejects a stale snapshot", async () => {
    const observed = observeApp();
    const context = await renderAppWithProjectAndSession();
    try {
      const original = [body(0, "Resident before loss")];
      await seed(observed, wireSession(2, original, 1));
      const pending = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
      const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(pending.promise);
      const state = vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
      const complete = [body(0, "Newer live delta body")];
      act(() => {
        latestEventSource().dispatchNamedEvent("lagged", "1");
        latestEventSource().dispatchNamedEvent("delta", {
          type: "codexUpdated", revision: 3, codex: { notices: [] },
        });
        latestEventSource().dispatchNamedEvent("delta", {
          type: "textReplace", revision: 3, sessionId: ID, sessionSeq: 3, bodySeqEpoch: INSTANCE,
          sessionMutationStamp: 3, messageId: "history-0", messageIndex: 0, messageCount: 1,
          text: "Newer live delta body",
        });
      });
      await settleAsyncUi();
      observed.expectBodies(original, true);
      expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("Resident before loss");
      expect(tail).toHaveBeenCalledExactlyOnceWith(ID, 20);
      act(() => latestEventSource().dispatchNamedEvent("state", JSON.stringify(makeStateResponse({
        revision: 2, serverInstanceId: INSTANCE, ...observed.supportingSlices(),
        sessions: [{ ...wireSession(2, [], 1), messageCount: 1, queuePaused: false,
          status: "idle", preview: "Stale recovery" }],
      }))));
      observed.expectBodies(original, true);
      expect(observed.ref().preview).not.toBe("Stale recovery");
      await act(async () => {
        pending.resolve({ revision: 3, serverInstanceId: INSTANCE, session: wireSession(3, complete, 1) });
        await flushUiWork();
      });
      await settleAsyncUi();
      observed.expectBodies(complete, true);
      expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("Newer live delta body");
      expect(observed.ref().bodySeq).toBe(3);
      expect(tail).toHaveBeenCalledTimes(1);
      // Global progress was contiguous; only the dirty body needs a read.
      expect(state).not.toHaveBeenCalled();
    } finally { context.cleanup(); }
  });

  it("D6 repairs a fully loaded local suffix without discarding its aligned head", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const original = Array.from({ length: 26 }, (_, i) => body(i, i === 25 ? "Cut final text" : `Older ${i}`));
        await seed(observed, wireSession(1, original, 26));
        const retainedHead = observed.ref().messages[0];
        const complete = original.map((message, i) => i === 25 ? body(i, "Complete final paragraph") : message);
        const repair = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(repair.promise);
        const snapshot = recoveryState(3);
        Object.assign(snapshot, observed.supportingSlices());
        snapshot.sessions = [{ ...snapshot.sessions[0], bodySeq: 2, messageCount: 26, sessionMutationStamp: 3 }];
        act(() => { latestEventSource().dispatchNamedEvent("state", JSON.stringify(snapshot)); });
        await settleAsyncUi();
        expect(tail).toHaveBeenCalledExactlyOnceWith(ID, 20);
        observed.expectBodies(original, true);
        await act(async () => {
          repair.resolve({ revision: 3, serverInstanceId: INSTANCE,
            session: { ...wireSession(2, complete.slice(-20), 26), sessionMutationStamp: 3 } });
          await flushUiWork();
        });
        await settleAsyncUi();
        observed.expectBodies(complete, true);
        expect(observed.ref().messages[0]).toBe(retainedHead);
        expect(observed.ref().bodySeq).toBe(2);
        expect(document.querySelector('[data-message-id="history-25"]')).toHaveTextContent("Complete final paragraph");
        expect(tail).toHaveBeenCalledTimes(1);
      } finally { context.cleanup(); }
    });
  });
  it.each(["delayed", "dropped"] as const)("shows the complete streamed table and later card when a chunk is %s behind another session's snapshot", async delivery => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        const prefix = "| Item | Status |\n| --- | --- |\n| TermAl - P0 |";
        const chunks = [" Complete answer |\n", "| Engram | Ready |\n", "\nEverything is visible without reloading."];
        const fullText = prefix + chunks.join("");
        await seed(observed, wireSession(1, [body(0, prefix)], 1));
        const card: EngramControlMessage = {
          id: "later-checkpoint", type: "engramControl", author: "assistant", timestamp: "10:01",
          schemaVersion: 1, stage: "checkpoint", assurance: "verified", decision: "grant",
          dispatch: "sent_on_grant", latencyMs: { total: 1 }, failMode: "enforced",
        };
        const canonical: Session = { ...wireSession(5, [body(0, fullText)], 2),
          messages: [body(0, fullText), card], messagesLoaded: true, status: "idle", sessionMutationStamp: 7 };
        const repair = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const tails = vi.spyOn(api, "fetchSessionTail").mockReturnValue(repair.promise);
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        const snapshot = recoveryState(4);
        Object.assign(snapshot, observed.supportingSlices());
        snapshot.sessions = [
          { ...snapshot.sessions[0], bodySeq: 2, messageCount: 1, sessionMutationStamp: 3 },
          { ...snapshot.sessions[0], id: "other-session", name: "Other session", bodySeqEpoch: "other-epoch",
            bodySeq: 1, messageCount: 0, sessionMutationStamp: 4, preview: "Another session committed" },
        ];
        const emit = (index: number, revision: number) => latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "textDelta", revision, sessionId: ID, sessionSeq: index + 2, bodySeqEpoch: INSTANCE,
          messageId: "history-0", messageIndex: 0, messageCount: 1,
          textStartByte: prefix.length + chunks.slice(0, index).join("").length,
          delta: chunks[index], sessionMutationStamp: revision,
        }));
        act(() => {
          // The first chunk committed at R3; another session's R4 snapshot
          // overtook its publication. Subsequent chunks retain wire offsets.
          latestEventSource().dispatchNamedEvent("state", JSON.stringify(snapshot));
          expect(observed.revision()).toBe(4);
          expect(observed.sessions().find(session => session.id === "other-session")?.preview)
            .toBe("Another session committed");
          expect(observed.ref().messages).toEqual([body(0, prefix)]);
          if (delivery === "delayed") emit(0, 3);
          emit(1, 5);
          emit(2, 6);
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
            type: "messageCreated", revision: 7, sessionId: ID, sessionSeq: 5, bodySeqEpoch: INSTANCE,
            messageId: card.id, messageIndex: 1, messageCount: 2, message: card,
            sessionMutationStamp: 7, status: "idle",
          }));
        });
        await settleAsyncUi();
        if (delivery === "dropped") {
          expect(tails).toHaveBeenCalledExactlyOnceWith(ID, 20);
          expect(observed.ref().messages).toEqual([body(0, prefix)]);
          await act(async () => {
            repair.resolve({ revision: 7, serverInstanceId: INSTANCE, session: canonical });
            await flushUiWork();
          });
          await settleAsyncUi();
        } else expect(tails).not.toHaveBeenCalled();
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection.messages).toEqual(canonical.messages);
          expect(projection.bodySeq).toBe(5);
          expect(projection.messagesLoaded).toBe(true);
        }
        const renderedText = document.querySelector('[data-message-id="history-0"]');
        expect(renderedText).toHaveTextContent("Complete answer");
        expect(Array.from(renderedText!.querySelectorAll("tbody tr"), row =>
          Array.from(row.querySelectorAll("td"), cell => cell.textContent?.trim())))
          .toEqual([["TermAl - P0", "Complete answer"], ["Engram", "Ready"]]);
        expect(renderedText).toHaveTextContent("Everything is visible without reloading.");
        expect(document.querySelector('[data-message-id="later-checkpoint"]')).toHaveTextContent("Turn checkpointed");
        expect(tails).toHaveBeenCalledTimes(delivery === "dropped" ? 1 : 0);
      } finally { context.cleanup(); }
    });
  });

  it("attached replacing history navigation autonomously restores its stale tail across all sinks", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "Captured older body")], 1));
        const repaired = wireSession(2, [body(0, "Complete final body")], 1);
        const reply = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const tails = vi.spyOn(api, "fetchSessionTail").mockReturnValue(reply.promise);
        vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
          revision: 1, serverInstanceId: INSTANCE, bodySeq: 1, bodySeqEpoch: INSTANCE,
          sessionMutationStamp: 1, messageCount: 1, messageStartIndex: 0,
          hasMore: false, hasNewer: false, nextBefore: null, nextAfter: null,
          messages: [body(0, "Captured older body")],
        });
        act(() => { latestEventSource().dispatchNamedEvent("delta", {
          type: "textReplace", revision: 1, sessionId: ID, sessionSeq: 2, bodySeqEpoch: INSTANCE,
          sessionMutationStamp: 1, messageId: "history-0", messageIndex: 0, messageCount: 1,
          text: "Complete final body",
        }); });
        await settleAsyncUi();
        observed.expectBodies(repaired.messages as TextMessage[], true);
        await act(async () => { expect(await requestSessionHistoryAroundPage(ID, 0)).toBe(true); await flushUiWork(); });
        await settleAsyncUi();
        expect(tails).toHaveBeenCalledExactlyOnceWith(ID, 20);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection.bodySeq).toBeUndefined();
        }
        await act(async () => { reply.resolve({ revision: 3, serverInstanceId: INSTANCE, session: repaired }); await flushUiWork(); });
        await settleAsyncUi();
        observed.expectBodies(repaired.messages as TextMessage[], true);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) expect(projection.bodySeq).toBe(2);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("Complete final body");
        expect(tails).toHaveBeenCalledTimes(1);
      } finally { context.cleanup(); }
    });
  });

  it.each(["failure", "abort"] as const)("M1: %s replacing navigation cannot strand summary-ahead verification", async terminal => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "Before")], 1));
        let resolveNavigation!: (page: Awaited<ReturnType<typeof api.fetchSessionHistory>>) => void;
        let rejectNavigation!: (error: Error) => void;
        const navigation = new Promise<Awaited<ReturnType<typeof api.fetchSessionHistory>>>((resolve, reject) => {
          resolveNavigation = resolve; rejectNavigation = reject;
        });
        vi.spyOn(api, "fetchSessionHistory").mockReturnValueOnce(navigation);
        const controller = new AbortController();
        let demand!: Promise<boolean>;
        act(() => { demand = terminal === "abort"
          ? requestSessionHistoryTailPage(ID, { signal: controller.signal })
          : requestSessionHistoryAroundPage(ID, 0); });
        await act(async () => {
          if (terminal === "abort") {
            controller.abort();
          } else rejectNavigation(new Error("Transient navigation failure"));
          await expect(demand).resolves.toBe(false);
          await flushUiWork();
        });
        const repaired = wireSession(2, [body(0, "Verified final body")], 1);
        const tails = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({
          revision: 3, serverInstanceId: INSTANCE, session: repaired,
        });
        const summary = recoveryState(3);
        Object.assign(summary, observed.supportingSlices());
        summary.sessions = [{ ...summary.sessions[0], bodySeq: 2, messageCount: 1, sessionMutationStamp: 2 }];
        act(() => { observed.live().adoptState(summary); });
        await settleAsyncUi();
        expect(tails).toHaveBeenCalledExactlyOnceWith(ID, 20);
        observed.expectBodies(repaired.messages as TextMessage[], true);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("Verified final body");
        if (terminal === "abort") {
          // Repair must not wait for the abandoned HTTP request. Its late old
          // answer must also remain unable to replace the verified body.
          await act(async () => {
            resolveNavigation({ ...startReply(0, 1, 1), messageCount: 1, hasNewer: false });
            await flushUiWork();
          });
          await settleAsyncUi();
          observed.expectBodies(repaired.messages as TextMessage[], true);
          expect(tails).toHaveBeenCalledTimes(1);
        }
      } finally { context.cleanup(); }
    });
  });

  it("M2: stale-global next body preserves newer HTTP status, preview, stamp and queue at all sinks", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        const pendingPrompts = [{ id: "current-prompt", text: "Current queue", timestamp: "now" }];
        // Broad summaries redact queue bodies. Establish them with the real
        // targeted creation/read path before adopting newer HTTP metadata.
        await seed(observed, { ...wireSession(1, [body(0, "Before")], 1), pendingPrompts });
        const tails = vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
        const state = recoveryState(20);
        Object.assign(state, observed.supportingSlices());
        state.sessions = [{ ...state.sessions[0], bodySeq: 2, messageCount: 2, sessionMutationStamp: 20, status: "idle",
          preview: "New HTTP preview", queuePaused: true, queueProjectionHash: "new-queue" }];
        act(() => {
          observed.live().adoptState(state);
          expect(observed.ref()).toMatchObject({ status: "idle", sessionMutationStamp: 20, pendingPrompts });
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({ type: "messageCreated",
            revision: 3, sessionId: ID, sessionSeq: 2, bodySeqEpoch: INSTANCE,
            messageId: "history-1", messageIndex: 1, messageCount: 2, message: body(1, "Next body"),
            status: "active", preview: "Old SSE preview", sessionMutationStamp: 3,
            sessionQueue: { pendingPrompts: [], queuePaused: false, queueProjectionHash: "old-queue" },
          }));
        });
        await settleAsyncUi();
        observed.expectBodies([body(0, "Before"), body(1, "Next body")], true);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection).toMatchObject({ status: "idle", preview: "New HTTP preview",
            sessionMutationStamp: 20, queuePaused: true, queueProjectionHash: "new-queue", pendingPrompts });
        }
        expect(observed.revision()).toBe(20);
        expect(tails).not.toHaveBeenCalled();
      } finally { context.cleanup(); }
    });
  });

  // The stream does not suppress a retained event for an older global revision
  // after a catch-up snapshot; the global revision governs metadata, while a
  // body delta is governed by its per-session body sequence and epoch.
  it("after adopting a catch-up snapshot, processes an older-global body delta its body sequence still needs", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "Before")], 1));
        vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
        // The catch-up snapshot at R = 12 says the session's body sequence is
        // at 2, but carries no message body.
        const snapshot = recoveryState(12);
        Object.assign(snapshot, observed.supportingSlices());
        snapshot.sessions = [{ ...snapshot.sessions[0], bodySeq: 2, messageCount: 2,
          sessionMutationStamp: 12, status: "idle", preview: "Snapshot preview" }];
        act(() => {
          observed.live().adoptState(snapshot);
          // A retained delta of global revision 4, below R, is the body that
          // sequence 2 still needs.
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({ type: "messageCreated",
            revision: 4, sessionId: ID, sessionSeq: 2, bodySeqEpoch: INSTANCE,
            messageId: "history-1", messageIndex: 1, messageCount: 2, message: body(1, "Retained older body"),
            status: "active", preview: "Older SSE preview", sessionMutationStamp: 4,
          }));
        });
        await settleAsyncUi();
        observed.expectBodies([body(0, "Before"), body(1, "Retained older body")], true);
        expect(document.querySelector('[data-message-id="history-1"]')).toHaveTextContent("Retained older body");
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection).toMatchObject({ status: "idle", preview: "Snapshot preview", sessionMutationStamp: 12 });
        }
        expect(observed.revision()).toBe(12);
        // The tail stub never resolves, so the body asserted above came from
        // the retained delta, never from a read. Whether the snapshot's own
        // read had started before the delta arrived depends only on dispatch
        // timing, which is not part of this contract and is not asserted.
      } finally { context.cleanup(); }
    });
  });

  it.each(["next", "same"] as const)("M2: %s global revision still admits current body-delta metadata", async order => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "Before")], 1));
        const state = recoveryState(10);
        Object.assign(state, observed.supportingSlices());
        state.sessions = [{ ...state.sessions[0], bodySeq: 1, messageCount: 1, sessionMutationStamp: 1, status: "idle" }];
        act(() => { observed.live().adoptState(state); });
        const pendingPrompts = [{ id: "next-prompt", timestamp: "now", text: "Next queued prompt" }];
        const revision = order === "next" ? 11 : 10;
        act(() => { latestEventSource().dispatchNamedEvent("delta", JSON.stringify({ type: "messageCreated",
          revision, sessionId: ID, sessionSeq: 2, bodySeqEpoch: INSTANCE,
          messageId: "history-1", messageIndex: 1, messageCount: 2, message: body(1, "Next body"),
          status: "active", preview: "Current SSE preview", sessionMutationStamp: 2,
          sessionQueue: { pendingPrompts, queuePaused: true, queueProjectionHash: "next-queue" },
        })); });
        await settleAsyncUi();
        observed.expectBodies([body(0, "Before"), body(1, "Next body")], true);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection).toMatchObject({ status: "active", preview: "Current SSE preview",
            sessionMutationStamp: 2, queuePaused: true, queueProjectionHash: "next-queue", pendingPrompts });
        }
        expect(observed.revision()).toBe(revision);
      } finally { context.cleanup(); }
    });
  });

  it("F1: repeated paired snapshots preserve an uncertified hidden session and all list/store identities", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        await seed(observed, wireSession(10, [body(0)], 1), 10);
        const hidden = { ...wireSession(10, [], 1), id: "hidden-paired", name: "Hidden" };
        const snapshot = makeStateResponse({ ...observed.supportingSlices(), revision: 11,
          serverInstanceId: INSTANCE, sessions: [wireSession(10, [], 1), hidden] as Parameters<typeof makeStateResponse>[0]["sessions"] });
        act(() => { observed.live().adoptState(snapshot, { force: true }); });
        await settleAsyncUi();
        const previous = observed.sessions();
        const resident = previous.find(s => s.id === hidden.id)!;
        const slice = getSessionRecordSnapshotForTesting(hidden.id);
        const publications = observed.listPublications();
        expect(resident.bodySeq).toBeUndefined();
        for (const revision of [12, 13]) {
          act(() => { observed.live().adoptState({ ...snapshot, revision }, { force: true }); });
          await settleAsyncUi();
          expect(observed.sessions()).toBe(previous);
          expect(observed.sessions().find(s => s.id === hidden.id)).toBe(resident);
          expect(getSessionRecordSnapshotForTesting(hidden.id)).toBe(slice);
          expect(observed.listPublications(), "unchanged paired snapshots publish no session list").toBe(publications);
        }
      } finally { context.cleanup(); }
    });
  });

  it("F2: delayed loss-tail metadata and range commit cannot roll newer idle metadata back", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const messages = Array.from({ length: 4 }, (_, i) => body(i));
        const pendingPrompts = [{ id: "current-prompt", timestamp: "now", text: "Current queue" }];
        // Broad summaries omit prompt bodies. Establish the queue through the
        // targeted read before testing metadata freshness during loss repair.
        await seed(observed, { ...wireSession(10, messages, 4), pendingPrompts }, 10);
        const deferred = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(deferred.promise);
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        act(() => { latestEventSource().dispatchNamedEvent("lagged", "1"); });
        await settleAsyncUi();
        expect(tail).toHaveBeenCalledTimes(1);
        const current = { ...wireSession(10, [], 4), status: "idle" as const,
          preview: "Completed turn", sessionMutationStamp: 20, queuePaused: true,
          pendingPrompts };
        const snapshot = makeStateResponse({ ...observed.supportingSlices(), revision: 20,
          serverInstanceId: INSTANCE, sessions: [current] as Parameters<typeof makeStateResponse>[0]["sessions"] });
        act(() => { observed.live().adoptState(snapshot, { force: true }); });
        expect(observed.ref()).toMatchObject({ sessionMutationStamp: 20, pendingPrompts });
        await act(async () => { deferred.resolve({ revision: 11, serverInstanceId: INSTANCE,
          session: { ...wireSession(10, messages, 4), sessionMutationStamp: 11, preview: "Earlier active turn" } });
          await flushUiWork(); });
        await settleAsyncUi();
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection).toMatchObject({ status: "idle", preview: current.preview,
            sessionMutationStamp: 20, queuePaused: true, pendingPrompts: current.pendingPrompts });
          expect(projection.messages).toEqual(messages);
        }
      } finally { context.cleanup(); }
    });
  });

  it.each(["next", "same", "equalStamp"] as const)("L4: %s delayed body metadata respects the ahead HTTP mutation stamp", async order => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "Before")], 1), 10);
        const pendingPrompts = [{ id: "current-prompt", timestamp: "now", text: "Current queue" }];
        const canonical = { ...wireSession(2, [body(0, "Before"), body(1, "Next body")], 2),
          sessionMutationStamp: 20, status: "idle" as const, preview: "Completed turn",
          queuePaused: true, queueProjectionHash: "new-queue", pendingPrompts };
        const tails = vi.spyOn(api, "fetchSessionTail").mockResolvedValue({ revision: 13,
          serverInstanceId: INSTANCE, session: canonical });
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        act(() => { latestEventSource().dispatchNamedEvent("lagged", "1"); });
        await settleAsyncUi();
        expect(tails).toHaveBeenCalledTimes(1);
        expect(observed.revision()).toBe(10);
        expect(observed.ref()).toMatchObject({ status: "idle", sessionMutationStamp: 20, pendingPrompts });
        act(() => {
          if (order === "same") latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
            type: "sessionCreated", revision: 11, sessionId: "same-commit-child", session: { ...wireSession(0, [], 0),
              id: "same-commit-child", name: "Same commit child" },
          }));
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({ type: "messageCreated",
            revision: 11, sessionId: ID, sessionSeq: 2, bodySeqEpoch: INSTANCE,
            messageId: "history-1", messageIndex: 1, messageCount: 2, message: body(1, "Next body"),
            sessionMutationStamp: order === "equalStamp" ? 20 : 11,
            status: order === "equalStamp" ? "idle" : "active",
            preview: order === "equalStamp" ? canonical.preview : "Earlier active turn",
            sessionQueue: order === "equalStamp" ? { pendingPrompts, queuePaused: true, queueProjectionHash: "new-queue" }
              : { pendingPrompts: [], queuePaused: false, queueProjectionHash: "old-queue" },
          }));
        });
        await settleAsyncUi();
        expect(observed.revision()).toBe(11);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection).toMatchObject({ status: "idle", sessionMutationStamp: 20,
            preview: canonical.preview,
            queuePaused: true, queueProjectionHash: "new-queue", pendingPrompts });
          expect(projection.messages).toEqual(canonical.messages);
          expect(projection.bodySeq).toBe(2);
        }
        expect(tails).toHaveBeenCalledTimes(1);
      } finally { context.cleanup(); }
    });
  });

  it("F3: a paired repair read at R+3 leaves queued SessionCreated at R+1 visible", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const messages = [body(0)];
        await seed(observed, wireSession(10, messages, 1), 10);
        vi.spyOn(api, "fetchSessionTail").mockResolvedValue({ revision: 13, serverInstanceId: INSTANCE,
          session: wireSession(10, messages, 1) });
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        act(() => { latestEventSource().dispatchNamedEvent("lagged", "1"); });
        await settleAsyncUi();
        expect(observed.revision(), "one-session HTTP proof is not a global snapshot").toBe(10);
        const child = { ...wireSession(0, [], 0), id: "queued-child", name: "Queued child" };
        act(() => { latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "sessionCreated", revision: 11, sessionId: child.id, session: child,
        })); });
        await settleAsyncUi();
        expect(observed.sessions().find(s => s.id === child.id)?.name).toBe(child.name);
        expect(getSessionRecordSnapshotForTesting(child.id)?.name).toBe(child.name);
        expect(observed.revision()).toBe(11);
        observed.expectBodies(messages, true);
      } finally { context.cleanup(); }
    });
  });

  // Unstaged decision evidence: the fetched page's proof cannot certify an
  // independently retained range whose next body frame has not arrived.
  it("EVIDENCE keeps certified bodies when a same-commit summary precedes its body delta", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        initial.sessions[0].bodySeq = 0;
        act(() => { observed.live().adoptState(initial, { force: true }); });
        const resident = body(0, "Certified resident before Stop or delegation commit");
        await seed(observed, wireSession(10, [resident], 1), 10);
        observed.expectBodies([resident], true);
        const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        const summary = recoveryState(11);
        Object.assign(summary, observed.supportingSlices());
        summary.sessions[0].messageCount = 2;
        const notice = body(1, "Same-commit lifecycle notice");
        act(() => { latestEventSource().dispatchNamedEvent("state", summary); });
        const afterSummary = [observed.ref(), observed.store()];
        act(() => { latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "messageCreated", revision: 11, sessionId: ID, messageId: notice.id,
          messageIndex: 1, messageCount: 2, message: notice, preview: "", status: "active",
          sessionSeq: 11, bodySeqEpoch: INSTANCE, sessionMutationStamp: 11,
        })); });
        await settleAsyncUi();
        for (const projection of afterSummary) expect(projection.messages).toEqual([resident]);
        observed.expectBodies([resident, notice], true);
        expect(tail).not.toHaveBeenCalled();
      } finally { context.cleanup(); }
    });
  });

  it.each(["gap", "ordered", "stale"] as const)("keeps body and global continuity independent (%s)", async order => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        initial.sessions[0].bodySeq = 0;
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "A")], 1));
        const snapshot = recoveryState(10);
        Object.assign(snapshot, observed.supportingSlices());
        snapshot.sessions[0] = { ...snapshot.sessions[0], messageCount: 1, bodySeq: 1, sessionMutationStamp: 1 };
        act(() => { observed.live().adoptState(snapshot); });
        const resync = createDeferred<Awaited<ReturnType<typeof api.fetchState>>>();
        const fetchState = vi.spyOn(api, "fetchState").mockReturnValue(resync.promise);
        const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
        const newcomer = { ...wireSession(0, [], 0), id: "created-11", name: "Created at 11" };
        const emitCreated = () => latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "sessionCreated", revision: 11, sessionId: newcomer.id, session: newcomer,
        }));
        const emitBody = () => latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "textDelta", revision: order === "stale" ? 9 : 12, sessionId: ID,
          bodySeqEpoch: INSTANCE, sessionSeq: 2, sessionMutationStamp: 2,
          messageId: "history-0", messageIndex: 0, messageCount: 1, textStartByte: 1, delta: "B",
        }));
        act(() => {
          if (order === "ordered") emitCreated();
          emitBody();
          expect(observed.ref().messages).toEqual([body(0, "AB")]);
          expect(observed.revision()).toBe(order === "ordered" ? 12 : 10);
          if (order === "gap") emitCreated();
        });
        await settleAsyncUi();
        observed.expectBodies([body(0, "AB")], true);
        expect(tail).not.toHaveBeenCalled();
        if (order === "gap") {
          expect(fetchState).toHaveBeenCalledTimes(1);
          const repaired = recoveryState(12);
          Object.assign(repaired, observed.supportingSlices());
          repaired.sessions[0] = { ...repaired.sessions[0], messageCount: 1, bodySeq: 2, sessionMutationStamp: 2 };
          repaired.sessions.push({ ...newcomer, queuePaused: false, messageCount: 0 });
          await act(async () => { resync.resolve(repaired); await flushUiWork(); });
          await settleAsyncUi();
        } else expect(fetchState).not.toHaveBeenCalled();
        if (order !== "stale") expect(observed.sessions().some(session => session.id === newcomer.id)).toBe(true);
        expect(observed.revision()).toBe(order === "stale" ? 10 : 12);
        observed.expectBodies([body(0, "AB")], true); // Applied exactly once, never evicted.
        expect(tail).not.toHaveBeenCalled();
      } finally { context.cleanup(); }
    });
  });

  it("certifies an empty UI creation after observing its first paired frame", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const id = "session-empty";
      const observed = observeApp(id);
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await settleAsyncUi();
        const empty = { ...wireSession(0, [], 0), id };
        delete empty.bodySeq;
        delete empty.bodySeqEpoch;
        vi.spyOn(api, "createSession").mockResolvedValue({ sessionId: id, session: empty, revision: 2, serverInstanceId: INSTANCE });
        vi.spyOn(api, "refreshSessionModelOptions").mockReturnValue(new Promise(() => {}));
        const read = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const realTail = api.fetchSessionTail;
        const tails = vi.spyOn(api, "fetchSessionTail").mockImplementation((sessionId, limit) =>
          sessionId === id ? read.promise : realTail(sessionId, limit));
        const calls = () => tails.mock.calls.filter(call => call[0] === id);
        const first = () => {
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({ type: "codexUpdated", revision: 3, codex: {} }));
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "messageCreated", revision: 2, sessionId: id, messageIndex: 0,
          messageId: "history-0", messageCount: 1, sessionMutationStamp: 1,
          message: body(0, "A"), preview: "A", status: "active",
          sessionSeq: 1, bodySeqEpoch: INSTANCE,
          }));
        };
        await act(async () => {
          expect(await observed.actions().handleNewSession({ agent: "Codex", projectSelectionId: "project-termal" })).toBe(true);
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(calls()).toEqual([]);
        act(first);
        await settleAsyncUi();
        expect(calls()).toEqual([[id, 20]]);
        expect(observed.ref().messages).toEqual([]); // Unrelated global progress drops the legacy frame.
        const snapshot = { ...wireSession(1, [body(0, "A")], 1), id };
        await act(async () => { read.resolve({ revision: 3, serverInstanceId: INSTANCE, session: snapshot }); await flushUiWork(); });
        await settleAsyncUi();
        observed.expectBodies([body(0, "A")], true);
        expect(observed.ref().bodySeqEpoch).toBe(INSTANCE);
        expect(observed.ref().bodySeq).toBe(1);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("A");
        expect(calls()).toEqual([[id, 20]]);
      } finally { context.cleanup(); }
    });
  });

  it.each([0, 1])("does not certify a UI creation without an observed pair (%s bodies)", async count => {
    await withVerifiedNoReactActWarnings(async () => {
      const id = "session-legacy-created";
      const observed = observeApp(id);
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await settleAsyncUi();
        const createdBodies = count ? [body(0, "A")] : [];
        const created = { ...wireSession(count, createdBodies, count), id };
        delete created.bodySeq;
        delete created.bodySeqEpoch;
        vi.spyOn(api, "createSession").mockResolvedValue({ sessionId: id, session: created, revision: 2, serverInstanceId: INSTANCE });
        vi.spyOn(api, "refreshSessionModelOptions").mockReturnValue(new Promise(() => {}));
        const realTail = api.fetchSessionTail;
        const tails = vi.spyOn(api, "fetchSessionTail").mockImplementation((sessionId, limit) =>
          sessionId === id ? new Promise(() => {}) : realTail(sessionId, limit));
        await act(async () => {
          expect(await observed.actions().handleNewSession({ agent: "Codex", projectSelectionId: "project-termal" })).toBe(true);
          await flushUiWork();
        });
        await settleAsyncUi();
        observed.expectBodies(createdBodies, true);
        expect(tails.mock.calls.filter(call => call[0] === id)).toEqual([]);
        act(() => { latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "messageCreated", revision: 3, sessionId: id, messageIndex: count,
          messageId: `history-${count}`, messageCount: count + 1, sessionMutationStamp: count + 1,
          message: body(count, "B"), preview: "B", status: "active",
        })); });
        await settleAsyncUi();
        observed.expectBodies([...createdBodies, body(count, "B")], true);
        expect(observed.ref().bodySeq).toBeUndefined();
        expect(observed.ref().bodySeqEpoch).toBeUndefined();
        expect(tails.mock.calls.filter(call => call[0] === id)).toEqual([]);
      } finally { context.cleanup(); }
    });
  });

  it("arms certification from a pair observed while hidden and reads only at the next display", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const id = "session-hidden";
      const observed = observeApp(id);
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await settleAsyncUi();
        const read = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const realTail = api.fetchSessionTail;
        const tails = vi.spyOn(api, "fetchSessionTail").mockImplementation((sessionId, limit) =>
          sessionId === id ? read.promise : realTail(sessionId, limit));
        const hidden = { ...wireSession(1, [body(0, "A")], 1), id };
        delete hidden.bodySeq;
        delete hidden.bodySeqEpoch;
        await act(async () => {
          expect(observed.live().adoptCreatedSessionResponse({ sessionId: id,
            session: hidden, revision: 2, serverInstanceId: INSTANCE },
          { openSessionId: ID })).toBe("adopted");
          await flushUiWork();
        });
        expect(tails.mock.calls.filter(call => call[0] === id)).toEqual([]);
        act(() => { latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "textDelta", revision: 3, sessionId: id, sessionSeq: 2, bodySeqEpoch: INSTANCE,
          messageId: "history-0", messageIndex: 0, messageCount: 1,
          textStartByte: 1, delta: "B", sessionMutationStamp: 2,
        })); });
        await settleAsyncUi();
        expect(tails.mock.calls.filter(call => call[0] === id)).toEqual([]);
        observed.expectBodies([body(0, "AB")], true);
        await act(async () => {
          expect(observed.live().adoptCreatedSessionResponse({ sessionId: id, session: observed.ref(), revision: 4, serverInstanceId: INSTANCE })).toBe("adopted");
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(tails.mock.calls.filter(call => call[0] === id)).toEqual([[id, 20]]);
        observed.expectBodies([body(0, "AB")], true);
        await act(async () => { read.resolve({ revision: 4, serverInstanceId: INSTANCE,
          session: { ...wireSession(2, [body(0, "AB")], 1), id } }); await flushUiWork(); });
        await settleAsyncUi();
        expect(observed.ref().bodySeq).toBe(2);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("AB");
      } finally { context.cleanup(); }
    });
  });

  it("receives body sequences before global revision gating, including a same-revision batch and a dropped frame", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const received = vi.spyOn(authorityModule.TranscriptRepairAuthority.prototype, "receiveBodyDelta");
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        // The generic harness initially serves an untagged broad snapshot.
        // Establish this stream's identity before testing same-instance HTTP
        // progress, rather than accidentally exercising restart unloading.
        const initialState = recoveryState(1);
        Object.assign(initialState, observed.supportingSlices());
        initialState.sessions[0].bodySeq = 0;
        act(() => { observed.live().adoptState(initialState, { force: true }); });
        await seed(observed, wireSession(1, [body(0, "A")], 1));
        observed.expectBodies([body(0, "A")], true);
        expect(observed.ref().bodySeq).toBe(1);
        const progressed = recoveryState(100);
        Object.assign(progressed, observed.supportingSlices());
        progressed.sessions = [{ ...progressed.sessions[0], bodySeqEpoch: INSTANCE, bodySeq: 1, sessionMutationStamp: 1, messageCount: 1 }];
        act(() => { observed.live().adoptState(progressed); });
        observed.expectBodies([body(0, "A")], true);
        const emit = (seq: number, offset: number, text: string) => latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
          type: "textDelta", sessionId: ID, bodySeqEpoch: INSTANCE, sessionSeq: seq, revision: 3,
          messageId: "history-0", messageIndex: 0, messageCount: 1,
          textStartByte: offset, delta: text, sessionMutationStamp: seq,
        }));
        act(() => { emit(2, 1, "B"); emit(3, 2, "C"); emit(2, 1, "B"); });
        expect(received.mock.results.map(result => result.value)).toEqual(["applied", "applied", "late"]);
        await settleAsyncUi();
        observed.expectBodies([body(0, "ABC")], true);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("ABC");
        const repair = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        vi.spyOn(api, "fetchSessionTail").mockReturnValue(repair.promise);
        // Sequence 4 was lost. The sequence-5 frame is retained while the
        // replacement read supplies all changes through 4, with no pause.
        act(() => { emit(5, 4, "E"); });
        expect(observed.ref().messages).toEqual([body(0, "ABC")]);
        await act(async () => {
          repair.resolve({ revision: 4, serverInstanceId: INSTANCE,
            session: wireSession(4, [body(0, "ABCD")], 1) });
          await flushUiWork();
        });
        await settleAsyncUi();
        observed.expectBodies([body(0, "ABCDE")], true);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("ABCDE");
        act(() => { emit(6, 5, "F"); });
        await settleAsyncUi();
        observed.expectBodies([body(0, "ABCDEF")], true);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("ABCDEF");
      } finally { context.cleanup(); }
    });
  });

  it("W-M3: a same-revision create and update, sequenced in enqueue order, complete the body without a read", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        await seed(observed, wireSession(2, [body(0, "Pending approval")], 1));
        const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        act(() => {
          // One successful Stop commit at revision 3: the terminal message is
          // created first, then the cancelled interaction is updated.
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
            type: "messageCreated", revision: 3, sessionId: ID, sessionSeq: 3, bodySeqEpoch: INSTANCE,
            messageId: "history-1", messageIndex: 1, messageCount: 2,
            message: body(1, "Turn stopped by user."), preview: "Turn stopped by user.",
            status: "idle", sessionMutationStamp: 3,
          }));
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
            type: "messageUpdated", revision: 3, sessionId: ID, sessionSeq: 4, bodySeqEpoch: INSTANCE,
            messageId: "history-0", messageIndex: 0, messageCount: 2,
            message: body(0, "Rejected approval"), preview: "Turn stopped by user.",
            status: "idle", sessionMutationStamp: 3,
          }));
        });
        await settleAsyncUi();
        observed.expectBodies([body(0, "Rejected approval"), body(1, "Turn stopped by user.")], true);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection.bodySeq).toBe(4);
        }
        expect(document.querySelector('[data-message-id="history-1"]')).toHaveTextContent("Turn stopped by user.");
        expect(tail).not.toHaveBeenCalled();
      } finally { context.cleanup(); }
    });
  });

  it("W-M4: loss, lagged, a recovery snapshot, then an older retained delta neither rolls back nor leaves the body incomplete", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const initial = recoveryState(1);
        Object.assign(initial, observed.supportingSlices());
        act(() => { observed.live().adoptState(initial, { force: true }); });
        await seed(observed, wireSession(2, [body(0, "A")], 1));
        const complete = [body(0, "AB")];
        const repair = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        const tail = vi.spyOn(api, "fetchSessionTail").mockReturnValue(repair.promise);
        vi.spyOn(api, "fetchState").mockReturnValue(new Promise(() => {}));
        // The recovery snapshot is taken fresh at R5, ahead of a delta at r3
        // that was still retained in the mailbox behind the loss signal.
        const recovery = recoveryState(5);
        Object.assign(recovery, observed.supportingSlices());
        recovery.sessions = [{ ...recovery.sessions[0], bodySeq: 3, messageCount: 1, sessionMutationStamp: 5,
          status: "idle", preview: "Recovered at R5" }];
        act(() => {
          latestEventSource().dispatchNamedEvent("lagged", "1");
          latestEventSource().dispatchNamedEvent("state", JSON.stringify(recovery));
          latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
            type: "textDelta", revision: 3, sessionId: ID, sessionSeq: 3, bodySeqEpoch: INSTANCE,
            messageId: "history-0", messageIndex: 0, messageCount: 1,
            textStartByte: 1, delta: "B", sessionMutationStamp: 3,
          }));
        });
        await settleAsyncUi();
        expect(observed.revision(), "the older retained delta does not roll the revision back").toBe(5);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection).toMatchObject({ status: "idle", preview: "Recovered at R5", sessionMutationStamp: 5 });
        }
        if (tail.mock.calls.length > 0) {
          await act(async () => {
            repair.resolve({ revision: 5, serverInstanceId: INSTANCE,
              session: { ...wireSession(3, complete, 1), sessionMutationStamp: 5, status: "idle", preview: "Recovered at R5" } });
            await flushUiWork();
          });
          await settleAsyncUi();
        }
        observed.expectBodies(complete, true);
        expect(observed.ref().bodySeq).toBe(3);
        expect(observed.revision()).toBe(5);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("AB");
        expect(tail.mock.calls.length).toBeLessThanOrEqual(1);
      } finally { context.cleanup(); }
    });
  });

  it("keeps dirty bodies visible at all sinks when stale settings are published before the list frame", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        await seed(observed, wireSession(2, [body(0, "Pre-loss settings body")], 1));
        observed.expectBodies([body(0, "Pre-loss settings body")], true);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("Pre-loss settings body");
        const repair = createDeferred<Awaited<ReturnType<typeof api.fetchSessionTail>>>();
        vi.spyOn(api, "fetchSessionTail").mockReturnValue(repair.promise);
        vi.spyOn(api, "updateSessionSettings").mockReturnValue(new Promise(() => {}));
        act(() => {
          latestEventSource().dispatchNamedEvent("lagged", "1");
          expect(observed.ref().messages).toEqual([body(0, "Pre-loss settings body")]);
          // App's render lookup has not received the scheduled list frame yet.
          expect(observed.rendered().messages).toEqual([body(0, "Pre-loss settings body")]);
          void observed.actions().handleSessionSettingsChange(ID, "reasoningEffort", "high");
        });
        await settleAsyncUi();
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection.reasoningEffort).toBe("high");
        }
        observed.expectBodies([body(0, "Pre-loss settings body")], true);
        expect(document.querySelector('[data-message-id="history-0"]')).toHaveTextContent("Pre-loss settings body");
        await act(async () => { repair.resolve(tailReply(2, 1)); await flushUiWork(); });
        await settleAsyncUi();
        observed.expectBodies([body(0)], true);
      } finally { context.cleanup(); }
    });
  });

  it("does not back-write a pre-loss transition into the live ref while dirty", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        await seed(observed, wireSession(2, [body(0)], 1));
        vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
        observed.pauseList();
        act(() => {
          expect(observed.live().adoptCreatedSessionResponse({
            sessionId: ID, session: { ...wireSession(2, [body(0)], 1), name: "Pending pre-loss transition" },
            revision: 3, serverInstanceId: INSTANCE,
          })).toBe("adopted");
          latestEventSource().dispatchNamedEvent("lagged", "1");
          expect(observed.ref().messages).toEqual([body(0)]);
          observed.releaseList();
        });
        await settleAsyncUi();
        observed.expectBodies([body(0)], true);
      } finally { context.cleanup(); }
    });
  });

  it.each([false, true])("clears an omitted empty queue after a covering read (detached %s)", async detached => {
    await withVerifiedNoReactActWarnings(async () => {
      const observed = observeApp();
      const context = await renderAppWithProjectAndSession();
      try {
        const queued = { id: "queued-P", timestamp: "10:00", text: "Queued P before lost dispatch" };
        await seed(observed, { ...wireSession(2, Array.from({ length: COUNT }, (_, i) => body(i))), pendingPrompts: [queued] });
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection.pendingPrompts).toEqual([queued]);
        }
        expect(document.querySelector(".workspace-pane.active")).toHaveTextContent(queued.text);
        let revision = 2;
        vi.spyOn(api, "fetchSessionHistory").mockImplementation(async (_, request) => request.start !== undefined
          ? startReply(request.start, revision, request.limit) : aroundReply(request.around!, revision, request.limit));
        if (detached) {
          await act(async () => { expect(await requestSessionHistoryAroundPage(ID, 500)).toBe(true); await flushUiWork(); });
          await settleAsyncUi();
        }
        revision = 3; // Dispatch of P was lost; only fresh metadata/body reads arrive.
        const read = tailReply(revision);
        expect(Object.prototype.hasOwnProperty.call(read.session, "pendingPrompts")).toBe(false);
        const tail = vi.spyOn(api, "fetchSessionTail").mockResolvedValue(read);
        act(() => {
          latestEventSource().dispatchNamedEvent("lagged", "1");
          latestEventSource().dispatchNamedEvent("state", recoveryState(revision));
        });
        await settleAsyncUi();
        expect(tail).toHaveBeenCalledTimes(1);
        for (const projection of [observed.ref(), observed.store(), observed.rendered()]) {
          expect(projection.pendingPrompts ?? []).toEqual([]);
        }
        expect(document.querySelector(".workspace-pane.active")).not.toHaveTextContent(queued.text);
      } finally { context.cleanup(); }
    });
  });
});
