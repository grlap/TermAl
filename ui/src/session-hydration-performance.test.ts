import { afterEach, describe, expect, it, vi } from "vitest";

import {
  __resetSessionHydrationPerformanceForTests,
  noteSessionTailAdopted,
  noteSessionTranscriptCommitted,
  sessionTranscriptCommitToken,
} from "./session-hydration-performance";
import type { Message, Session } from "./types";

function message(id: string, text = id): Message {
  return {
    id,
    type: "text",
    timestamp: "12:00",
    author: "assistant",
    text,
  };
}

function session(id: string, messages: Message[]): Session {
  return { id, messages } as Session;
}

describe("session hydration performance diagnostics", () => {
  afterEach(() => {
    __resetSessionHydrationPerformanceForTests();
    vi.restoreAllMocks();
  });

  it("stays silent for a prompt transcript commit", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const adoptedSession = session("session-fast", [message("tail")]);
    const token = noteSessionTailAdopted(adoptedSession, 1_000);

    expect(
      noteSessionTranscriptCommitted("session-fast", token, 1, 1_011),
    ).toBe(11);
    expect(warn).not.toHaveBeenCalled();
  });

  it("reports and consumes a delayed transcript commit", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const adoptedSession = session("session-slow", [message("tail")]);
    const token = noteSessionTailAdopted(adoptedSession, 1_000);

    expect(
      noteSessionTranscriptCommitted("session-slow", token, 1, 6_200, (text) =>
        warn(text),
      ),
    ).toBe(5_200);
    expect(warn).toHaveBeenCalledWith(
      "session hydration> transcript commit delayed 5200ms after tail adoption for `session-slow` (1 messages)",
    );
    expect(
      noteSessionTranscriptCommitted("session-slow", token, 1, 7_000),
    ).toBeNull();
  });

  it("matches the exact adopted session generation, not sampled message content", () => {
    const firstAdoption = session("session-replaced", [
      message("shared-first"),
      message("changed-interior", "first version"),
      message("shared-middle"),
      message("shared-last"),
    ]);
    const replacementAdoption = session("session-replaced", [
      message("shared-first"),
      message("changed-interior", "second version"),
      message("shared-middle"),
      message("shared-last"),
    ]);
    const firstToken = noteSessionTailAdopted(firstAdoption, 1_000);
    const replacementToken = noteSessionTailAdopted(replacementAdoption, 1_050);

    expect(sessionTranscriptCommitToken(firstAdoption)).toBe(firstToken);
    expect(sessionTranscriptCommitToken(replacementAdoption)).toBe(
      replacementToken,
    );
    expect(
      noteSessionTranscriptCommitted("session-replaced", firstToken, 4, 1_100),
    ).toBeNull();
    expect(
      noteSessionTranscriptCommitted(
        "session-replaced",
        replacementToken,
        4,
        1_120,
      ),
    ).toBe(70);
  });

  it("consumes the exact adoption token for a bounded resident window", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const adoptedSession = session(
      "session-windowed",
      Array.from({ length: 20 }, (_, index) => message(`tail-${index}`)),
    );
    const token = noteSessionTailAdopted(adoptedSession, 1_000);

    expect(
      noteSessionTranscriptCommitted("session-windowed", token, 8, 1_025),
    ).toBe(25);
    expect(warn).not.toHaveBeenCalled();
    expect(
      noteSessionTranscriptCommitted("session-windowed", token, 20, 10_000),
    ).toBeNull();
  });

  it("expires abandoned adoptions instead of reporting stale latency", () => {
    const warn = vi.fn();
    const adoptedSession = session("session-abandoned", [message("tail")]);
    const token = noteSessionTailAdopted(adoptedSession, 1_000);

    expect(
      noteSessionTranscriptCommitted(
        "session-abandoned",
        token,
        1,
        31_001,
        warn,
      ),
    ).toBeNull();
    expect(warn).not.toHaveBeenCalled();
  });
});
