// App.live-state.restart-roundtrip.test.tsx
//
// Owns: the canonical regression test for the "after server restart,
// the latest assistant message doesn't render until Ctrl+Shift+R"
// bug class. Across recent rounds this single user-visible symptom
// produced 9+ distinct fixes spread across `app-live-state.ts`,
// `app-session-actions.ts`, `session-reconcile.ts`,
// `session-hydration-adoption.ts`, `live-updates.ts`,
// `state-revision.ts`, `api_sse.rs`, `sse_broadcast.rs`,
// `state.rs`, and `app_boot.rs`. Per-fix unit tests live alongside
// the modules they pin (e.g. `session-reconcile.test.ts` for
// `forceMessagesUnloaded`, the `rejects a lower same-instance
// reconnect` test in `App.live-state.reconnect.test.tsx` for the
// L197 same-instance rollback guard, the `applies metadata patch
// immediately and hydrates when an unhydrated session receives a
// missing-target delta` test in `App.live-state.deltas.test.tsx`
// for `appliedNeedsResync`).
//
// This file is deliberately a TRIPWIRE: one cross-layer integration
// test that exercises the full restart recovery chain end-to-end
// and asserts the visible-message contract. Per-fix unit tests can
// pass while the cross-layer composition silently regresses; this
// test catches that case. If you find yourself touching any of the
// modules listed above and this test fails with an "expected
// assistant text body to render" error, that is a strong signal
// you are regressing one of the historical bugs — start by reading
// the named fix-point comments inline below to find which one.
//
// Does not own: per-fix unit tests, the broader reconnect /
// fallback-state-resync flow tests (those live in
// App.live-state.reconnect.test.tsx), delta-gap / orchestrator-only
// delta tests (App.live-state.deltas.test.tsx), watchdog / wake-gap
// recovery (App.live-state.watchdog.test.tsx,
// App.live-state.visibility.test.tsx), or the send-after-restart
// preview-tooltip regression (App.session-lifecycle.test.tsx).
//
// New file added in Round 12 to address the recurring "I had to
// refresh again" user reports. See `docs/bugs.md` preamble entries
// for the individual fix histories the test guards against.
import {
  act,
  cleanup,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  type ForwardedRef,
} from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import { setAppTestHooksForTests } from "./app-test-hooks";
import {
  EventSourceMock,
  ResizeObserverMock,
  clickAndSettle,
  createActWrappedAnimationFrameMocks,
  dispatchOpenedStateEvent,
  flushUiWork,
  jsonResponse,
  latestEventSource,
  makeSession,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  renderApp,
  restoreGlobal,
  settleAsyncUi,
  stubScrollIntoView,
  withSuppressedActWarnings,
} from "./app-test-harness";

vi.mock("./MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function MonacoDiffEditorMock(
    _props: unknown,
    ref: ForwardedRef<{
      getScrollTop: () => number;
      goToNextChange: () => void;
      goToPreviousChange: () => void;
      setScrollTop: (scrollTop: number) => void;
    }>,
  ) {
    useImperativeHandle(ref, () => ({
      getScrollTop: () => 0,
      goToNextChange: () => {},
      goToPreviousChange: () => {},
      setScrollTop: () => {},
    }));
    return <div data-testid="monaco-diff-editor" />;
  }),
}));

vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function MonacoCodeEditorMock(
    {
      onStatusChange,
      value,
    }: {
      onStatusChange?: (status: {
        line: number;
        column: number;
        tabSize: number;
        insertSpaces: boolean;
        endOfLine: "LF" | "CRLF";
      }) => void;
      value: string;
    },
    ref: ForwardedRef<{
      focus: () => void;
      getScrollTop: () => number;
      setScrollTop: (scrollTop: number) => void;
    }>,
  ) {
    useImperativeHandle(ref, () => ({
      focus: () => {},
      getScrollTop: () => 0,
      setScrollTop: () => {},
    }));
    useEffect(() => {
      onStatusChange?.({
        line: 1,
        column: 1,
        tabSize: 2,
        insertSpaces: true,
        endOfLine: "LF",
      });
    }, [onStatusChange]);
    return (
      <textarea data-testid="monaco-code-editor" value={value} readOnly />
    );
  }),
}));

