// Identical public-App discriminator on master and the sequence candidate.
// An older peer has no sequence fields; lagged must retain master's behaviour.
import { act, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "./api";
import * as live from "./app-live-state";
import * as actions from "./app-session-actions";
import { getSessionRecordSnapshotForTesting } from "./session-store";
import type { Session, TextMessage } from "./types";
import { createScheduledAnimationFrameMocks, EventSourceMock, flushUiWork,
  latestEventSource, makeStateResponse, makeStateSessionSummary, makeWorkspaceLayoutResponse, renderAppWithProjectAndSession,
  settleAsyncUi, withVerifiedNoReactActWarnings } from "./app-test-harness";

const message = (text: string): TextMessage => ({
  id: "legacy-message", type: "text", author: "assistant", timestamp: "10:00", text,
});
function legacy(text: string): Session {
  return { id: "session-1", name: "Legacy peer", emoji: "AI", agent: "Codex",
    workdir: "/projects/termal", projectId: "project-termal", model: "default",
    status: "active", preview: "", sessionMutationStamp: 2, messageCount: 1,
    queuePaused: false, messagesLoaded: true, messages: [message(text)] };
}
beforeEach(() => {
  const frames = createScheduledAnimationFrameMocks();
  vi.stubGlobal("requestAnimationFrame", frames.requestAnimationFrameMock);
  vi.stubGlobal("cancelAnimationFrame", frames.cancelAnimationFrameMock);
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
});
it("leaves a peer without body sequences exactly as master across lagged and ordinary publications", async () => {
  await withVerifiedNoReactActWarnings(async () => {
    let liveParams!: Parameters<typeof live.useAppLiveState>[0];
    let result!: ReturnType<typeof live.useAppLiveState>;
    let actionParams!: Parameters<typeof actions.useAppSessionActions>[0];
    const realLive = live.useAppLiveState;
    const realActions = actions.useAppSessionActions;
    vi.spyOn(live, "useAppLiveState").mockImplementation(params => {
      liveParams = params; result = realLive(params); return result;
    });
    vi.spyOn(actions, "useAppSessionActions").mockImplementation(params => {
      actionParams = params; return realActions(params);
    });
    const context = await renderAppWithProjectAndSession();
    const supportingSlices = {
      projects: liveParams.adoptionRefs.projectsRef.current,
      workspaces: liveParams.adoptionRefs.workspaceSummariesRef.current,
      orchestrators: liveParams.adoptionRefs.orchestratorsRef.current,
    };
    const assertBodies = (text: string) => {
      const projections = [
        liveParams.adoptionRefs.sessionsRef.current.find(s => s.id === "session-1")!,
        getSessionRecordSnapshotForTesting("session-1")!,
        actionParams.lookups.sessionLookup.get("session-1")!,
      ];
      for (const session of projections) {
        expect(session.bodySeq).toBeUndefined();
        expect(session.messages).toEqual([message(text)]);
        expect(session.messagesLoaded).toBe(true);
      }
      expect(document.querySelector('[data-message-id="legacy-message"]')).toHaveTextContent(text);
    };
    try {
      act(() => { result.adoptState(makeStateResponse({ revision: 2,
        serverInstanceId: "test-instance", sessions: [makeStateSessionSummary(legacy(""))],
        ...supportingSlices }), { force: true }); });
      await act(async () => {
        expect(result.adoptCreatedSessionResponse({ sessionId: "session-1",
          revision: 3, serverInstanceId: "test-instance", session: legacy("Legacy body") })).toBe("adopted");
        await flushUiWork();
      });
      await settleAsyncUi();
      assertBodies("Legacy body");
      vi.spyOn(api, "fetchSessionTail").mockReturnValue(new Promise(() => {}));
      act(() => { latestEventSource().dispatchNamedEvent("lagged", "1"); });
      await settleAsyncUi();
      assertBodies("Legacy body");
      act(() => { latestEventSource().dispatchNamedEvent("state", JSON.stringify(makeStateResponse({
        revision: 4, serverInstanceId: "test-instance", sessions: [makeStateSessionSummary(legacy(""))],
        ...supportingSlices,
      }))); });
      await settleAsyncUi();
      assertBodies("Legacy body");
      act(() => { latestEventSource().dispatchNamedEvent("delta", JSON.stringify({
        type: "textDelta", revision: 5, sessionId: "session-1", messageId: "legacy-message",
        messageIndex: 0, messageCount: 1, textStartByte: 11, delta: " stays", sessionMutationStamp: 3,
      })); });
      await settleAsyncUi();
      assertBodies("Legacy body stays");
      await act(async () => {
        expect(result.adoptCreatedSessionResponse({ sessionId: "session-1", revision: 6,
          serverInstanceId: "test-instance", session: { ...legacy("Targeted legacy body"), sessionMutationStamp: 4 } })).toBe("adopted");
        await flushUiWork();
      });
      await settleAsyncUi();
      assertBodies("Targeted legacy body");
    } finally { context.cleanup(); }
  });
});
