// Pure bounded-history merge helpers.
//
// The server returns ascending pages with stable message ids and authoritative
// global positions. Live SSE may mutate the retained window while a page is in
// flight, so each merge preserves only the resident ranges whose position and
// identity remain compatible with that page. Server-instance admission stays
// in app-live-state.ts.

import type { SessionHistoryResponse } from "./api";
import type { Session } from "./types";

export type SessionHistoryMergeOutcome =
  | { kind: "applied"; session: Session }
  | { kind: "cursorChanged" }
  | { kind: "metadataChanged" }
  | { kind: "protocolError"; message: string };

function residentMessageStartIndex(session: Session) {
  if (session.messagesLoaded === true) {
    return 0;
  }
  return (
    session.messageStartIndex ??
    Math.max(
      0,
      (session.messageCount ?? session.messages.length) -
        session.messages.length,
    )
  );
}

function maximumSessionMutationStamp(
  current: Session,
  page: SessionHistoryResponse,
) {
  return Math.max(
    current.sessionMutationStamp ?? 0,
    page.sessionMutationStamp,
  );
}

function pageProtocolError(
  page: SessionHistoryResponse,
): SessionHistoryMergeOutcome | null {
  if (page.hasMore && page.messages.length === 0) {
    return {
      kind: "protocolError",
      message: "session history page claimed older messages but returned none",
    };
  }
  const expectedNextBefore = page.hasMore
    ? (page.messages[0]?.id ?? null)
    : null;
  if ((page.nextBefore ?? null) !== expectedNextBefore) {
    return {
      kind: "protocolError",
      message: "session history page returned an inconsistent next cursor",
    };
  }
  const expectedNextAfter = page.hasNewer
    ? (page.messages[page.messages.length - 1]?.id ?? null)
    : null;
  if ((page.nextAfter ?? null) !== expectedNextAfter) {
    return {
      kind: "protocolError",
      message: "session history page returned an inconsistent forward cursor",
    };
  }
  const ids = new Set<string>();
  for (const message of page.messages) {
    if (!message.id || ids.has(message.id)) {
      return {
        kind: "protocolError",
        message: "session history page returned duplicate or empty message ids",
      };
    }
    ids.add(message.id);
  }
  return null;
}

export function prependSessionHistoryPage({
  current,
  page,
  requestedBefore,
}: {
  current: Session;
  page: SessionHistoryResponse;
  requestedBefore: string;
}): SessionHistoryMergeOutcome {
  const protocolError = pageProtocolError(page);
  if (protocolError) {
    return protocolError;
  }
  if (current.messages[0]?.id !== requestedBefore) {
    return { kind: "cursorChanged" };
  }

  const currentIds = new Set(current.messages.map((message) => message.id));
  if (
    page.messages.some(
      (message) => message.id === requestedBefore || currentIds.has(message.id),
    )
  ) {
    return {
      kind: "protocolError",
      message: "session history page overlapped its exclusive before cursor",
    };
  }

  const messages = [...page.messages, ...current.messages];
  return {
    kind: "applied",
    session: {
      ...current,
      messages,
      // `messagesLoaded` means the resident window spans the complete
      // transcript from the true start through the live tail. Reaching the
      // start of a detached historical window is not enough.
      messagesLoaded: !page.hasMore && current.hasNewerHistory !== true,
      hasOlderHistory: page.hasMore,
      hasNewerHistory: current.hasNewerHistory ?? false,
      messageStartIndex: Math.max(
        0,
        residentMessageStartIndex(current) - page.messages.length,
      ),
      messageCount: Math.max(
        current.messageCount ?? 0,
        page.messageCount,
        messages.length,
      ),
    },
  };
}

export function replaceSessionWithHistoryStartPage({
  current,
  page,
}: {
  current: Session;
  page: SessionHistoryResponse;
}): SessionHistoryMergeOutcome {
  const protocolError = pageProtocolError(page);
  if (protocolError) {
    return protocolError;
  }
  if (page.hasMore || page.nextBefore) {
    return {
      kind: "protocolError",
      message:
        "session history start page did not begin at the transcript start",
    };
  }
  if (page.messageCount > 0 && page.messages.length === 0) {
    return {
      kind: "protocolError",
      message:
        "session history start page was empty for a non-empty transcript",
    };
  }
  const currentMessageCount = current.messageCount ?? current.messages.length;
  const currentMutationStamp = current.sessionMutationStamp ?? 0;
  const hasNewerHistory =
    page.hasNewer === true ||
    page.messageCount < currentMessageCount ||
    page.sessionMutationStamp < currentMutationStamp;
  return {
    kind: "applied",
    session: {
      ...current,
      messages: page.messages,
      messagesLoaded: !hasNewerHistory,
      hasOlderHistory: false,
      hasNewerHistory,
      messageStartIndex: 0,
      messageCount: Math.max(
        currentMessageCount,
        page.messageCount,
        page.messages.length,
      ),
      sessionMutationStamp: maximumSessionMutationStamp(current, page),
    },
  };
}

