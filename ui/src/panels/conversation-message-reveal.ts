// Owns: pane-scroll-scoped continuity for one-shot, paint-only reveals of
// newly appended resident transcript messages.
// Does not own: card layout, scroll anchoring, virtualization measurements,
// message hydration, or activity-strip presentation.

import { useEffect, useLayoutEffect, useMemo, useRef } from "react";

import type { Message } from "../types";

export type ConversationMessageRevealInput = {
  isActive: boolean;
  isTurnActive: boolean;
  messageIds: readonly string[];
  pendingPromptIds: readonly string[];
  userScrollGeneration: number;
};

export type ConversationMessageRevealState = {
  isTurnActive: boolean;
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
export const CONVERSATION_MESSAGE_ENTRY_REVEAL_CLASS_NAME =
  "conversation-message-entry-reveal";
const CONVERSATION_MESSAGE_ENTRY_REVEAL_CANCELLED_ATTRIBUTE =
  "data-conversation-message-entry-reveal-cancelled";
const MAX_CONVERSATION_MESSAGE_REVEAL_SCOPES = 256;
const revealStateByScopeKey = new Map<
  string,
  ConversationMessageRevealState
>();

function getConversationMessageRevealState(revealScopeKey: string) {
  return revealStateByScopeKey.get(revealScopeKey) ?? null;
}

function setConversationMessageRevealState(
  revealScopeKey: string,
  state: ConversationMessageRevealState,
) {
  // Refresh recency only from committed/effect work, never during render.
  // Bounding the registry preserves remount continuity without retaining
  // every deleted session/pane scope for the lifetime of the application.
  revealStateByScopeKey.delete(revealScopeKey);
  revealStateByScopeKey.set(revealScopeKey, state);
  while (
    revealStateByScopeKey.size > MAX_CONVERSATION_MESSAGE_REVEAL_SCOPES
  ) {
    const oldestScopeKey = revealStateByScopeKey.keys().next().value;
    if (oldestScopeKey === undefined) {
      break;
    }
    revealStateByScopeKey.delete(oldestScopeKey);
  }
}

export function resetConversationMessageRevealRegistryForTesting() {
  revealStateByScopeKey.clear();
}

export function getConversationMessageRevealRegistrySizeForTesting() {
  return revealStateByScopeKey.size;
}

export function cancelConversationMessageEntryReveals(root: ParentNode) {
  root
    .querySelectorAll<HTMLElement>(
      `.${CONVERSATION_MESSAGE_ENTRY_REVEAL_CLASS_NAME}`,
    )
    .forEach((element) => {
      // User navigation owns the viewport immediately. Remove the animation
      // class in the same input task so the browser cannot paint another fade
      // frame after wheel, touch, or keyboard scrolling starts. The unmanaged
      // marker prevents a later React className update from restarting this
      // one-shot animation on the same mounted message shell.
      element.setAttribute(
        CONVERSATION_MESSAGE_ENTRY_REVEAL_CANCELLED_ATTRIBUTE,
        "",
      );
      element.classList.remove(CONVERSATION_MESSAGE_ENTRY_REVEAL_CLASS_NAME);
    });
}

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
    isTurnActive: input.isTurnActive,
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

  if (previous.isTurnActive && !input.isTurnActive) {
    // Completing a turn preserves the final response's visual continuity.
    // This is explicit turn state, not the presence or height of an activity card.
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
  isTurnActive,
  messages,
  pendingPromptIds,
  revealScopeKey,
  userScrollGeneration,
}: {
  isActive: boolean;
  isTurnActive: boolean;
  messages: readonly Message[];
  pendingPromptIds: readonly string[];
  revealScopeKey: string;
  userScrollGeneration: number;
}): ReadonlySet<string> {
  const messageIds = useMemo(
    () => messages.map((message) => message.id),
    [messages],
  );
  const input = useMemo<ConversationMessageRevealInput>(
    () => ({
      isActive,
      isTurnActive,
      messageIds,
      pendingPromptIds,
      userScrollGeneration,
    }),
    [
      isActive,
      isTurnActive,
      messageIds,
      pendingPromptIds,
      userScrollGeneration,
    ],
  );
  const transition = resolveConversationMessageRevealTransition(
    getConversationMessageRevealState(revealScopeKey),
    input,
  );

  useLayoutEffect(() => {
    // Commit after the DOM commit so StrictMode/concurrent render replays see
    // the same transition. Keeping the watermark outside the component makes
    // tab remounts and virtualizer recycling non-events rather than replays.
    // Re-resolve from the latest committed watermark for this pane/scroll
    // scope so concurrent render replays cannot publish stale transition data.
    const committedTransition = resolveConversationMessageRevealTransition(
      getConversationMessageRevealState(revealScopeKey),
      input,
    );
    setConversationMessageRevealState(
      revealScopeKey,
      committedTransition.nextState,
    );
  }, [input, revealScopeKey]);

  return useMemo(
    () =>
      transition.revealMessageIds.length > 0
        ? new Set(transition.revealMessageIds)
        : EMPTY_REVEAL_MESSAGE_IDS,
    [transition.revealMessageIds],
  );
}

function pendingRevealMatches(
  revealScopeKey: string,
  messageId: string,
  userScrollGeneration: number,
) {
  return (
    revealStateByScopeKey
      .get(revealScopeKey)
      ?.pendingRevealIds.get(messageId) === userScrollGeneration
  );
}

function consumePendingReveal(
  revealScopeKey: string,
  messageId: string,
) {
  const current = getConversationMessageRevealState(revealScopeKey);
  if (!current?.pendingRevealIds.has(messageId)) {
    return;
  }

  const nextPendingRevealIds = new Map(current.pendingRevealIds);
  // A matching entry has now painted once. A mismatched entry became stale
  // because the reader navigated before its virtualized shell mounted.
  nextPendingRevealIds.delete(messageId);
  setConversationMessageRevealState(revealScopeKey, {
    ...current,
    pendingRevealIds: nextPendingRevealIds,
  });
}

export function useConversationMessageRevealOnMount({
  messageId,
  revealScopeKey,
  revealInCurrentCommit,
  revealUserScrollGeneration,
  userScrollGeneration,
}: {
  messageId: string;
  revealScopeKey: string;
  revealInCurrentCommit: boolean;
  revealUserScrollGeneration: number;
  userScrollGeneration: number;
}) {
  const shouldRevealRef = useRef<boolean | null>(null);
  if (shouldRevealRef.current === null) {
    shouldRevealRef.current =
      (revealInCurrentCommit &&
        revealUserScrollGeneration === userScrollGeneration) ||
      pendingRevealMatches(revealScopeKey, messageId, userScrollGeneration);
  }

  useEffect(() => {
    // Passive consumption runs after the parent layout effect has published
    // this commit's pending ids. That ordering makes both an immediate mount
    // and the virtualizer's later mounted-range commit exactly-once, including
    // StrictMode's render/effect replay.
    consumePendingReveal(revealScopeKey, messageId);
  }, [messageId, revealScopeKey, userScrollGeneration]);

  return shouldRevealRef.current;
}
