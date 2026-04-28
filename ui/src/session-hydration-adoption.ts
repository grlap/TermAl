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
  messageCount: number | null;
  revision: number | null;
  serverInstanceId: string | null;
  sessionMutationStamp: number | null;
};

export type AdoptFetchedSessionOutcome =
  | "adopted"
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

function hydrationResponseMatchesSession(
  responseSession: Session,
  currentSession: Session,
) {
  if (!hydrationSessionMetadataMatches(responseSession, currentSession)) {
    return false;
  }

  if (!hydrationRetainedMessagesMatch(responseSession, currentSession)) {
    return false;
  }

  return true;
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
  const responseMatches = hydrationResponseMatchesSession(
    responseSession,
    currentSession,
  );
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
  const canAdoptLowerRevisionHydration =
    currentSession.messagesLoaded !== true &&
    responseIsNotOlderThanRequest &&
    requestStillMatches &&
    responseMatches;
  // The downgrade allowance below is intentionally narrower than
  // "metadata-only": the request and response must still match the current
  // summary, otherwise a delayed full-session response can clobber newer
  // delta metadata and mark stale text as loaded.
  // Mismatched session-hydration responses are not authoritative enough to
  // prove whether the responding instance is newer or a late old process, so
  // they trigger a full /api/state resync above instead of being adopted
  // directly. For same-instance hydration, preserve the existing nuance: on
  // a genuine first hydration (no local messages yet) we accept even a lower
  // revision, but once the session has hydrated messages we refuse to clobber
  // them with an older snapshot.
  if (
    !shouldAdoptSnapshotRevision(currentRevision, responseRevision, {
      lastSeenServerInstanceId: currentServerInstanceId,
      nextServerInstanceId: responseServerInstanceId,
      seenServerInstanceIds,
      force: true,
      allowRevisionDowngrade: canAdoptLowerRevisionHydration,
    })
  ) {
    return "stale";
  }

  if (!requestStillMatches || !responseMatches) {
    if (
      requestStillMatches &&
      hydrationSessionMetadataIsAhead(responseSession, currentSession)
    ) {
      return "stateResync";
    }
    return "stale";
  }

  if (responseSession.messagesLoaded !== true) {
    return "stale";
  }

  return "adopted";
}
