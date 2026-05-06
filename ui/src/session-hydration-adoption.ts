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
import type { Session } from "./types";

export type SessionHydrationRequestContext = {
  allowDivergentTextRepairAfterNewerRevision?: boolean;
  allowPartialTranscript?: boolean;
  messageCount: number | null;
  revision: number | null;
  serverInstanceId: string | null;
  sessionMutationStamp: number | null;
};

export type AdoptFetchedSessionOutcome =
  | "adopted"
  | "partial"
  | "stale"
  | "stateResync"
  | "restartResync";

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
  const responseMessageKeys = responseSession.messages.map((message) =>
    JSON.stringify(message),
  );
  let responseIndex = 0;
  for (const currentMessage of currentSession.messages) {
    const currentSerialized = JSON.stringify(currentMessage);
    let matched = false;
    while (responseIndex < responseMessageKeys.length) {
      const responseSerialized = responseMessageKeys[responseIndex];
      responseIndex += 1;
      if (responseSerialized === currentSerialized) {
        matched = true;
        break;
      }
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
  const responseMatches =
    responseMetadataMatches &&
    hydrationRetainedMessagesMatch(responseSession, currentSession);
  if (
    requestStillMatches &&
    !responseMatches &&
    hydrationSessionMetadataIsAhead(responseSession, currentSession)
  ) {
    return "stateResync";
  }

  const responseIsNotOlderThanRequest =
    requestContext.revision === null ||
    responseRevision >= requestContext.revision;
  // See the text-repair sibling below before changing this branch: both
  // downgrade allowances share request/revision guards but intentionally
  // differ on transcript-match and loaded-state requirements.
  const canAdoptLowerRevisionHydration =
    currentSession.messagesLoaded !== true &&
    responseIsNotOlderThanRequest &&
    requestStillMatches &&
    responseMatches;
  // Text repair is the sibling downgrade path for an already-loaded
  // transcript whose live deltas diverged from the server transcript. It
  // deliberately does not require retained messages to match, but still
  // requires matching metadata and an explicitly flagged repair request so a
  // normal delayed hydration cannot overwrite newer text. See also the normal
  // lower-revision hydration branch above; they should stay visibly paired.
  const canAdoptLowerRevisionTextRepairHydration =
    requestContext.allowDivergentTextRepairAfterNewerRevision === true &&
    responseIsNotOlderThanRequest &&
    requestStillMatches &&
    responseMetadataMatches &&
    responseSession.messagesLoaded === true;
  // The downgrade allowance below is intentionally narrower than
  // "metadata-only": the request and response must still match the current
  // summary, otherwise a delayed full-session response can clobber newer
  // delta metadata and mark stale text as loaded. Same-metadata full-session
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

  if (!requestStillMatches || !responseMatches) {
    if (
      requestStillMatches &&
      (requestRevisionStillCurrent ||
        requestContext.allowDivergentTextRepairAfterNewerRevision === true) &&
      responseMetadataMatches &&
      responseSession.messagesLoaded === true
    ) {
      return "adopted";
    }
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
      requestContext.allowPartialTranscript === true &&
      responseSession.messages.length > 0
    ) {
      return "partial";
    }
    return "stale";
  }

  return "adopted";
}
