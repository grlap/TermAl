import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AgentSessionPanel } from "./AgentSessionPanel";
import * as conversationMarkers from "./conversation-markers";
import * as historyDemand from "../session-history-demand";
import {
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
} from "../session-store";
import type { Session } from "../types";

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
