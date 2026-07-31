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

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Message } from "../types";

export type MessageNavigationTargets = {
  prevMessageId: string | null;
  nextMessageId: string | null;
};

export type MessageNavigationKind = "delegation" | "userPrompt";
export type MessageNavigationDirection = "previous" | "next";

export type MessageNavigationLookup = (
  messageId: string,
  kind: MessageNavigationKind,
) => MessageNavigationTargets;

export type MessageNavigationContextValue = {
  getNavigationTargets: MessageNavigationLookup;
  hasOlderHistory: boolean;
  hasNewerHistory: boolean;
  jumpToMessageId: (messageId: string) => void;
  navigateToAdjacentMessage: (
    messageId: string,
    kind: MessageNavigationKind,
    direction: MessageNavigationDirection,
  ) => void;
};

const noopLookup: MessageNavigationLookup = () => ({
  prevMessageId: null,
  nextMessageId: null,
});
const noopJump = () => {};
const noopNavigate = () => {};

// Default leaves navigation buttons inert; cards opt in only when wrapped in
// `MessageNavigationProvider`.
const MessageNavigationContext = createContext<MessageNavigationContextValue>({
  getNavigationTargets: noopLookup,
  hasOlderHistory: false,
  hasNewerHistory: false,
  jumpToMessageId: noopJump,
  navigateToAdjacentMessage: noopNavigate,
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

type PendingAdjacentNavigation = {
  direction: MessageNavigationDirection;
  kind: MessageNavigationKind;
  messageId: string;
  token: number;
};

// Owns prompt/delegation navigation across the bounded resident transcript
// window. Resident targets jump synchronously. A target beyond either edge
// requests one bounded page at a time until the adjacent matching card appears
// or that history direction is exhausted.
export function usePagedMessageNavigation({
  hasNewerHistory,
  hasOlderHistory,
  jumpToMessageId,
  messages,
  requestNewerPage,
  requestOlderPage,
  sessionId,
}: {
  hasNewerHistory: boolean;
  hasOlderHistory: boolean;
  jumpToMessageId: (messageId: string) => void;
  messages: ReadonlyArray<Message>;
  requestNewerPage: () => Promise<boolean>;
  requestOlderPage: () => Promise<boolean>;
  sessionId: string;
}): MessageNavigationContextValue {
  const targetMaps = useMessageNavigationTargetMaps(messages);
  const getNavigationTargets = useMemo(
    () => makeMessageNavigationLookup(targetMaps),
    [targetMaps],
  );
  const [pendingNavigation, setPendingNavigation] =
    useState<PendingAdjacentNavigation | null>(null);
  const [requestSettledVersion, setRequestSettledVersion] = useState(0);
  const pendingNavigationRef = useRef<PendingAdjacentNavigation | null>(null);
  const requestInFlightTokenRef = useRef<number | null>(null);
  const nextNavigationTokenRef = useRef(1);

  pendingNavigationRef.current = pendingNavigation;

  const navigateToAdjacentMessage = useCallback(
    (
      messageId: string,
      kind: MessageNavigationKind,
      direction: MessageNavigationDirection,
    ) => {
      const targets = getNavigationTargets(messageId, kind);
      const residentTarget =
        direction === "previous"
          ? targets.prevMessageId
          : targets.nextMessageId;
      if (residentTarget !== null) {
        setPendingNavigation(null);
        jumpToMessageId(residentTarget);
        return;
      }
      const canLoad =
        direction === "previous" ? hasOlderHistory : hasNewerHistory;
      if (!canLoad) {
        return;
      }
      const nextPending = {
        direction,
        kind,
        messageId,
        token: nextNavigationTokenRef.current,
      };
      nextNavigationTokenRef.current += 1;
      pendingNavigationRef.current = nextPending;
      setPendingNavigation(nextPending);
    },
    [
      getNavigationTargets,
      hasNewerHistory,
      hasOlderHistory,
      jumpToMessageId,
    ],
  );

  useEffect(() => {
    if (!pendingNavigation) {
      return;
    }
    const {
      direction,
      kind,
      messageId,
      token,
    } = pendingNavigation;
    const targets = getNavigationTargets(messageId, kind);
    const residentTarget =
      direction === "previous"
        ? targets.prevMessageId
        : targets.nextMessageId;
    if (residentTarget !== null) {
      pendingNavigationRef.current = null;
      setPendingNavigation(null);
      jumpToMessageId(residentTarget);
      return;
    }
    const canLoad =
      direction === "previous" ? hasOlderHistory : hasNewerHistory;
    if (!canLoad) {
      pendingNavigationRef.current = null;
      setPendingNavigation(null);
      return;
    }
    if (requestInFlightTokenRef.current !== null) {
      return;
    }

    requestInFlightTokenRef.current = token;
    const requestPage =
      direction === "previous" ? requestOlderPage : requestNewerPage;
    void requestPage()
      .then((applied) => {
        if (
          !applied &&
          pendingNavigationRef.current?.token === token
        ) {
          pendingNavigationRef.current = null;
          setPendingNavigation(null);
        }
      })
      .finally(() => {
        if (requestInFlightTokenRef.current === token) {
          requestInFlightTokenRef.current = null;
        }
        if (pendingNavigationRef.current?.token === token) {
          setRequestSettledVersion((version) => version + 1);
        }
      });
  }, [
    getNavigationTargets,
    hasNewerHistory,
    hasOlderHistory,
    jumpToMessageId,
    pendingNavigation,
    requestNewerPage,
    requestOlderPage,
    requestSettledVersion,
  ]);

  useEffect(() => {
    requestInFlightTokenRef.current = null;
    pendingNavigationRef.current = null;
    setPendingNavigation(null);
  }, [sessionId]);

  return useMemo(
    () => ({
      getNavigationTargets,
      hasOlderHistory,
      hasNewerHistory,
      jumpToMessageId,
      navigateToAdjacentMessage,
    }),
    [
      getNavigationTargets,
      hasNewerHistory,
      hasOlderHistory,
      jumpToMessageId,
      navigateToAdjacentMessage,
    ],
  );
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
  const {
    getNavigationTargets,
    hasNewerHistory,
    hasOlderHistory,
    navigateToAdjacentMessage,
  } = useMessageNavigation();
  const targets = getNavigationTargets(messageId, kind);
  const canNavigatePrevious =
    targets.prevMessageId !== null || hasOlderHistory;
  const canNavigateNext =
    targets.nextMessageId !== null || hasNewerHistory;

  // Keep the controls visible at bounded-window edges whenever another history
  // page may contain the adjacent card. This is the state in which hiding the
  // group made prompt navigation disappear after jump-to-start.
  if (!canNavigatePrevious && !canNavigateNext) {
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
        disabled={!canNavigatePrevious}
        onClick={() => {
          if (canNavigatePrevious) {
            navigateToAdjacentMessage(messageId, kind, "previous");
          }
        }}
      >
        <span className="message-meta-jump-icon" aria-hidden="true">
          ↑
        </span>
      </button>
      <button
        type="button"
        className="ghost-button message-meta-jump-button"
        aria-label={labels.next}
        title={labels.next}
        disabled={!canNavigateNext}
        onClick={() => {
          if (canNavigateNext) {
            navigateToAdjacentMessage(messageId, kind, "next");
          }
        }}
      >
        <span className="message-meta-jump-icon" aria-hidden="true">
          ↓
        </span>
      </button>
    </span>
  );
}
