// Owns: resolving the activity strip's state, label, and prompt context.
// Does not own: transcript content, scroll authority, or activity rendering.
// Split from: ui/src/panels/AgentSessionPanel.tsx.
// Activity presentation uses explicit session state, never transcript residency.
import type { Session } from "../types";

export type SessionActivitySource = Pick<
  Session,
  "agent" | "status" | "liveActivity" | "pendingPrompts" | "queuePaused"
>;

type SessionActivityState =
  | "idle"
  | "working"
  | "sending"
  | "stopping"
  | "approval"
  | "waiting"
  | "queued"
  | "paused"
  | "error";

export type SessionActivityOptions = {
  session: SessionActivitySource;
  isSending?: boolean;
  isStopping?: boolean;
  delegationWaitPrompt?: string | null;
};

export function resolveSessionActivity({
  session,
  isSending = false,
  isStopping = false,
  delegationWaitPrompt = null,
}: SessionActivityOptions) {
  const prompt = session.liveActivity?.prompt.trim() || null;
  const pendingPrompt = session.pendingPrompts?.[0]?.text.trim() || null;
  const describe = (
    state: SessionActivityState,
    label: string,
    context: string | null = null,
    animated = false,
  ) => ({ state, label: `${session.agent} ${label}`, prompt: context, animated });

  if (isStopping || session.status === "stopping") {
    return describe("stopping", "is stopping", prompt, true);
  }
  if (session.status === "approval") {
    return describe("approval", "is waiting for approval or input", prompt);
  }
  if (session.status === "active") {
    return describe("working", "is working", prompt, true);
  }
  if (isSending) {
    // An in-flight send is explicit local state. Old output is irrelevant.
    return describe("sending", "is sending a prompt", null, true);
  }
  if (session.status === "error") {
    return describe("error", "encountered an error");
  }
  if (session.queuePaused && (session.pendingPrompts?.length ?? 0) > 0) {
    return describe("paused", "has a paused queue", pendingPrompt);
  }
  if (delegationWaitPrompt) {
    return describe("waiting", "is waiting for delegated sessions", delegationWaitPrompt);
  }
  if ((session.pendingPrompts?.length ?? 0) > 0) {
    return describe("queued", "is starting the next turn", pendingPrompt, true);
  }
  return describe("idle", "is idle");
}
