// Owns: session-scoped continuity for one-shot, paint-only reveals of newly
// appended resident transcript messages.
// Does not own: card layout, scroll anchoring, virtualization measurements,
// message hydration, or LIVE TURN presentation.

import { useEffect, useLayoutEffect, useMemo, useRef } from "react";

import type { Message } from "../types";

export type ConversationMessageRevealInput = {
  isActive: boolean;
  liveTurnVisible: boolean;
  messageIds: readonly string[];
  pendingPromptIds: readonly string[];
  userScrollGeneration: number;
};

export type ConversationMessageRevealState = {
  liveTurnVisible: boolean;
  // Workspace routing currently keeps one session tab (and therefore one
  // virtualizer generation) per session. If duplicate session panes become a
  // supported layout, pending reveals must be keyed by pane/scroll-state too.
  pendingRevealIds: ReadonlyMap<string, number>;
  pendingPromptIds: ReadonlySet<string>;
  tailMessageId: string | null;
  userScrollGeneration: number;
};

export type ConversationMessageRevealTransition = {
  nextState: ConversationMessageRevealState;
  revealMessageIds: readonly string[];
};

const EMPTY_REVEAL_MESSAGE_IDS: ReadonlySet<string> = new Set();
const revealStateBySessionId = new Map<
  string,
  ConversationMessageRevealState
>();

export function resolveConversationMessageRevealTransition(
  previous: ConversationMessageRevealState | null,
  input: ConversationMessageRevealInput,
): ConversationMessageRevealTransition {
  const currentTailMessageId =
    input.messageIds[input.messageIds.length - 1] ?? null;
  const currentMessageIds = new Set(input.messageIds);
  const pendingRevealIds = new Map<string, number>();
  previous?.pendingRevealIds.forEach((generation, messageId) => {
    if (
      generation === input.userScrollGeneration &&
      currentMessageIds.has(messageId)
    ) {
      pendingRevealIds.set(messageId, generation);
    }
  });
  const nextState: ConversationMessageRevealState = {
    liveTurnVisible: input.liveTurnVisible,
    pendingRevealIds,
    pendingPromptIds: new Set(input.pendingPromptIds),
    tailMessageId: currentTailMessageId,
    userScrollGeneration: input.userScrollGeneration,
  };

  if (
    previous === null ||
    !input.isActive ||
    previous.userScrollGeneration !== input.userScrollGeneration ||
    previous.tailMessageId === null
  ) {
    pendingRevealIds.clear();
    return { nextState, revealMessageIds: [] };
  }

  const previousTailIndex = input.messageIds.lastIndexOf(
    previous.tailMessageId,
  );
  if (previousTailIndex < 0) {
    // A disjoint history/adoption window does not prove a newly appended live
    // message. Seed its identity and never replay content the reader may
    // already have been viewing elsewhere.
    pendingRevealIds.clear();
    return { nextState, revealMessageIds: [] };
  }

  if (previous.liveTurnVisible && !input.liveTurnVisible) {
    // The final resident message can replace LIVE TURN in the same commit. Its
    // content is a continuity handoff, not a new visual object to flash again.
    return { nextState, revealMessageIds: [] };
  }

  const revealMessageIds: string[] = [];
  for (
    let messageIndex = previousTailIndex + 1;
    messageIndex < input.messageIds.length;
    messageIndex += 1
  ) {
    const messageId = input.messageIds[messageIndex];
    if (messageId && !previous.pendingPromptIds.has(messageId)) {
      revealMessageIds.push(messageId);
      pendingRevealIds.set(messageId, input.userScrollGeneration);
    }
  }

  return { nextState, revealMessageIds };
}

export function useConversationMessageRevealIds({
  isActive,
  liveTurnVisible,
  messages,
  pendingPromptIds,
  sessionId,
  userScrollGeneration,
}: {
  isActive: boolean;
  liveTurnVisible: boolean;
  messages: readonly Message[];
  pendingPromptIds: readonly string[];
  sessionId: string;
  userScrollGeneration: number;
}): ReadonlySet<string> {
  const messageIds = useMemo(
    () => messages.map((message) => message.id),
    [messages],
  );
  const input = useMemo<ConversationMessageRevealInput>(
    () => ({
      isActive,
      liveTurnVisible,
      messageIds,
      pendingPromptIds,
      userScrollGeneration,
    }),
    [
      isActive,
      liveTurnVisible,
      messageIds,
      pendingPromptIds,
      userScrollGeneration,
    ],
  );
  const transition = resolveConversationMessageRevealTransition(
    revealStateBySessionId.get(sessionId) ?? null,
    input,
  );

  useLayoutEffect(() => {
    // Commit after the DOM commit so StrictMode/concurrent render replays see
    // the same transition. Keeping the watermark outside the component makes
    // tab remounts and virtualizer recycling non-events rather than replays.
    // Re-resolve from the latest committed watermark: the same session can be
    // visible in two panes, and an older-window layout effect must never move a
    // newer pane's watermark backwards.
    const committedTransition = resolveConversationMessageRevealTransition(
      revealStateBySessionId.get(sessionId) ?? null,
      input,
    );
    revealStateBySessionId.set(sessionId, committedTransition.nextState);
  }, [input, sessionId]);

  return useMemo(
    () =>
      transition.revealMessageIds.length > 0
        ? new Set(transition.revealMessageIds)
        : EMPTY_REVEAL_MESSAGE_IDS,
    [transition.revealMessageIds],
  );
}

function pendingRevealMatches(
  sessionId: string,
  messageId: string,
  userScrollGeneration: number,
) {
  return (
    revealStateBySessionId
      .get(sessionId)
      ?.pendingRevealIds.get(messageId) === userScrollGeneration
  );
}

function consumePendingReveal(
  sessionId: string,
  messageId: string,
) {
  const current = revealStateBySessionId.get(sessionId);
  if (!current?.pendingRevealIds.has(messageId)) {
    return;
  }

  const nextPendingRevealIds = new Map(current.pendingRevealIds);
  // A matching entry has now painted once. A mismatched entry became stale
  // because the reader navigated before its virtualized shell mounted.
  nextPendingRevealIds.delete(messageId);
  revealStateBySessionId.set(sessionId, {
    ...current,
    pendingRevealIds: nextPendingRevealIds,
  });
}

export function useConversationMessageRevealOnMount({
  messageId,
  revealInCurrentCommit,
  revealUserScrollGeneration,
  sessionId,
  userScrollGeneration,
}: {
  messageId: string;
  revealInCurrentCommit: boolean;
  revealUserScrollGeneration: number;
  sessionId: string;
  userScrollGeneration: number;
}) {
  const shouldRevealRef = useRef<boolean | null>(null);
  if (shouldRevealRef.current === null) {
    shouldRevealRef.current =
      (revealInCurrentCommit &&
        revealUserScrollGeneration === userScrollGeneration) ||
      pendingRevealMatches(sessionId, messageId, userScrollGeneration);
  }

  useEffect(() => {
    // Passive consumption runs after the parent layout effect has published
    // this commit's pending ids. That ordering makes both an immediate mount
    // and the virtualizer's later mounted-range commit exactly-once, including
    // StrictMode's render/effect replay.
    consumePendingReveal(sessionId, messageId);
  }, [messageId, sessionId, userScrollGeneration]);

  return shouldRevealRef.current;
}