export function appendSessionHistoryPage({
  current,
  page,
  requestedAfter,
}: {
  current: Session;
  page: SessionHistoryResponse;
  requestedAfter: string;
}): SessionHistoryMergeOutcome {
  const protocolError = pageProtocolError(page);
  if (protocolError) {
    return protocolError;
  }
  if (current.messages[current.messages.length - 1]?.id !== requestedAfter) {
    return { kind: "cursorChanged" };
  }
  const currentIds = new Set(current.messages.map((message) => message.id));
  if (
    page.messages.some(
      (message) => message.id === requestedAfter || currentIds.has(message.id),
    )
  ) {
    return {
      kind: "protocolError",
      message: "session history page overlapped its exclusive after cursor",
    };
  }
  const messages = [...current.messages, ...page.messages];
  const currentMessageCount = current.messageCount ?? current.messages.length;
  const currentMutationStamp = current.sessionMutationStamp ?? 0;
  // A delayed page remains usable when its exclusive cursor and message ids
  // still line up, but older metadata cannot prove that it reached the live
  // tail. Preserve the fresher resident metadata and keep newer history open
  // so a later tail request can reconcile any concurrently changed content.
  const hasNewerHistory =
    page.hasNewer === true ||
    page.messageCount < currentMessageCount ||
    page.sessionMutationStamp < currentMutationStamp;
  const coversTranscriptStart =
    current.hasOlderHistory === false || current.messagesLoaded === true;
  return {
    kind: "applied",
    session: {
      ...current,
      messages,
      // `page.hasMore` describes history older than this forward page, not
      // history older than the already-resident window. The transcript becomes
      // complete only once that start-anchored window also reaches the live
      // tail.
      messagesLoaded: coversTranscriptStart && !hasNewerHistory,
      hasOlderHistory: current.hasOlderHistory ?? false,
      hasNewerHistory,
      messageStartIndex: residentMessageStartIndex(current),
      messageCount: Math.max(
        currentMessageCount,
        page.messageCount,
        messages.length,
      ),
      sessionMutationStamp: maximumSessionMutationStamp(current, page),
    },
  };
}

export function replaceSessionWithHistoryTailPage({
  current,
  page,
}: {
  current: Session;
  page: SessionHistoryResponse;
}): SessionHistoryMergeOutcome {
  const protocolError = pageProtocolError(page);
  if (protocolError) {
    return protocolError;
  }
  if (page.hasNewer || page.nextAfter) {
    return {
      kind: "protocolError",
      message:
        "session history tail page did not end at the live transcript tail",
    };
  }
  if (page.messageCount > 0 && page.messages.length === 0) {
    return {
      kind: "protocolError",
      message: "session history tail page was empty for a non-empty transcript",
    };
  }

  const currentMessageCount = current.messageCount ?? current.messages.length;
  const currentMutationStamp = current.sessionMutationStamp ?? null;
  const pageMutationStamp = page.sessionMutationStamp ?? null;
  if (
    page.messageCount < currentMessageCount ||
    (currentMutationStamp !== null &&
      pageMutationStamp !== null &&
      pageMutationStamp < currentMutationStamp)
  ) {
    return { kind: "metadataChanged" };
  }

  return {
    kind: "applied",
    session: {
      ...current,
      messages: page.messages,
      messagesLoaded: !page.hasMore,
      hasOlderHistory: page.hasMore,
      hasNewerHistory: false,
      messageStartIndex: Math.max(0, page.messageCount - page.messages.length),
      messageCount: Math.max(page.messageCount, page.messages.length),
      sessionMutationStamp: page.sessionMutationStamp,
    },
  };
}

