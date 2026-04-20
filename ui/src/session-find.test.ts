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
              title: "Rust code review",
              status: "completed",
              detail: "Reviewer found a batching bug in location smoothing.",
            },
            {
              id: "task-2",
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

  it("indexes connection-retry notices so both live and resolved card copy match", () => {
    // Regression guard for the "resolved retry notice text
    // diverges from session find indexing" bug. A
    // `ConnectionRetryCard` renders two distinct text
    // surfaces depending on whether the turn has resumed:
    //   - Live      → `notice.detail` (the stored
    //                 `message.text`).
    //   - Resolved  → a synthesized "Connection recovered" /
    //                 "Connection dropped briefly; the turn
    //                 continued after …" heading + detail.
    // Before the fix, the search index only carried
    // `message.text`, so a search for terms visible ONLY in the
    // resolved card (e.g. "Connection recovered", "the turn
    // continued") missed the message — and a search for
    // live-card terms ("Retrying automatically") landed on a
    // resolved card whose visible copy didn't contain the
    // query, making search look broken. Include both forms in
    // the search index for connection-retry notices.
    const session = createSession({
      messages: [
        {
          id: "message-retry",
          type: "text",
          author: "assistant",
          timestamp: "09:00",
          text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
        },
      ],
    });
    // Live-card detail text (the raw stored message text).
    expect(buildSessionSearchMatches(session, "Retrying automatically")).toEqual([
      expect.objectContaining({ itemId: "message-retry" }),
    ]);
    // Resolved-card heading.
    expect(buildSessionSearchMatches(session, "Connection recovered")).toEqual([
      expect.objectContaining({ itemId: "message-retry" }),
    ]);
    // Resolved-card synthesized detail (past-tense, with the
    // attempt label lowercased to match the rendered copy).
    expect(
      buildSessionSearchMatches(session, "the turn continued after attempt 2 of 5"),
    ).toEqual([expect.objectContaining({ itemId: "message-retry" })]);
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