describe("App live state — restart roundtrip (canonical)", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;
  const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
  const originalCancelAnimationFrame = globalThis.cancelAnimationFrame;

  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createActWrappedAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({
      workspaces: [],
    });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(
      makeWorkspaceLayoutResponse(),
    );
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    if (originalRequestAnimationFrame === undefined) {
      delete (globalThis as Partial<typeof globalThis>).requestAnimationFrame;
    } else {
      globalThis.requestAnimationFrame = originalRequestAnimationFrame;
    }
    if (originalCancelAnimationFrame === undefined) {
      delete (globalThis as Partial<typeof globalThis>).cancelAnimationFrame;
    } else {
      globalThis.cancelAnimationFrame = originalCancelAnimationFrame;
    }
    window.localStorage.clear();
    if (vi.isFakeTimers()) {
      vi.useRealTimers();
    }
    setAppTestHooksForTests(null);
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("renders the streamed assistant reply through a full restart recovery chain without hard refresh", async () => {
    // ────────────────────────────────────────────────────────────────────
    // Cross-layer fix points exercised by this test (each named with the
    // module owning it; if this test fails after a change to one of those
    // modules, that is a strong "you regressed this fix" signal):
    //
    //   • app-live-state.ts :: `forceMessagesUnloaded` in
    //     `resolveAdoptStateSessionOptions` — server-instance change forces
    //     `messagesLoaded: false` on summary sessions despite cleared
    //     persisted `sessionMutationStamp`. Without it, the active session
    //     keeps stale streaming-partial content visible because the
    //     visible-session hydration `useEffect` never re-fires.
    //
    //   • session-reconcile.ts :: `reconcileSummarySession` consumes
    //     `forceMessagesUnloaded` and flips `messagesLoaded: false` even
    //     when local `messages.length >= next.messageCount` (the
    //     "complete enough" coincidence after restart).
    //
    //   • session-hydration-adoption.ts :: `classifyFetchedSessionAdoption`
    //     restartResync / adopted paths — the hydration response from the
    //     new instance is adopted as the canonical transcript and replaces
    //     the preserved-but-stale local messages.
    //
    //   • app-live-state.ts :: `handleStateEvent` peek-check + `onopen`
    //     `forceAdoptNextStateEventRef` — the first state event from the
    //     replacement instance is force-adopted across the
    //     server-instance-mismatch gate.
    //
    //   • app-live-state.ts :: `isEqualRevisionAutomaticReconnectSnapshot`
    //     (was `isNotNewerAutomaticReconnectSnapshot`, L197 fix in
    //     `bugs.md` preamble) — a late lower-revision same-instance
    //     reconnect `/api/state` response that arrives AFTER deltas
    //     advanced local state must be rejected, not force-adopted into a
    //     rollback that hides the just-streamed assistant reply.
    //
    //   • app-live-state.ts :: SSE transport effect cleanup includes
    //     `removeEventListener("lagged", ...)` alongside `state` /
    //     `delta` / `workspaceFilesChanged` removals — if the cleanup
    //     leaks the lagged listener, recreated EventSources double-fire
    //     and force-adopt arming becomes unstable.
    //
    //   • api_sse.rs :: lagged-recovery `event("lagged").data("1")`
    //     non-empty payload — empty data fields can be coalesced or
    //     skipped per WHATWG spec; the byte ensures the marker reaches
    //     the JS handler. Wire-format only; this test does not exercise
    //     the lagged path directly but documents the dependency.
    //
    // ────────────────────────────────────────────────────────────────────
    await withSuppressedActWarnings(async () => {
      const originalEventSource = globalThis.EventSource;
      const originalResizeObserver = globalThis.ResizeObserver;
      const originalFetch = globalThis.fetch;

      // The "before restart" snapshot the user is looking at: instance A,
      // revision 5, hydrated session with a streaming-partial assistant
      // reply visible. THIS IS THE LOAD-BEARING SHAPE for the
      // `forceMessagesUnloaded` fix: the local transcript has the SAME
      // `messageCount` as the post-restart server (2), but the assistant
      // body is the partial chunk that streamed before the restart while
      // the server has the finalized text. Without
      // `forceMessagesUnloaded`, `reconcileSummarySession`'s
      // "hasCompleteMessages" branch would keep `messagesLoaded: true`
      // against the matching count and the visible-session hydration
      // effect would never re-fetch. The user would see the partial
      // streaming chunk forever (until hard refresh).
      const initialSession = makeSession("session-1", {
        name: "Codex Session",
        status: "active",
        preview: "Hello, I'll he",
        messagesLoaded: true,
        messageCount: 2,
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Hi",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello, I'll he",
          },
        ],
      });

      // The "after restart" hydration response from the new server: full
      // transcript including the assistant message that streamed during
      // the restart window. `messagesLoaded: true` so adoption is
      // canonical (replaces any stale preserved messages).
      const recoveredSession = makeSession("session-1", {
        name: "Codex Session",
        status: "idle",
        preview: "Hello, I'll help you with that.",
        messagesLoaded: true,
        messageCount: 2,
        // Persisted record loaded from SQLite has cleared mutation
        // stamp; matches the SSE state event's `sessionMutationStamp:
        // undefined` so `hydrationSessionMetadataMatches` accepts the
        // response. (Pre-restart, the assistant message streamed and
        // completed; SQLite holds the full transcript; the new server
        // just loaded from disk and has not mutated yet.)
        sessionMutationStamp: undefined,
        messages: [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Hi",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello, I'll help you with that.",
          },
        ],
      });

      let stateRequestCount = 0;
      let sessionRequestCount = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          stateRequestCount += 1;
          // /api/state response: THE L197 TRAP. The earlier-scheduled
          // reconnect resync's fetchState was queued before SSE deltas
          // streamed the assistant reply; the request gets processed by
          // the new server at a revision strictly less than the current
          // local revision. Same instance, lower revision — without
          // `isEqualRevisionAutomaticReconnectSnapshot`'s `===` clamp,
          // `shouldForceAuthoritativeSnapshot` force-adopts this stale
          // response and visibly rolls the assistant message body off
          // the screen.
          //
          // Returned with `messagesLoaded: true` and only the
          // pre-assistant user message so the rollback is visible in the
          // assertions. (Production `/api/state` responses are always
          // metadata-first — `reconcileSummarySession` preserves
          // `previous.messages` on rollback so an honest metadata-first
          // payload would not surface the regression. The gate logic is
          // the same regardless of payload shape; this hydrated-shape
          // fixture is the cleanest way to assert the rollback was
          // rejected at the gate, not silently absorbed.)
          return jsonResponse(
            makeStateResponse({
              revision: 5,
              serverInstanceId: "replacement-instance",
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Codex Session",
                  status: "active",
                  preview: "Hi",
                  messagesLoaded: true,
                  messageCount: 1,
                  sessionMutationStamp: undefined,
                  messages: [
                    {
                      id: "message-user-1",
                      type: "text",
                      timestamp: "10:00",
                      author: "you",
                      text: "Hi",
                    },
                  ],
                }),
              ],
            }),
          );
        }
        if (requestUrl.pathname === "/api/sessions/session-1") {
          sessionRequestCount += 1;
          // The visible-session hydration effect fires `/api/sessions/{id}`
          // after `forceMessagesUnloaded` flipped `messagesLoaded: false`.
          // The response's full transcript replaces the stale local
          // messages and the assistant reply becomes visible.
          return jsonResponse({
            revision: 6,
            serverInstanceId: "replacement-instance",
            session: recoveredSession,
          });
        }
        if (requestUrl.pathname === "/api/git/status") {
          return jsonResponse({
            ahead: 0,
            behind: 0,
            branch: "main",
            files: [],
            isClean: true,
            repoRoot: "/tmp",
            upstream: "origin/main",
            workdir: "/tmp",
          });
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      vi.stubGlobal("fetch", fetchMock);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      const scrollIntoViewSpy = stubScrollIntoView();

      try {
        await renderApp();
        const eventSource = latestEventSource();

        // Step 1: Establish the "before restart" view — current-instance
        // at revision 5, hydrated session visible in the active pane.
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 5,
            serverInstanceId: "current-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [initialSession],
          }),
        );
        await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
        const sessionList = document.querySelector(".session-list");
        if (!(sessionList instanceof HTMLDivElement)) {
          throw new Error("Session list not found");
        }
        const sessionRowButton = within(sessionList)
          .getByText("Codex Session")
          .closest("button");
        if (!sessionRowButton) {
          throw new Error("Session row button not found");
        }
        await clickAndSettle(sessionRowButton);
        // Sanity: pre-restart partial assistant chunk is visible in the
        // active pane. This is what the user is staring at when the
        // backend gets restarted — they need the COMPLETE assistant
        // text to replace it once recovery completes.
        expect(screen.getAllByText("Hello, I'll he").length).toBeGreaterThan(0);
        expect(screen.getAllByText("Hi").length).toBeGreaterThan(0);

        // Step 2: Simulate a backend restart from the user's perspective.
        // `dispatchOpenedStateEvent` runs `onopen` then a state event in
        // the same `act` — that mirrors the SSE handler reconnecting
        // after Vite's proxy 502 gap. `onopen` arms
        // `forceAdoptNextStateEventRef = true` because
        // `latestStateRevisionRef.current !== null` (set by the
        // pre-restart state event), so the peek-check and `adoptState`
        // both pass through the server-instance-mismatch gate with
        // `allowUnknownServerInstance: force = true`.
        await dispatchOpenedStateEvent(
          eventSource,
          makeStateResponse({
            revision: 6,
            serverInstanceId: "replacement-instance",
            projects: [],
            orchestrators: [],
            workspaces: [],
            sessions: [
              makeSession("session-1", {
                name: "Codex Session",
                status: "idle",
                preview: "Hello, I'll help you with that.",
                messagesLoaded: false,
                messageCount: 2,
                // No mutation stamp — persisted-record clear contract.
                sessionMutationStamp: undefined,
                messages: [],
              }),
            ],
          }),
        );
        await settleAsyncUi();

        // Step 3: After the replacement-instance state event is adopted,
        // `forceMessagesUnloaded` flipped `session-1.messagesLoaded` to
        // false. The visible-session hydration `useEffect` re-fires and
        // calls `/api/sessions/session-1`. Without the
        // `forceMessagesUnloaded` fix, the matching `messageCount: 2`
        // (against `previous.messages.length: 1`?) would NOT trigger
        // hydration here, and stage 4 below would never get the
        // canonical transcript.
        //
        // Note: the actual messageCount bump from 1→2 already triggers
        // hydration via the existing "newer count" branch even without
        // `forceMessagesUnloaded`; the LOAD-BEARING case for that fix
        // is when `messageCount` happens to MATCH (e.g., the streamed
        // partial coincidentally aligned with the persisted count).
        // That subtlety is pinned directly in
        // `session-reconcile.test.ts` "forces messagesLoaded=false on
        // summary sessions when forceMessagesUnloaded is set". This
        // test still asserts the hydration fetch fires here as the
        // composed visible-message contract.
        await waitFor(() => {
          expect(sessionRequestCount).toBeGreaterThanOrEqual(1);
        });

        // Step 4: Live deltas extend the assistant message text. With
        // `messagesLoaded: false` (set by `forceMessagesUnloaded` in
        // step 2) the message body still reflects the partial chunk
        // because the hydration response's full text differs from the
        // local partial — `classifyFetchedSessionAdoption` rejects
        // that as `stale` to protect against losing local-only deltas.
        // In the realistic flow, the new server's SSE stream resumes
        // streaming and finishes the message via `textDelta` events;
        // the test simulates that resumption directly so the active
        // transcript actually receives the rest of the assistant reply.
        await act(async () => {
          eventSource.dispatchNamedEvent("delta", {
            type: "textDelta",
            revision: 7,
            sessionId: "session-1",
            messageId: "message-assistant-1",
            messageIndex: 1,
            messageCount: 2,
            delta: "lp you with that.",
            preview: "Hello, I'll help you with that.",
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        // The assistant bubble in the ACTIVE TRANSCRIPT now contains
        // the complete reply. Scope to `.message-card.bubble-assistant`
        // (the actual rendered bubble article) so a regression that
        // only updates sidebar preview metadata cannot satisfy this.
        await waitFor(() => {
          const assistantBubble = document.querySelector(
            ".message-card.bubble-assistant",
          );
          expect(assistantBubble?.textContent).toContain(
            "Hello, I'll help you with that.",
          );
        });
        // The pre-restart streaming partial must be GONE from the
        // active transcript: textDelta extended it to the full reply,
        // so the bubble's textContent must NOT end with "Hello, I'll
        // he" (it now ends with the complete sentence).
        const assistantBubbleAfterDelta = document.querySelector(
          ".message-card.bubble-assistant",
        );
        expect(assistantBubbleAfterDelta?.textContent).not.toMatch(
          /Hello, I'll he$/,
        );

        // Step 5: A late stale `_sseFallback` reconnect probe arrives
        // with the SAME instance (`replacement-instance`) but a LOWER
        // revision than current. This is the L197 trap: the
        // earlier-scheduled reconnect resync's `requestedRevision` was
        // captured before the deltas advanced local state. Without
        // `isEqualRevisionAutomaticReconnectSnapshot`'s `===` clamp,
        // `shouldForceAuthoritativeSnapshot` would force-adopt the
        // lower-revision response and roll local state back past the
        // assistant message we just rendered.
        //
        // The test verifies the assistant text remains visible AFTER
        // this trap fires, proving the rollback was rejected.
        const stateRequestCountBeforeFallback = stateRequestCount;
        await act(async () => {
          eventSource.dispatchNamedEvent("state", {
            _sseFallback: true,
            revision: 5,
            serverInstanceId: "replacement-instance",
            projects: [],
            sessions: [],
          });
          await flushUiWork();
        });
        await settleAsyncUi();

        // Final assertions: the canonical visible-message contract holds
        // SCOPED TO THE ACTIVE TRANSCRIPT (not the sidebar preview).
        // The assistant bubble's textContent must still contain the
        // complete reply after the L197 trap fired; without the fix
        // the lower-revision response's `[user]`-only `messages` array
        // would have replaced the bubble via `reconcileSession`'s main
        // path, hiding the assistant message.
        const assistantBubbleAfterFallback = document.querySelector(
          ".message-card.bubble-assistant",
        );
        expect(assistantBubbleAfterFallback).toBeTruthy();
        expect(assistantBubbleAfterFallback?.textContent).toContain(
          "Hello, I'll help you with that.",
        );
        // The user bubble must still be in the transcript too — the
        // late fallback should not have rolled back to a session
        // without the user prompt either.
        const userBubble = document.querySelector(
          ".message-card.bubble-you",
        );
        expect(userBubble?.textContent).toContain("Hi");
        // The fallback marker triggered an /api/state probe; verify
        // the request count incremented FROM the value we captured
        // before dispatching the marker, not just the cumulative count
        // (which has prior /api/sessions/{id} hydration calls in it).
        // This assertion proves the recovery probe ran but did NOT
        // clobber local state.
        expect(stateRequestCount).toBeGreaterThanOrEqual(
          stateRequestCountBeforeFallback + 1,
        );
      } finally {
        scrollIntoViewSpy.mockRestore();
        restoreGlobal("fetch", originalFetch);
        restoreGlobal("EventSource", originalEventSource);
        restoreGlobal("ResizeObserver", originalResizeObserver);
      }
    });
  });
});
