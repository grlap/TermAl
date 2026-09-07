// session-hydration-adoption.ts
//
// Owns pure session-hydration comparison and adoption classification helpers
// used by `useAppLiveState`.
//
// Split out of: ui/src/app-live-state.ts. Keep side effects in the hook; this
// module only decides whether a fetched session response is adoptable or needs
// a resync/retry path.

import {
  isServerInstanceMismatch,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import type { DeltaApplyResult } from "./live-updates";
import type { DeltaEvent, Message, Session } from "./types";

export type HydrationDeltaObservation = {
  delta: DeltaEvent | null;
  revisionAction: "apply" | "ignore" | "resync";
  resultKind: DeltaApplyResult["kind"] | null;
  previousSession: Session | null;
  nextSession: Session | null;
  targetPresent: boolean;
};

export type PartialTailAppendProof = {
  // Immutable request baseline and latest validated projection; no replay log.
  baseline: Session;
  latest: Session;
  valid: boolean;
};

function sameMessage(left: Message, right: Message) {
  return left === right || JSON.stringify(left) === JSON.stringify(right);
}

function compatibleTextAppend(before: Message, after: Message) {
  return (
    before.type === "text" && after.type === "text" &&
    after.text.startsWith(before.text) &&
    sameMessage(before, { ...after, text: before.text })
  );
}

function isRetainedTail(session: Session) {
  return (
    session.messagesLoaded === false && session.hasNewerHistory !== true &&
    session.messages.length > 0 &&
    Number.isSafeInteger(session.messageStartIndex) &&
    Number.isSafeInteger(session.messageCount) && session.messageStartIndex! >= 0 &&
    session.messageStartIndex! + session.messages.length === session.messageCount
  );
}

export function samePartialTailProjection(left: Session, right: Session) {
  return (
    left.id === right.id && left.remoteId === right.remoteId &&
    left.messageCount === right.messageCount &&
    left.sessionMutationStamp === right.sessionMutationStamp &&
    left.messageStartIndex === right.messageStartIndex &&
    left.messagesLoaded === right.messagesLoaded &&
    left.hasNewerHistory === right.hasNewerHistory &&
    left.hasOlderHistory === right.hasOlderHistory &&
    left.messages.length === right.messages.length &&
    left.messages.every((message, index) => sameMessage(message, right.messages[index]))
  );
}

/** Fail closed before inspecting an observation; never infer its event kind from text. */
export function advancePartialTailAppendProof(
  proof: PartialTailAppendProof,
  observation: HydrationDeltaObservation,
) {
  if (!proof.valid) return;
  proof.valid = false;
  const { delta, previousSession: before, nextSession: after } = observation;
  if (
    !delta || !before || !after || observation.revisionAction !== "apply" ||
    observation.resultKind !== "applied" ||
    !samePartialTailProjection(proof.latest, before) ||
    !isRetainedTail(before) || !isRetainedTail(after) ||
    after.messageStartIndex !== before.messageStartIndex ||
    typeof before.sessionMutationStamp !== "number" ||
    typeof after.sessionMutationStamp !== "number" ||
    after.sessionMutationStamp <= before.sessionMutationStamp
  ) return;
  const lastIndex = before.messages.length - 1;
  if (delta.type === "textDelta") {
    if (
      !observation.targetPresent || delta.messageId !== before.messages[lastIndex].id ||
      delta.messageIndex !== before.messageCount! - 1 ||
      after.messageCount !== before.messageCount ||
      after.messages.length !== before.messages.length ||
      !before.messages.slice(0, -1).every((message, index) =>
        sameMessage(message, after.messages[index])) ||
      !compatibleTextAppend(before.messages[lastIndex], after.messages[lastIndex])
    ) return;
  } else if (delta.type === "messageCreated") {
    if (
      observation.targetPresent || delta.messageIndex !== before.messageCount ||
      delta.messageCount !== before.messageCount! + 1 ||
      after.messageCount !== delta.messageCount ||
      after.messages.length !== before.messages.length + 1 ||
      !before.messages.every((message, index) => sameMessage(message, after.messages[index])) ||
      !sameMessage(delta.message, after.messages[after.messages.length - 1])
    ) return;
  } else {
    return;
  }
  proof.latest = after;
  proof.valid = true;
}

type SessionHydrationRequestMetadata = {
  messageCount: number | null;
  revision: number | null;
  serverInstanceId: string | null;
  sessionMutationStamp: number | null;
  partialTailAppendProof?: PartialTailAppendProof;
};

export type SessionHydrationRequestContext =
  | ({
      kind: "sessionTail";
    } & SessionHydrationRequestMetadata)
  | ({
      kind: "partialTail";
    } & SessionHydrationRequestMetadata)
  | ({
      kind: "textRepair";
    } & SessionHydrationRequestMetadata);

export type AdoptFetchedSessionOutcome =
  | "adopted"
  | "partial"
  | "partialCoverage"
  | "stale"
  | "stateResync"
  | "restartResync";

/** Add only older coverage; every resident record and current metadata win. */
export function mergeAppendOnlyPartialTailCoverage(
  response: Session,
  current: Session,
  request: SessionHydrationRequestContext,
): Session | null {
  const proof = request.partialTailAppendProof;
  if (
    request.kind !== "partialTail" || !proof?.valid ||
    !isRetainedTail(proof.baseline) || !isRetainedTail(current) ||
    !samePartialTailProjection(proof.latest, current) ||
    current.messageStartIndex !== proof.baseline.messageStartIndex ||
    response.id !== current.id || response.remoteId !== current.remoteId ||
    response.messagesLoaded !== false || response.messages.length === 0 ||
    !Number.isSafeInteger(response.messageCount) ||
    request.messageCount === null || request.sessionMutationStamp === null ||
    typeof current.sessionMutationStamp !== "number" ||
    typeof response.sessionMutationStamp !== "number" ||
    response.messageCount! < request.messageCount ||
    response.messageCount! > current.messageCount! ||
    response.sessionMutationStamp < request.sessionMutationStamp ||
    response.sessionMutationStamp > current.sessionMutationStamp
  ) return null;
  const start = response.messageCount! - response.messages.length;
  const currentStart = current.messageStartIndex!;
  // The resident representation has no holes. Require a real overlap; an old
  // disjoint window must be declined, never used to replace the newest cards.
  if (
    start < 0 || response.messageStartIndex !== start ||
    response.messageCount! <= currentStart || start >= current.messageCount! ||
    new Set(response.messages.map((message) => message.id)).size !== response.messages.length ||
    new Set(current.messages.map((message) => message.id)).size !== current.messages.length
  ) return null;
  const currentPositions = new Map(
    current.messages.map((message, index) => [message.id, currentStart + index]),
  );
  for (let index = 0; index < response.messages.length; index += 1) {
    const incoming = response.messages[index];
    const position = start + index;
    const residentPosition = currentPositions.get(incoming.id);
    if (residentPosition !== undefined && residentPosition !== position) return null;
    if (position < currentStart) continue;
    const resident = current.messages[position - currentStart];
    if (
      !resident || incoming.id !== resident.id ||
      (!sameMessage(incoming, resident) && !compatibleTextAppend(incoming, resident))
    ) return null;
    const baseline = proof.baseline.messages[position - currentStart];
    // The response is not older than the request; immutable baseline records
    // cannot be shorter/different, even if they happen to prefix current text.
    if (
      baseline && !sameMessage(baseline, incoming) &&
      !compatibleTextAppend(baseline, incoming)
    ) return null;
  }
  const prefix = response.messages.slice(0, Math.max(0, currentStart - start));
  if (prefix.length === 0) return current;
  return {
    ...current,
    messages: [...prefix, ...current.messages],
    messageStartIndex: Math.min(start, currentStart),
    messagesLoaded: false,
    hasOlderHistory: Math.min(start, currentStart) > 0,
    hasNewerHistory: false,
  };
}

export function hydrationRetainedMessagesMatch(
  responseSession: Pick<Session, "messages">,
  currentSession: Pick<Session, "messages">,
) {
  if (
    responseSession.messages.length === 0 ||
    currentSession.messages.length === 0
  ) {
    return true;
  }

  // This comparison intentionally covers the persisted message shape exactly.
  // UI-only message fields must either stay out of `Message` or be excluded
  // here explicitly, otherwise hydration can treat equivalent transcripts as
  // divergent and drop retained messages. The current client may retain a
  // gapped transcript tail from live deltas while waiting for hydration; every
  // retained message must appear in the fetched transcript, in order.
  let responseIndex = 0;
  for (const currentMessage of currentSession.messages) {
    const currentMessageId = currentMessage.id;
    let currentSerialized: string | null = null;
    let matched = false;
    while (responseIndex < responseSession.messages.length) {
      const responseMessage = responseSession.messages[responseIndex];
      responseIndex += 1;
      if (responseMessage.id !== currentMessageId) {
        continue;
      }
      currentSerialized ??= JSON.stringify(currentMessage);
      if (JSON.stringify(responseMessage) !== currentSerialized) {
        continue;
      }
      matched = true;
      break;
    }
    if (!matched) {
      return false;
    }
  }

  return true;
}

export function getHydrationMessageCount(
  session: Pick<Session, "messageCount" | "messagesLoaded" | "messages">,
) {
  if (typeof session.messageCount === "number") {
    return session.messageCount;
  }
  return session.messagesLoaded === true ? session.messages.length : null;
}

export function getHydrationMutationStamp(
  session: Pick<Session, "sessionMutationStamp">,
) {
  return session.sessionMutationStamp ?? null;
}

export function hydrationSessionMetadataMatches(
  responseSession: Pick<
    Session,
    "messageCount" | "messagesLoaded" | "messages" | "sessionMutationStamp"
  >,
  currentSession: Pick<
    Session,
    "messageCount" | "messagesLoaded" | "messages" | "sessionMutationStamp"
  >,
) {
  return (
    getHydrationMessageCount(responseSession) ===
      getHydrationMessageCount(currentSession) &&
    getHydrationMutationStamp(responseSession) ===
      getHydrationMutationStamp(currentSession)
  );
}

export function hydrationSessionMetadataIsAhead(
  responseSession: Pick<
    Session,
    "messageCount" | "messagesLoaded" | "messages" | "sessionMutationStamp"
  >,
  currentSession: Pick<
    Session,
    "messageCount" | "messagesLoaded" | "messages" | "sessionMutationStamp"
  >,
) {
  const responseMessageCount = getHydrationMessageCount(responseSession);
  const currentMessageCount = getHydrationMessageCount(currentSession);
  if (
    responseMessageCount !== null &&
    currentMessageCount !== null &&
    responseMessageCount > currentMessageCount
  ) {
    return true;
  }

  const responseMutationStamp = getHydrationMutationStamp(responseSession);
  const currentMutationStamp = getHydrationMutationStamp(currentSession);
  return (
    responseMutationStamp !== null &&
    currentMutationStamp !== null &&
    responseMutationStamp > currentMutationStamp
  );
}

function hydrationRequestStillMatchesSession(
  requestContext: SessionHydrationRequestContext,
  currentSession: Session,
) {
  return (
    requestContext.messageCount === getHydrationMessageCount(currentSession) &&
    requestContext.sessionMutationStamp ===
      getHydrationMutationStamp(currentSession)
  );
}

function isStaleHydrationFromSupersededInstance(
  requestContext: SessionHydrationRequestContext,
  responseServerInstanceId: string,
  currentServerInstanceId: string | null,
) {
  return (
    Boolean(requestContext.serverInstanceId) &&
    Boolean(currentServerInstanceId) &&
    Boolean(responseServerInstanceId) &&
    requestContext.serverInstanceId !== currentServerInstanceId &&
    requestContext.serverInstanceId === responseServerInstanceId
  );
}

export function classifyFetchedSessionAdoption({
  responseSession,
  responseRevision,
  responseServerInstanceId,
  requestContext,
  currentSession,
  currentRevision,
  currentServerInstanceId,
  seenServerInstanceIds,
}: {
  responseSession: Session;
  responseRevision: number;
  responseServerInstanceId: string;
  requestContext: SessionHydrationRequestContext;
  currentSession: Session | null;
  currentRevision: number | null;
  currentServerInstanceId: string | null;
  seenServerInstanceIds: ReadonlySet<string>;
}): AdoptFetchedSessionOutcome {
  if (!currentSession) {
    return "stale";
  }

  if (
    isStaleHydrationFromSupersededInstance(
      requestContext,
      responseServerInstanceId,
      currentServerInstanceId,
    )
  ) {
    return "stale";
  }

  const requestServerInstanceBaseline =
    requestContext.serverInstanceId ?? currentServerInstanceId;
  if (
    isServerInstanceMismatch(
      requestServerInstanceBaseline,
      responseServerInstanceId,
    )
  ) {
    return "restartResync";
  }

  const requestStillMatches = hydrationRequestStillMatchesSession(
    requestContext,
    currentSession,
  );
  const requestRevisionStillCurrent =
    requestContext.revision === null ||
    currentRevision === null ||
    requestContext.revision === currentRevision;
  const responseMetadataMatches = hydrationSessionMetadataMatches(
    responseSession,
    currentSession,
  );
  if (
    requestStillMatches &&
    !responseMetadataMatches &&
    hydrationSessionMetadataIsAhead(responseSession, currentSession)
  ) {
    return "stateResync";
  }

  // An old snapshot may still supply missing history when every intervening
  // session mutation was a validated tail append. This never replaces newer
  // resident text or relaxes cross-instance/ahead-authority protections.
  if (
    !requestStillMatches && requestContext.revision !== null &&
    currentRevision !== null && responseRevision >= requestContext.revision &&
    responseRevision <= currentRevision &&
    Boolean(requestContext.serverInstanceId) &&
    requestContext.serverInstanceId === currentServerInstanceId &&
    currentServerInstanceId === responseServerInstanceId &&
    mergeAppendOnlyPartialTailCoverage(responseSession, currentSession, requestContext)
  ) {
    return "partialCoverage";
  }

  const responseIsNotOlderThanRequest =
    requestContext.revision === null ||
    responseRevision >= requestContext.revision;
  let retainedMessagesMatch: boolean | null = null;
  const responseMatches = () => {
    if (!responseMetadataMatches) {
      return false;
    }
    retainedMessagesMatch ??= hydrationRetainedMessagesMatch(
      responseSession,
      currentSession,
    );
    return retainedMessagesMatch;
  };
  // See the text-repair sibling below before changing this branch: both
  // downgrade allowances share request/revision guards but intentionally
  // differ on transcript-match and loaded-state requirements.
  const canAdoptLowerRevisionHydration =
    currentSession.messagesLoaded !== true &&
    responseIsNotOlderThanRequest &&
    requestStillMatches &&
    responseMatches();
  // Text repair is the sibling downgrade path for an already-loaded
  // transcript whose live deltas diverged from the server transcript. It
  // deliberately does not require retained messages to match, but still
  // requires matching metadata and an explicitly flagged repair request so a
  // normal delayed hydration cannot overwrite newer text. See also the normal
  // lower-revision hydration branch above; they should stay visibly paired.
  const canAdoptLowerRevisionTextRepairHydration =
    requestContext.kind === "textRepair" &&
    responseIsNotOlderThanRequest &&
    requestStillMatches &&
    responseMetadataMatches &&
    responseSession.messagesLoaded === true;
  // The downgrade allowance below is intentionally narrower than
  // "metadata-only": the request and response must still match the current
  // summary, otherwise a delayed bounded-tail response can clobber newer
  // delta metadata and mark stale text as loaded. Same-metadata tail
  // responses may still replace divergent retained messages below; that is
  // the recovery path for live text streams whose deltas were applied with a
  // revision gap and left the retained transcript inconsistent with the
  // server's canonical transcript.
  if (
    !shouldAdoptSnapshotRevision(currentRevision, responseRevision, {
      lastSeenServerInstanceId: currentServerInstanceId,
      nextServerInstanceId: responseServerInstanceId,
      seenServerInstanceIds,
      force: true,
      allowRevisionDowngrade:
        canAdoptLowerRevisionHydration ||
        canAdoptLowerRevisionTextRepairHydration,
    })
  ) {
    return "stale";
  }

  const canAdoptLoadedResponseWithoutRetainedMatch =
    requestStillMatches &&
    (requestRevisionStillCurrent || requestContext.kind === "textRepair") &&
    responseMetadataMatches &&
    responseSession.messagesLoaded === true;
  if (canAdoptLoadedResponseWithoutRetainedMatch) {
    return "adopted";
  }
  const canAdoptPartialTailWithoutRetainedMatch =
    requestContext.kind === "partialTail" &&
    requestStillMatches &&
    requestRevisionStillCurrent &&
    responseMetadataMatches &&
    responseSession.messagesLoaded !== true &&
    responseSession.messages.length > 0;
  if (canAdoptPartialTailWithoutRetainedMatch) {
    return "partial";
  }

  if (!requestStillMatches || !responseMatches()) {
    if (
      requestStillMatches &&
      hydrationSessionMetadataIsAhead(responseSession, currentSession)
    ) {
      return "stateResync";
    }
    return "stale";
  }

  if (responseSession.messagesLoaded !== true) {
    if (
      requestContext.kind === "partialTail" &&
      responseSession.messages.length > 0
    ) {
      return "partial";
    }
    return "stale";
  }

  return "adopted";
}
