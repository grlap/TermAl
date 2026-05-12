// conversation-navigation.tsx
//
// Owns the in-conversation "jump to previous/next message of the same kind"
// context shared by message cards. Today this is used by the two card kinds
// that the user wants to navigate one-by-one through a long conversation:
//
//   - parallelAgents (each one marks a delegation event)
//   - text + author "you" (user prompts)
//
// It does NOT own marker rail navigation, search highlight handling, or the
// underlying virtualizer handle — those live in `conversation-markers.tsx` and
// `VirtualizedConversationMessageList.tsx`. The provider here just routes
// per-card prev/next lookups and a single `jumpToMessageId` callback so
// individual message cards can render small inline ⬆ / ⬇ buttons without
// pulling the whole transcript through their props.
//
// Split out of: `ui/src/panels/AgentSessionPanel.tsx` /
// `ui/src/message-cards.tsx`.

import { createContext, useContext, useMemo, type ReactNode } from "react";
import type { Message } from "../types";

export type MessageNavigationTargets = {
  prevMessageId: string | null;
  nextMessageId: string | null;
};

export type MessageNavigationKind = "delegation" | "userPrompt";

export type MessageNavigationLookup = (
  messageId: string,
  kind: MessageNavigationKind,
) => MessageNavigationTargets;

export type MessageNavigationContextValue = {
  getNavigationTargets: MessageNavigationLookup;
  jumpToMessageId: (messageId: string) => void;
};

const noopLookup: MessageNavigationLookup = () => ({
  prevMessageId: null,
  nextMessageId: null,
});
const noopJump = () => {};

// Default leaves navigation buttons inert; cards opt in only when wrapped in
// `MessageNavigationProvider`.
const MessageNavigationContext = createContext<MessageNavigationContextValue>({
  getNavigationTargets: noopLookup,
  jumpToMessageId: noopJump,
});

export function useMessageNavigation(): MessageNavigationContextValue {
  return useContext(MessageNavigationContext);
}

export function MessageNavigationProvider({
  children,
  value,
}: {
  children: ReactNode;
  value: MessageNavigationContextValue;
}) {
  return (
    <MessageNavigationContext.Provider value={value}>
      {children}
    </MessageNavigationContext.Provider>
  );
}

export type MessageNavigationTargetMaps = {
  delegation: Map<string, MessageNavigationTargets>;
  userPrompt: Map<string, MessageNavigationTargets>;
};

// Builds prev/next maps for the two card kinds the navigation buttons cover.
// Pure of React; exported so the buttons can be unit-tested in isolation.
export function buildMessageNavigationTargetMaps(
  messages: ReadonlyArray<Message>,
): MessageNavigationTargetMaps {
  return {
    delegation: buildTargetMap(messages, (message) => message.type === "parallelAgents"),
    userPrompt: buildTargetMap(
      messages,
      (message) => message.type === "text" && message.author === "you",
    ),
  };
}

export function useMessageNavigationTargetMaps(
  messages: ReadonlyArray<Message>,
): MessageNavigationTargetMaps {
  return useMemo(() => buildMessageNavigationTargetMaps(messages), [messages]);
}

function buildTargetMap(
  messages: ReadonlyArray<Message>,
  predicate: (message: Message) => boolean,
): Map<string, MessageNavigationTargets> {
  const ids: string[] = [];
  for (const message of messages) {
    if (predicate(message)) {
      ids.push(message.id);
    }
  }
  const targets = new Map<string, MessageNavigationTargets>();
  for (let index = 0; index < ids.length; index += 1) {
    const id = ids[index]!;
    targets.set(id, {
      prevMessageId: index > 0 ? ids[index - 1]! : null,
      nextMessageId: index < ids.length - 1 ? ids[index + 1]! : null,
    });
  }
  return targets;
}

export function makeMessageNavigationLookup(
  targetMaps: MessageNavigationTargetMaps,
): MessageNavigationLookup {
  return (messageId, kind) => {
    const map = kind === "delegation" ? targetMaps.delegation : targetMaps.userPrompt;
    return (
      map.get(messageId) ?? {
        prevMessageId: null,
        nextMessageId: null,
      }
    );
  };
}

const KIND_NAVIGATION_LABEL: Record<
  MessageNavigationKind,
  { prev: string; next: string; group: string }
> = {
  delegation: {
    prev: "Jump to previous delegation",
    next: "Jump to next delegation",
    group: "Delegation navigation",
  },
  userPrompt: {
    prev: "Jump to previous prompt",
    next: "Jump to next prompt",
    group: "Prompt navigation",
  },
};

// Renders the inline prev/next pair on a message card. Both buttons are kept
// in the DOM at all times so the layout doesn't shift when reaching either
// boundary; the unavailable one is rendered disabled.
export function MessageNavigationButtons({
  kind,
  messageId,
}: {
  kind: MessageNavigationKind;
  messageId: string;
}) {
  const { getNavigationTargets, jumpToMessageId } = useMessageNavigation();
  const targets = getNavigationTargets(messageId, kind);

  // If the message has no peer of the same kind in either direction the
  // buttons would always be inert; hide the group entirely so cards with a
  // single delegation or single prompt don't show decorative chrome.
  if (targets.prevMessageId === null && targets.nextMessageId === null) {
    return null;
  }

  const labels = KIND_NAVIGATION_LABEL[kind];

  return (
    <span
      className="message-meta-jump-controls"
      role="group"
      aria-label={labels.group}
    >
      <button
        type="button"
        className="ghost-button message-meta-jump-button"
        aria-label={labels.prev}
        title={labels.prev}
        disabled={targets.prevMessageId === null}
        onClick={() => {
          if (targets.prevMessageId !== null) {
            jumpToMessageId(targets.prevMessageId);
          }
        }}
      >
        <MessageNavigationUpIcon />
      </button>
      <button
        type="button"
        className="ghost-button message-meta-jump-button"
        aria-label={labels.next}
        title={labels.next}
        disabled={targets.nextMessageId === null}
        onClick={() => {
          if (targets.nextMessageId !== null) {
            jumpToMessageId(targets.nextMessageId);
          }
        }}
      >
        <MessageNavigationDownIcon />
      </button>
    </span>
  );
}

function MessageNavigationUpIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" width="12" height="12">
      <path
        d="M8 3.5 L3 9 H6 V12.5 H10 V9 H13 Z"
        fill="currentColor"
      />
    </svg>
  );
}

function MessageNavigationDownIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" width="12" height="12">
      <path
        d="M8 12.5 L3 7 H6 V3.5 H10 V7 H13 Z"
        fill="currentColor"
      />
    </svg>
  );
}
