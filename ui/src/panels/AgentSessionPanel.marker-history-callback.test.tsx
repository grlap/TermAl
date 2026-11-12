import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AgentSessionPanel } from "./AgentSessionPanel";
import * as conversationMarkers from "./conversation-markers";
import * as historyDemand from "../session-history-demand";
import * as api from "../api";
import { installAnimationFrameHarness } from "../SessionPaneView.scroll.fixtures";
import { loadBoundedSessionHistoryWindow, type SessionHistoryLoadingContext } from "../session-history-loading";
import {
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
} from "../session-store";
import type { Session } from "../types";
import type { VirtualizedConversationMessageListHandleRef } from "./virtualized-conversation-types";

function makeSession(id: string): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/project",
    model: "test-model",
    status: "idle",
    preview: "",
    messages: [
      {
        id: `${id}-message`,
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "A resident message",
      },
    ],
  };
}

function setSessions(sessions: Session[]) {
  syncComposerSessionsStore({
    sessions,
    draftsBySessionId: {},
    draftAttachmentsBySessionId: {},
  });
}

afterEach(() => {
  cleanup();
  resetSessionStoreForTesting();
  vi.restoreAllMocks();
});

describe("AgentSessionPanel centered-history callback", () => {
  it("clicks an unloaded checkpoint dot, adopts its bounded page, and navigates to its message", async () => {
    const { drainAnimationFrames } = installAnimationFrameHarness(1_000 / 60);
    let current: Session = {
      ...makeSession("checkpoint-session"), messageCount: 1_000, messageStartIndex: 999,
      hasOlderHistory: true, hasNewerHistory: false, sessionMutationStamp: 1,
      markers: [{ id: "early-checkpoint", name: "Earlier checkpoint", kind: "checkpoint", sessionId: "checkpoint-session",
        messageId: "checkpoint-message", messageIndexHint: 20, color: "#888888", createdAt: "2026-01-01", updatedAt: "2026-01-01", createdBy: "user" }],
    };
    const page = {
      sessionId: current.id, messages: [{ ...current.messages[0], id: "checkpoint-message" }],
      messageCount: 1_000, messageStartIndex: 20, hasMore: true, hasNewer: true,
      nextBefore: "checkpoint-message", nextAfter: "checkpoint-message",
      revision: 1, serverInstanceId: "server-a", sessionMutationStamp: 1,
    };
    const fetch = vi.spyOn(api, "fetchSessionHistory").mockResolvedValue(page);
    const loads: Promise<void>[] = [];
    const context: SessionHistoryLoadingContext = {
      getSession: () => current, getLastSeenServerInstanceId: () => "server-a", inFlightOlderLoads: new Map(), isMounted: () => true,
      publishSession: next => { current = next; setSessions([next]); return true; },
      reportRequestError: vi.fn(), requestActionRecoveryResync: vi.fn(),
    };
    const remove = historyDemand.addSessionHistoryPageDemandListener(demand => { loads.push(loadBoundedSessionHistoryWindow({ context, demand })); });
    const node = document.createElement("section");
    Object.defineProperties(node, { clientHeight: { value: 600 }, clientWidth: { value: 600 }, scrollHeight: { value: 200_000 } });
    const scrollContainerRef = { current: node };
    const handle: VirtualizedConversationMessageListHandleRef = { current: null };
    setSessions([current]);
    const view = render(<AgentSessionPanel paneId="checkpoint-pane" viewMode="session" activeSessionId={current.id}
      isLoading={false} isUpdating={false} commandMessages={[]} diffMessages={[]} scrollContainerRef={scrollContainerRef} virtualizerHandleRef={handle}
      onApprovalDecision={() => {}} onUserInputSubmit={async () => {}} onMcpElicitationSubmit={() => {}} onCodexAppRequestSubmit={() => {}}
      onCancelQueuedPrompt={() => {}} onSessionSettingsChange={() => {}} conversationSearchQuery="" conversationSearchMatchedItemKeys={new Set()}
      conversationSearchActiveItemKey={null} onConversationSearchItemMount={() => {}}
      renderCommandCard={() => null} renderDiffCard={() => null} renderMessageCard={message => <article>{message.id}</article>} renderPromptSettings={() => null} />);
    try {
      expect(screen.queryByText("checkpoint-message")).toBeNull();
      const jump = vi.spyOn(handle.current!, "jumpToMessageId");
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Go to checkpoint Earlier checkpoint" }));
        await Promise.all(loads);
      });
      act(() => { drainAnimationFrames(); });
      expect(fetch).toHaveBeenCalledWith(current.id, expect.objectContaining({ around: 20 }));
      expect(current.messageStartIndex).toBe(20);
      expect(screen.getByText("checkpoint-message")).toBeInTheDocument();
      expect(jump).toHaveBeenCalledWith("checkpoint-message", expect.objectContaining({ align: "center" }));
      expect(context.reportRequestError).not.toHaveBeenCalled();
    } finally { remove(); view.unmount(); }
  });
  it("keeps the actual callback across same-session rerenders and targets a switched session", async () => {
    // Observe the production hook input while keeping its real implementation.
    const jump = vi.spyOn(conversationMarkers, "useConversationMarkerJump");
    const request = vi
      .spyOn(historyDemand, "requestSessionHistoryAroundPage")
      .mockResolvedValue(true);
    const first = makeSession("session-a");
    const second = makeSession("session-b");
    setSessions([first, second]);
    const scrollContainerRef = { current: document.createElement("section") };
    const matchedItemKeys = new Set<string>();
    const panel = (sessionId: string, query = "") => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={sessionId}
        isLoading={false}
        isUpdating={false}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={scrollContainerRef}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onCancelQueuedPrompt={() => {}}
        onSessionSettingsChange={() => {}}
        conversationSearchQuery={query}
        conversationSearchMatchedItemKeys={matchedItemKeys}
        conversationSearchActiveItemKey={null}
        onConversationSearchItemMount={() => {}}
        renderCommandCard={() => null}
        renderDiffCard={() => null}
        renderMessageCard={(message) => <article>{message.id}</article>}
        renderPromptSettings={() => null}
      />
    );
    const callbackSince = (callIndex: number, sessionId: string) => {
      const calls = jump.mock.calls
        .slice(callIndex)
        .filter(([input]) => input.sessionId === sessionId);
      // An unchanged reference is meaningful only if the producer rendered again.
      expect(calls.length).toBeGreaterThan(0);
      const callback = calls[calls.length - 1][0].requestMarkerHistoryAround;
      expect(callback).toBeTypeOf("function");
      return callback!;
    };

    const { rerender } = render(panel(first.id));
    const initial = callbackSince(0, first.id);

    let before = jump.mock.calls.length;
    rerender(panel(first.id, "resident"));
    expect(callbackSince(before, first.id)).toBe(initial);

    before = jump.mock.calls.length;
    act(() => {
      setSessions([{ ...first, preview: "Updated same-session record" }, second]);
    });
    expect(callbackSince(before, first.id)).toBe(initial);

    before = jump.mock.calls.length;
    rerender(panel(second.id));
    const switched = callbackSince(before, second.id);
    expect(switched).not.toBe(initial);

    request.mockClear();
    await expect(initial(120)).resolves.toBe(true);
    await expect(switched(240)).resolves.toBe(true);
    expect(request.mock.calls).toEqual([[first.id, 120], [second.id, 240]]);
  });
});
