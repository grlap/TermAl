import { describe, expect, it } from "vitest";

import {
  hydrationRetainedMessagesMatch,
  hydrationSessionMetadataMatches,
  resolveAdoptStateSessionOptions,
} from "./app-live-state";
import type { Message, Session } from "./types";

describe("resolveAdoptStateSessionOptions", () => {
  it("preserves an explicit mutation-stamp fast-path disable without a server instance change", () => {
    expect(
      resolveAdoptStateSessionOptions(
        { disableMutationStampFastPath: true },
        false,
      ).disableMutationStampFastPath,
    ).toBe(true);
  });

  it("disables the mutation-stamp fast path when the server instance changes", () => {
    expect(
      resolveAdoptStateSessionOptions(
        { disableMutationStampFastPath: false },
        true,
      ).disableMutationStampFastPath,
    ).toBe(true);
  });

  it("keeps the mutation-stamp fast path enabled by default", () => {
    expect(
      resolveAdoptStateSessionOptions(undefined, false)
        .disableMutationStampFastPath,
    ).toBe(false);
  });
});

describe("hydrationRetainedMessagesMatch", () => {
  const message: Message = {
    id: "message-1",
    type: "text",
    author: "assistant",
    timestamp: "10:00",
    text: "Hello",
  };

  it("matches structurally identical retained messages", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message }] },
        { messages: [{ ...message }] },
      ),
    ).toBe(true);
  });

  it("treats either empty side as retainable", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [] },
        { messages: [message] },
      ),
    ).toBe(true);
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [message] },
        { messages: [] },
      ),
    ).toBe(true);
  });

  it("rejects non-empty message shape mismatches", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message, text: "Hello" }] },
        { messages: [{ ...message, text: "Goodbye" }] },
      ),
    ).toBe(false);
  });

  it("rejects extra client-side fields on retained messages", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [message] },
        { messages: [{ ...message, localRenderCache: true } as Message] },
      ),
    ).toBe(false);
  });
});

describe("hydrationSessionMetadataMatches", () => {
  const baseSession: Session = {
    id: "session-1",
    name: "Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "idle",
    preview: "",
    messages: [],
    messagesLoaded: false,
  };

  it("rejects numeric response metadata when the current session captured null", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messageCount: 3,
          sessionMutationStamp: 7,
        },
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
      ),
    ).toBe(false);
  });

  it("treats null metadata as an exact value rather than a wildcard", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
      ),
    ).toBe(true);
  });

  it("falls back to loaded message length when messageCount is absent", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messages: [
            {
              id: "m1",
              type: "text",
              author: "assistant",
              timestamp: "10:00",
              text: "One",
            },
          ],
          messagesLoaded: true,
          sessionMutationStamp: 1,
        },
        {
          ...baseSession,
          messageCount: 1,
          messages: [],
          messagesLoaded: false,
          sessionMutationStamp: 1,
        },
      ),
    ).toBe(true);
  });
});