export function replaceSessionWithHistoryAroundPage({
  current,
  page,
  requestedPosition,
}: {
  current: Session;
  page: SessionHistoryResponse;
  requestedPosition: number;
}): SessionHistoryMergeOutcome {
  const protocolError = pageProtocolError(page);
  if (protocolError) {
    return protocolError;
  }
  const pageStart = Math.max(0, page.messageStartIndex ?? 0);
  const pageEnd = pageStart + page.messages.length;
  if (
    page.messageCount > 0 &&
    (page.messages.length === 0 ||
      requestedPosition < pageStart ||
      requestedPosition >= pageEnd)
  ) {
    return {
      kind: "protocolError",
      message:
        "session history around page did not contain its requested position",
    };
  }

  const currentMessageCount = current.messageCount ?? current.messages.length;
  const currentMutationStamp = current.sessionMutationStamp ?? 0;
  const messageCount = Math.max(
    currentMessageCount,
    page.messageCount,
    pageEnd,
  );
  const hasOlderHistory = page.hasMore || pageStart > 0;
  const hasNewerHistory =
    page.hasNewer === true ||
    pageEnd < messageCount ||
    page.sessionMutationStamp < currentMutationStamp;
  return {
    kind: "applied",
    session: {
      ...current,
      messages: page.messages,
      messagesLoaded: !hasOlderHistory && !hasNewerHistory,
      hasOlderHistory,
      hasNewerHistory,
      messageStartIndex: pageStart,
      messageCount,
      sessionMutationStamp: Math.max(
        currentMutationStamp,
        page.sessionMutationStamp,
      ),
    },
  };
}

export function repairSessionTailFromHistoryPage({
  current,
  page,
}: {
  current: Session;
  page: SessionHistoryResponse;
}): SessionHistoryMergeOutcome {
  const protocolError = pageProtocolError(page);
  if (protocolError) {
    return protocolError;
  }
  if (
    page.messageCount !== (current.messageCount ?? current.messages.length) ||
    page.sessionMutationStamp !== (current.sessionMutationStamp ?? 0)
  ) {
    return { kind: "metadataChanged" };
  }
  const pageStart = Math.max(
    0,
    page.messageStartIndex ?? page.messageCount - page.messages.length,
  );
  const pageEnd = pageStart + page.messages.length;
  if (page.hasNewer === true || pageEnd !== page.messageCount) {
    return {
      kind: "protocolError",
      message:
        "session history repair page did not end at the live transcript tail",
    };
  }
  if (page.messages.length === 0) {
    if (page.messageCount > 0) {
      return {
        kind: "protocolError",
        message:
          "session history repair page was empty for a non-empty transcript",
      };
    }
    return {
      kind: "applied",
      session: {
        ...current,
        messages: [],
        messagesLoaded: true,
        hasOlderHistory: false,
        hasNewerHistory: false,
        messageStartIndex: 0,
        messageCount: 0,
      },
    };
  }

  const currentStart = residentMessageStartIndex(current);
  const currentEnd = currentStart + current.messages.length;
  if (currentEnd < pageStart) {
    // `cursorChanged` also covers a resident window that no longer reaches the
    // authoritative tail page. Both cases require a silent state resync.
    return { kind: "cursorChanged" };
  }

  // The history response carries authoritative global positions. A missed or
  // partially applied text delta can make ids inside the retained suffix
  // diverge even though the response metadata still matches. Preserve only
  // the contiguous resident prefix before the page, then replace the entire
  // covered suffix. Requiring every overlapping id to match would turn the
  // exact corruption this path repairs into a user-visible protocol error.
  const retainedPrefixLength =
    currentStart <= pageStart ? pageStart - currentStart : 0;
  const candidateRetainedPrefix = current.messages.slice(
    0,
    retainedPrefixLength,
  );
  const pageIds = new Set(page.messages.map((message) => message.id));
  const retainedPrefixIsTrustworthy =
    currentStart >= 0 &&
    currentEnd <= page.messageCount &&
    !candidateRetainedPrefix.some((message) => pageIds.has(message.id));
  // A repeated page id or a resident window extending beyond the transcript
  // proves that at least one retained prefix position may be corrupt. There is
  // no safe way to represent the resulting hole, so converge on the
  // authoritative tail page as a partial window and allow older history to be
  // fetched again. Keeping the prefix could publish duplicate React keys or
  // incorrectly mark a phantom-filled transcript as fully loaded.
  const retainedPrefix = retainedPrefixIsTrustworthy
    ? candidateRetainedPrefix
    : [];
  const repairedStart = retainedPrefixIsTrustworthy
    ? Math.min(currentStart, pageStart)
    : pageStart;
  const messages = [...retainedPrefix, ...page.messages];
  return {
    kind: "applied",
    session: {
      ...current,
      messages,
      messagesLoaded: repairedStart === 0,
      hasOlderHistory: repairedStart > 0,
      hasNewerHistory: false,
      messageStartIndex: repairedStart,
      messageCount: page.messageCount,
    },
  };
}
