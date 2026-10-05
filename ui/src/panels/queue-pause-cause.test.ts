import { describe, expect, it } from "vitest";

import {
  queuePausedCardText,
  resolveQueuePauseCause,
  retainedPromptHoldText,
} from "./queue-pause-cause";

const DUE_AT = "2026-10-05T07:40:00Z";
const LOCAL_TIME = new Date(DUE_AT).toLocaleTimeString();
const RETRY = `Engram: waiting for admission; retrying automatically, attempt 3, next at ${DUE_AT}.`;
const HELD = "Engram: Waiting/Unknown. Original prompt retained; resume to retry or cancel.";

describe("resolveQueuePauseCause", () => {
  it("reads the attempt and the next time of an automatic Engram retry", () => {
    expect(resolveQueuePauseCause(RETRY)).toEqual({
      kind: "engramRetrying",
      preview: RETRY,
      retry: { attempt: 3, nextAt: LOCAL_TIME },
    });
  });

  it("keeps an automatic retry whose attempt or time it cannot read, without them", () => {
    for (const preview of [
      "Engram: waiting for admission; retrying automatically.",
      "Engram: waiting for admission; retrying automatically, attempt 3, next at soon.",
      "Engram: waiting for admission; retrying automatically, attempt three, next at 2026-10-05T07:40:00Z.",
    ]) {
      expect(resolveQueuePauseCause(preview)).toEqual({ kind: "engramRetrying", preview, retry: null });
    }
  });

  it("reads any other Engram preview as a hold", () => {
    expect(resolveQueuePauseCause(HELD)).toEqual({ kind: "engramHeld", preview: HELD });
  });

  it("reads only the Stop preview as a stop", () => {
    expect(resolveQueuePauseCause("Turn stopped by user.")).toEqual({ kind: "stopped" });
    expect(resolveQueuePauseCause("Turn stopped: Engram MCP configuration was revoked.")).toEqual({
      kind: "other",
      preview: "Turn stopped: Engram MCP configuration was revoked.",
    });
    expect(resolveQueuePauseCause("")).toEqual({ kind: "other", preview: "" });
  });
});

describe("queuePausedCardText", () => {
  it("asks for nothing while Engram retries on its own", () => {
    expect(queuePausedCardText(resolveQueuePauseCause(RETRY), "Codex", 2)).toEqual({
      heading: "Waiting for Engram admission; retrying automatically",
      detail: `Attempt 3, next at ${LOCAL_TIME}. No action is needed.`,
      waiting: "2 prompts waiting.",
    });
    const unread = "Engram: waiting for admission; retrying automatically.";
    expect(queuePausedCardText(resolveQueuePauseCause(unread), "Codex", 1)).toEqual({
      heading: "Waiting for Engram admission; retrying automatically",
      detail: `${unread} No action is needed.`,
      waiting: "1 prompt waiting.",
    });
  });

  it("names an Engram hold by its own preview", () => {
    expect(queuePausedCardText(resolveQueuePauseCause(HELD), "Claude", 1)).toEqual({
      heading: "Engram is holding the queue",
      detail: HELD,
      waiting: "1 prompt waiting.",
    });
  });

  it("says the agent was stopped only for a stop", () => {
    expect(queuePausedCardText(resolveQueuePauseCause("Turn stopped by user."), "Claude", 1)).toEqual({
      heading: "Claude was stopped; the queue is paused",
      detail: null,
      waiting: "1 prompt waiting. Send a new prompt or resume the queue to continue.",
    });
  });

  it("names any other cause by the preview, and leaves it out when there is none", () => {
    const failure = "Codex reported an error: the provider is overloaded.";
    expect(queuePausedCardText(resolveQueuePauseCause(failure), "Codex", 2)).toEqual({
      heading: "The queue is paused",
      detail: failure,
      waiting: "2 prompts waiting. Send a new prompt or resume the queue to continue.",
    });
    expect(queuePausedCardText(resolveQueuePauseCause(""), "Codex", 1).detail).toBeNull();
  });
});

describe("retainedPromptHoldText", () => {
  it("says Cancel drops the prompt while Engram retries, without asking for Resume", () => {
    expect(retainedPromptHoldText(resolveQueuePauseCause(RETRY))).toBe(
      `Engram admission is retrying automatically (attempt 3, next at ${LOCAL_TIME}). Cancel drops the retained prompt.`,
    );
    const unread = "Engram: waiting for admission; retrying automatically.";
    expect(retainedPromptHoldText(resolveQueuePauseCause(unread))).toBe(
      `${unread} Cancel drops the retained prompt.`,
    );
  });

  it("gives an Engram hold its own preview, and any other hold the plain held text", () => {
    expect(retainedPromptHoldText(resolveQueuePauseCause(HELD))).toBe(HELD);
    for (const preview of ["Turn stopped by user.", "", "Codex reported an error."]) {
      expect(retainedPromptHoldText(resolveQueuePauseCause(preview))).toBe(
        "Authorization is held. Resume to retry, or cancel the retained prompt.",
      );
    }
  });
});
