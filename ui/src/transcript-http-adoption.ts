// Pure session-tail adoption and its mechanical global-ledger effects.
// The local tail certificate is independent of the broad state revision;
// excluded or unpaired sessions keep master's classifier and effects.
import { classifyFetchedSessionAdoption, type AdoptFetchedSessionOutcome,
  type SessionHydrationRequestContext } from "./session-hydration-adoption";
import type { Session } from "./types";

export type HttpSessionDecisionInput = {
  responseSession: Session;
  responseRevision: number;
  responseServerInstanceId: string;
  requestContext: SessionHydrationRequestContext;
  currentSession: Session | null;
  currentRevision: number | null;
  currentServerInstanceId: string | null;
  seenServerInstanceIds: ReadonlySet<string>;
  pairedLocalTail: boolean;
};
export type HttpSessionDecision = {
  outcome: AdoptFetchedSessionOutcome;
  admission: "none" | "tail";
  revision: { kind: "leave" } | { kind: "set"; value: number };
  instance: { kind: "leave" } | { kind: "remember"; value: string };
};
export function decideHttpSessionAdoption(input: HttpSessionDecisionInput): HttpSessionDecision {
  const refuse = (outcome: AdoptFetchedSessionOutcome): HttpSessionDecision => ({
    outcome, admission: "none", revision: { kind: "leave" }, instance: { kind: "leave" },
  });
  const outcome = input.pairedLocalTail
    ? input.responseSession.messagesLoaded === true ? "adopted" : "partial"
    : classifyFetchedSessionAdoption(input);
  if (!input.currentSession || (outcome !== "adopted" && outcome !== "partial" &&
      outcome !== "partialCoverage")) return refuse(outcome);
  return { outcome, admission: "tail",
    revision: !input.pairedLocalTail && (input.currentRevision === null || input.responseRevision > input.currentRevision)
      ? { kind: "set", value: input.responseRevision } : { kind: "leave" },
    instance: input.responseServerInstanceId
      ? { kind: "remember", value: input.responseServerInstanceId } : { kind: "leave" } };
}
export function applyHttpSessionEffects(decision: HttpSessionDecision, refs: {
  latestRevision: { current: number | null };
  instance: { current: string | null };
  seenInstances: { current: Set<string> };
}) {
  if (decision.revision.kind === "set") refs.latestRevision.current = decision.revision.value;
  if (decision.instance.kind === "remember") {
    refs.seenInstances.current.add(decision.instance.value);
    refs.instance.current = decision.instance.value;
  }
}
