import { describe, expect, it } from "vitest";
import { resolveSessionActivity, type SessionActivityOptions } from "./AgentSessionPanel.waiting-indicator";

describe("explicit session activity", () => {
  const session = { agent: "Codex" as const, status: "idle" as const };
  it.each([
    [{ session }, "idle", false],
    [{ session: { ...session, status: "active" } }, "working", true],
    [{ session: { ...session, status: "stopping" } }, "stopping", true],
    [{ session: { ...session, status: "approval" } }, "approval", false],
    [{ session: { ...session, status: "error" } }, "error", false],
    [{ session, isSending: true }, "sending", true],
    [{ session, isStopping: true }, "stopping", true],
    [{ session, delegationWaitPrompt: "Waiting for reviewers" }, "waiting", false],
    [{ session: { ...session, pendingPrompts: [{ id: "q", timestamp: "10:00", text: "Next task" }] } }, "queued", true],
    [{ session: { ...session, queuePaused: true, pendingPrompts: [{ id: "q", timestamp: "10:00", text: "Next task" }] } }, "paused", false],
  ] satisfies [SessionActivityOptions, string, boolean][])("resolves explicit state %#", (options, state, animated) => {
    expect(resolveSessionActivity(options)).toMatchObject({ state, animated });
  });

  it("never reads messages to decide whether a send, wait, or active turn exists", () => {
    for (const status of ["idle", "active", "stopping"] as const) {
      const source = { ...session, status };
      Object.defineProperty(source, "messages", { get() { throw new Error("Transcript residency is not status"); } });
      expect(() => resolveSessionActivity({ session: source, isSending: true })).not.toThrow();
      expect(() => resolveSessionActivity({ session: source, delegationWaitPrompt: "Review fan-in" })).not.toThrow();
    }
  });

  it("uses the explicit user prompt rather than shell-command output", () => {
    expect(resolveSessionActivity({ session: {
      ...session, status: "active",
      liveActivity: { prompt: " /review-code ", command: "cargo test", commandStatus: "running" },
    } })).toMatchObject({ state: "working", prompt: "/review-code" });
  });

  it("does not reuse stale live activity when idle or sending", () => {
    const source = { ...session, liveActivity: { prompt: "Previous turn" } };
    expect(resolveSessionActivity({ session: source }).prompt).toBeNull();
    expect(resolveSessionActivity({ session: source, isSending: true }).prompt).toBeNull();
  });

  it("uses pending prompt context during queue handoff and never spins a paused queue", () => {
    const source = { ...session, pendingPrompts: [{ id: "q", timestamp: "10:00", text: "Next task" }] };
    expect(resolveSessionActivity({ session: source })).toMatchObject({ state: "queued", prompt: "Next task" });
    expect(resolveSessionActivity({ session: { ...source, queuePaused: true } })).toMatchObject({ state: "paused", animated: false });
  });
});
