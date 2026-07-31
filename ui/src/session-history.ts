// Pure bounded-history merge helpers.
//
// The server returns ascending pages before an exclusive stable message-id
// cursor. Live SSE may append while a page is in flight, so merges preserve the
// current retained tail and only prepend ids proven to be older than its first
// message. Server-instance admission stays in app-live-state.ts.

import type { SessionHistoryResponse } from "./api";
import type { Message, Session } from "./types";

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
      (session.messageCount ?? session.messages.length) - session.messages.length,
    )
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
      messagesLoaded:
        !page.hasMore && current.hasNewerHistory !== true,
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
      message: "session history start page did not begin at the transcript start",
    };
  }
  if (page.messageCount > 0 && page.messages.length === 0) {
    return {
      kind: "protocolError",
      message: "session history start page was empty for a non-empty transcript",
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
      sessionMutationStamp: Math.max(
        currentMutationStamp,
        page.sessionMutationStamp,
      ),
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
      messagesLoaded: coversTranscriptStart && page.hasNewer !== true,
      hasOlderHistory: current.hasOlderHistory ?? false,
      hasNewerHistory: page.hasNewer === true,
      messageStartIndex: residentMessageStartIndex(current),
      messageCount: Math.max(page.messageCount, messages.length),
      sessionMutationStamp: page.sessionMutationStamp,
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
      message: "session history tail page did not end at the live transcript tail",
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
      messageStartIndex: Math.max(
        0,
        page.messageCount - page.messages.length,
      ),
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
      message: "session history around page did not contain its requested position",
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
  if (page.messages.length === 0) {
    return {
      kind: "applied",
      session: {
        ...current,
        messages: [],
        messagesLoaded: true,
        messageCount: 0,
      },
    };
  }

  const currentIndexById = new Map(
    current.messages.map((message, index) => [message.id, index]),
  );
  let firstOverlap:
    { currentIndex: number; pageIndex: number; message: Message } | undefined;
  for (let pageIndex = 0; pageIndex < page.messages.length; pageIndex += 1) {
    const message = page.messages[pageIndex]!;
    const currentIndex = currentIndexById.get(message.id);
    if (currentIndex !== undefined) {
      firstOverlap = { currentIndex, pageIndex, message };
      break;
    }
  }
  if (!firstOverlap) {
    return { kind: "cursorChanged" };
  }

  const retainedPrefixLength = Math.max(
    0,
    firstOverlap.currentIndex - firstOverlap.pageIndex,
  );
  for (
    let pageIndex = firstOverlap.pageIndex;
    pageIndex < page.messages.length;
    pageIndex += 1
  ) {
    const currentIndex = retainedPrefixLength + pageIndex;
    const currentMessage = current.messages[currentIndex];
    if (currentMessage && currentMessage.id !== page.messages[pageIndex]?.id) {
      return {
        kind: "protocolError",
        message:
          "session history repair page did not align with retained messages",
      };
    }
  }
  const messages = [
    ...current.messages.slice(0, retainedPrefixLength),
    ...page.messages,
  ];
  return {
    kind: "applied",
    session: {
      ...current,
      messages,
      messagesLoaded: current.messagesLoaded === true,
      messageCount: Math.max(page.messageCount, messages.length),
    },
  };
}
