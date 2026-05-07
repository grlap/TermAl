import { describe, expect, it } from "vitest";

import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  buildSessionListSearchResult,
  buildSessionSearchMatches,
  buildSessionSearchMatchesFromIndex,
  sessionSearchItemKey,
} from "./session-find";
import type { Session } from "./types";

function createSession(overrides?: Partial<Session>): Session {
  return {
    id: "session-1",
    name: "Search Session",
    emoji: "S",
    agent: "Codex",
    workdir: "/repo",
    model: "gpt-5",
    status: "idle",
    preview: "Preview",
    messages: [],
    ...overrides,
  };
}

describe("session find helpers", () => {
  it("finds matches across mixed session message content in conversation order", () => {
    const session = createSession({
      messages: [
        {
          id: "message-text",
          type: "text",
          author: "assistant",
          timestamp: "09:00",
          text: "Search the repo for the widget controller.",
        },
        {
          id: "message-command",
          type: "command",
          author: "assistant",
          timestamp: "09:01",
          command: "rg -n widget ui/src",
          output: "ui/src/App.tsx:42: const widgetController = true;",
          status: "success",
        },
        {
          id: "message-diff",
          type: "diff",
          author: "assistant",
          timestamp: "09:02",
          filePath: "ui/src/widget.ts",
          summary: "Add widget search wiring",
          diff: "+ export const widgetController = createController();",
          changeType: "edit",
        },
      ],
      pendingPrompts: [
        {
          id: "prompt-1",
          timestamp: "09:03",
          text: "Double-check the widget controller edge cases.",
          attachments: [
            {
              fileName: "widget-screenshot.png",
              mediaType: "image/png",
              byteSize: 128,
            },
          ],
        },
      ],
    });

    expect(buildSessionSearchMatches(session, "widget")).toEqual([
      expect.objectContaining({
        itemId: "message-text",
        itemKey: sessionSearchItemKey("message", "message-text"),
        itemKind: "message",
      }),
      expect.objectContaining({
        itemId: "message-command",
        itemKey: sessionSearchItemKey("message", "message-command"),
        itemKind: "message",
      }),
      expect.objectContaining({
        itemId: "message-diff",
        itemKey: sessionSearchItemKey("message", "message-diff"),
        itemKind: "message",
      }),
      expect.objectContaining({
        itemId: "prompt-1",
        itemKey: sessionSearchItemKey("pendingPrompt", "prompt-1"),
        itemKind: "pendingPrompt",
      }),
    ]);
  });

  it("indexes subagent result messages for conversation search", () => {
    const session = createSession({
      messages: [
        {
          id: "message-subagent",
          type: "subagentResult",
          author: "assistant",
          timestamp: "09:00",
          title: "Subagent completed",
          summary: "Reviewer found a batching bug in location smoothing.",
          conversationId: "conversation-123",
          turnId: "turn-sub-1",
        },
      ],
    });

    expect(buildSessionSearchMatches(session, "batching")).toEqual([
      expect.objectContaining({
        itemId: "message-subagent",
        itemKey: sessionSearchItemKey("message", "message-subagent"),
        itemKind: "message",
      }),
    ]);
    expect(buildSessionSearchMatches(session, "turn-sub-1")).toHaveLength(1);
  });

  it("indexes parallel agent messages for conversation search", () => {
    const session = createSession({
      messages: [
        {
          id: "message-parallel",
          type: "parallelAgents",
          author: "assistant",
          timestamp: "09:00",
          agents: [
            {
              id: "task-1",
              source: "tool",
              title: "Rust code review",
              status: "completed",
              detail: "Reviewer found a batching bug in location smoothing.",
            },
            {
              id: "task-2",
              source: "tool",
              title: "Architecture code review",
              status: "initializing",
              detail: "Initializing...",
            },
          ],
        },
      ],
    });

    expect(buildSessionSearchMatches(session, "batching")).toEqual([
      expect.objectContaining({
        itemId: "message-parallel",
        itemKey: sessionSearchItemKey("message", "message-parallel"),
        itemKind: "message",
      }),
    ]);
    expect(buildSessionSearchMatches(session, "Architecture")).toHaveLength(1);
  });
  it("returns no results for blank queries", () => {
    const session = createSession({
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          timestamp: "09:00",
          text: "Nothing to see here.",
        },
      ],
    });

    expect(buildSessionSearchMatches(session, "")).toEqual([]);
    expect(buildSessionSearchMatches(session, "   ")).toEqual([]);
  });

  it("includes attachment metadata and builds compact snippets", () => {
    const session = createSession({
      pendingPrompts: [
        {
          id: "prompt-1",
          timestamp: "09:03",
          text: "",
          attachments: [
            {
              fileName: "error-shot.webp",
              mediaType: "image/webp",
              byteSize: 256,
            },
          ],
        },
      ],
    });

    expect(buildSessionSearchMatches(session, "webp")).toEqual([
      expect.objectContaining({
        itemId: "prompt-1",
        itemKey: sessionSearchItemKey("pendingPrompt", "prompt-1"),
        snippet: "error-shot.webp image/webp",
      }),
    ]);
  });

  it("builds cross-session search summaries from metadata or conversation hits", () => {
    const metadataSession = createSession({
      name: "Release automation",
      preview: "Queued",
    });
    const conversationSession = createSession({
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          timestamp: "09:00",
          text: "Investigate the flaky deploy gate in CI.",
        },
      ],
    });

    expect(buildSessionListSearchResult(metadataSession, "release")).toEqual({
      matchCount: 1,
      snippet: "Release automation",
    });
    expect(buildSessionListSearchResult(conversationSession, "deploy")).toEqual({
      matchCount: 1,
      snippet: "Investigate the flaky deploy gate in CI.",
    });
    expect(buildSessionListSearchResult(conversationSession, "missing")).toBeNull();
  });

  describe("indexes rendered connection-retry notice state", () => {
    const retryText =
      "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).";

    it("indexes a live retry notice as active retry copy", () => {
      const liveSession = createSession({
        status: "active",
        messages: [
          {
            id: "message-retry-live",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
        ],
      });

      expect(buildSessionSearchMatches(liveSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-live" }),
      ]);
      expect(buildSessionSearchMatches(liveSession, "Connection recovered")).toEqual([]);
    });

    it("indexes an inactive retry notice as ended plus literal stored copy", () => {
      const inactiveSession = createSession({
        status: "idle",
        messages: [
          {
            id: "message-retry-inactive",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
        ],
      });

      expect(buildSessionSearchMatches(inactiveSession, "Connection retry ended")).toEqual([
        expect.objectContaining({ itemId: "message-retry-inactive" }),
      ]);
      expect(buildSessionSearchMatches(inactiveSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-inactive" }),
      ]);
    });

    it("indexes a resolved retry notice as recovered copy", () => {
      const resolvedSession = createSession({
        status: "idle",
        messages: [
          {
            id: "message-retry-resolved",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
          {
            id: "message-response",
            type: "text",
            author: "assistant",
            timestamp: "09:01",
            text: "The turn continued after reconnecting.",
          },
        ],
      });

      expect(buildSessionSearchMatches(resolvedSession, "Connection recovered")).toEqual([
        expect.objectContaining({ itemId: "message-retry-resolved" }),
      ]);
      expect(
        buildSessionSearchMatches(resolvedSession, "the turn continued after attempt 2 of 5"),
      ).toEqual([expect.objectContaining({ itemId: "message-retry-resolved" })]);
      expect(buildSessionSearchMatches(resolvedSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-resolved" }),
      ]);
      expect(buildSessionSearchMatches(resolvedSession, "Connection retry ended")).toEqual([]);
    });

    it("indexes an older retry notice as superseded when a newer retry is active", () => {
      const supersededSession = createSession({
        status: "active",
        messages: [
          {
            id: "message-retry-superseded",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
          {
            id: "message-retry-newer",
            type: "text",
            author: "assistant",
            timestamp: "09:01",
            text: "Connection dropped before the response finished. Retrying automatically (attempt 3 of 5).",
          },
        ],
      });

      expect(buildSessionSearchMatches(supersededSession, "Retry superseded")).toEqual([
        expect.objectContaining({ itemId: "message-retry-superseded" }),
      ]);
      expect(
        buildSessionSearchMatches(supersededSession, "Reconnecting to continue this turn"),
      ).toEqual([expect.objectContaining({ itemId: "message-retry-newer" })]);
      expect(buildSessionSearchMatches(supersededSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-superseded" }),
        expect.objectContaining({ itemId: "message-retry-newer" }),
      ]);
    });

    it("indexes retry-then-resolved sequences with superseded and recovered states", () => {
      const retryThenResolvedSession = createSession({
        status: "idle",
        messages: [
          {
            id: "message-retry-first",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
          {
            id: "message-retry-second",
            type: "text",
            author: "assistant",
            timestamp: "09:01",
            text: "Connection dropped before the response finished. Retrying automatically (attempt 3 of 5).",
          },
          {
            id: "message-response-after-retries",
            type: "text",
            author: "assistant",
            timestamp: "09:02",
            text: "The turn recovered after the second retry.",
          },
        ],
      });

      expect(buildSessionSearchMatches(retryThenResolvedSession, "Retry superseded")).toEqual([
        expect.objectContaining({ itemId: "message-retry-first" }),
      ]);
      expect(buildSessionSearchMatches(retryThenResolvedSession, "Connection recovered")).toEqual([
        expect.objectContaining({ itemId: "message-retry-second" }),
      ]);
      expect(buildSessionSearchMatches(retryThenResolvedSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-first" }),
        expect.objectContaining({ itemId: "message-retry-second" }),
      ]);
    });

    it("indexes a retry before a new user prompt as inactive", () => {
      const newPromptSession = createSession({
        status: "active",
        messages: [
          {
            id: "message-retry-before-user",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
          {
            id: "message-new-user-prompt",
            type: "text",
            author: "you",
            timestamp: "09:01",
            text: "Try a different task.",
          },
        ],
      });

      expect(buildSessionSearchMatches(newPromptSession, "Connection retry ended")).toEqual([
        expect.objectContaining({ itemId: "message-retry-before-user" }),
      ]);
      expect(
        buildSessionSearchMatches(newPromptSession, "Reconnecting to continue this turn"),
      ).toEqual([]);
      expect(buildSessionSearchMatches(newPromptSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-before-user" }),
      ]);
    });

    it("indexes interleaved recovered and current retry notices independently", () => {
      const interleavedSession = createSession({
        status: "active",
        messages: [
          {
            id: "message-retry-old",
            type: "text",
            author: "assistant",
            timestamp: "09:00",
            text: retryText,
          },
          {
            id: "message-response-between",
            type: "text",
            author: "assistant",
            timestamp: "09:01",
            text: "Recovered before another reconnect attempt.",
          },
          {
            id: "message-retry-current",
            type: "text",
            author: "assistant",
            timestamp: "09:02",
            text: "Connection dropped before the response finished. Retrying automatically (attempt 3 of 5).",
          },
        ],
      });

      expect(buildSessionSearchMatches(interleavedSession, "Connection recovered")).toEqual([
        expect.objectContaining({ itemId: "message-retry-old" }),
      ]);
      expect(
        buildSessionSearchMatches(interleavedSession, "Reconnecting to continue this turn"),
      ).toEqual([expect.objectContaining({ itemId: "message-retry-current" })]);
      expect(buildSessionSearchMatches(interleavedSession, "Retrying automatically")).toEqual([
        expect.objectContaining({ itemId: "message-retry-old" }),
        expect.objectContaining({ itemId: "message-retry-current" }),
      ]);
    });
  });
  it("reuses prebuilt indexes for conversation and session-list search", () => {
    const session = createSession({
      name: "Deploy checks",
      messages: [
        {
          id: "message-1",
          type: "text",
          author: "assistant",
          timestamp: "09:00",
          text: "Track the deploy blocker in staging.",
        },
      ],
    });
    const searchIndex = buildSessionSearchIndex(session);

    expect(buildSessionSearchMatchesFromIndex(searchIndex, "deploy")).toEqual(
      buildSessionSearchMatches(session, "deploy"),
    );
    expect(buildSessionListSearchResultFromIndex(searchIndex, "checks")).toEqual(
      buildSessionListSearchResult(session, "checks"),
    );
  });
});
