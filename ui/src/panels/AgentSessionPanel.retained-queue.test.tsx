import {
  act,
  createEvent,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  StrictMode,
  useLayoutEffect,
  type ClipboardEvent as ReactClipboardEvent,
  type RefObject,
} from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import * as slashPalette from "./session-slash-palette";
import {
  AgentSessionPanel,
  AgentSessionPanelFooter,
} from "./AgentSessionPanel";
import { splitAgentCommandResolverTail } from "./session-agent-command-submission";
import { VirtualizedConversationMessageList } from "./VirtualizedConversationMessageList";
import { SessionActivityStrip } from "./session-activity-cards";
import { notifyMessageStackScrollWrite } from "../message-stack-scroll-sync";
import { MessageCard } from "../message-cards";
import { applyDeltaToSessions } from "../live-updates";
import {
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
  useSessionRecordSnapshot,
} from "../session-store";
import {
  DEFAULT_CONVERSATION_MARKER_COLOR,
  normalizeConversationMarkerColor,
} from "../conversation-marker-colors";
import {
  VIRTUALIZED_MESSAGE_GAP_PX,
  buildVirtualizedMessageLayout,
  clampVirtualizedViewportScrollTop,
  estimateConversationMessageHeight,
  getAdjustedVirtualizedScrollTopForHeightChange,
  getScrollContainerBottomGap,
  isScrollContainerAtPhysicalBottom,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import type {
  CommandMessage,
  ConversationMarker,
  DeltaEvent,
  DiffMessage,
  Message,
  Session,
} from "../types";
import { RESPONSE_BOARD_MESSAGE_MIME } from "../response-board";

function extractCssBlock(source: string, headerPattern: RegExp) {
  const match = source.match(headerPattern);
  if (!match || match.index === undefined) {
    throw new Error(`Missing CSS block matching ${headerPattern}`);
  }

  const openingBrace = source.indexOf("{", match.index + match[0].length);
  if (openingBrace < 0) {
    throw new Error(`Missing opening brace for CSS block ${headerPattern}`);
  }

  let depth = 0;
  let quote: '"' | "'" | null = null;
  let inComment = false;
  for (let index = openingBrace; index < source.length; index += 1) {
    const character = source[index];
    const nextCharacter = source[index + 1];
    if (inComment) {
      if (character === "*" && nextCharacter === "/") {
        inComment = false;
        index += 1;
      }
      continue;
    }
    if (quote) {
      if (character === "\\") {
        index += 1;
      } else if (character === quote) {
        quote = null;
      }
      continue;
    }
    if (character === "/" && nextCharacter === "*") {
      inComment = true;
      index += 1;
    } else if (character === '"' || character === "'") {
      quote = character;
    } else if (character === "{") {
      depth += 1;
    } else if (character === "}") {
      depth -= 1;
      if (depth === 0) {
        return source.slice(openingBrace + 1, index);
      }
    }
  }

  throw new Error(`Missing closing brace for CSS block ${headerPattern}`);
}

function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "test-model",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

function makeTextMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: index % 2 === 0 ? "you" : "assistant",
    text: `Message ${index + 1}`,
  }));
}

function installLongTranscriptScrollNodeMocks(scrollNode: HTMLElement) {
  const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
  let scrollTop = 20_000;
  let scrollHeight = 24_000;

  Object.defineProperty(scrollNode, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
  Object.defineProperty(scrollNode, "clientWidth", {
    configurable: true,
    get: () => 1000,
  });
  Object.defineProperty(scrollNode, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(scrollNode, "scrollTop", {
    configurable: true,
    get: () => scrollTop,
    set: (nextValue: number) => {
      scrollTop = nextValue;
    },
  });

  Element.prototype.getBoundingClientRect =
    function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      if (element === scrollNode) {
        return {
          bottom: 600,
          height: 600,
          left: 0,
          right: 1000,
          top: 0,
          width: 1000,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }
      if (element.classList.contains("virtualized-message-page")) {
        return {
          bottom: 600,
          height: 600,
          left: 0,
          right: 1000,
          top: 0,
          width: 1000,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }
      if (element.classList.contains("virtualized-message-slot")) {
        return {
          bottom: 80,
          height: 80,
          left: 0,
          right: 1000,
          top: 0,
          width: 1000,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }
      return originalGetBoundingClientRect.call(this);
    };

  return {
    cleanup() {
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    },
    setScrollTop(nextValue: number) {
      scrollTop = nextValue;
    },
    setScrollHeight(nextValue: number) {
      scrollHeight = nextValue;
    },
  };
}

function makeCommandMessages(count: number): CommandMessage[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `command-${index + 1}`,
    type: "command",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: "assistant",
    command: "pwd",
    output: ".",
    status: "success",
  }));
}

function makeDiffMessages(count: number): DiffMessage[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `diff-${index + 1}`,
    type: "diff",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: "assistant",
    filePath: `file-${index + 1}.ts`,
    summary: `Changed file ${index + 1}`,
    diff: "@@ -1 +1 @@\n-old\n+new",
    changeType: "edit",
  }));
}

function makeConversationMarker(
  input: Partial<ConversationMarker> &
    Pick<ConversationMarker, "id" | "messageId" | "name">,
): ConversationMarker {
  const { id, messageId, name, ...overrides } = input;
  return {
    id,
    sessionId: "session-1",
    kind: "decision",
    name,
    body: null,
    color: "#22c55e",
    messageId,
    messageIndexHint: 0,
    endMessageId: null,
    endMessageIndexHint: null,
    createdAt: "2026-05-01 10:00:00",
    updatedAt: "2026-05-01 10:00:00",
    createdBy: "user",
    ...overrides,
  };
}

const EMPTY_AGENT_COMMANDS: {
  kind?: "promptTemplate" | "nativeSlash";
  name: string;
  description: string;
  content: string;
  source: string;
  argumentHint?: string | null;
}[] = [];
const formatFooterByteSize = (byteSize: number) => `${byteSize} B`;
type AgentSessionPanelProps = Parameters<typeof AgentSessionPanel>[0];

function createAgentSessionPanelHarness(
  props: Partial<AgentSessionPanelProps> & {
    activeSession?: Session | null;
  } = {},
) {
  const { activeSession = null, ...panelProps } = props;
  const scrollContainerRef = { current: document.createElement("section") };
  const conversationSearchMatchedItemKeys = new Set<string>();

  syncComposerSessionsStore({
    sessions: activeSession ? [activeSession] : [],
    draftsBySessionId: {},
    draftAttachmentsBySessionId: {},
  });

  return (overrides: Partial<AgentSessionPanelProps> = {}) => (
    <AgentSessionPanel
      paneId="pane-1"
      viewMode="session"
      activeSessionId={activeSession?.id ?? null}
      isLoading={false}
      isUpdating={false}
      commandMessages={[]}
      diffMessages={[]}
      scrollContainerRef={scrollContainerRef}
      onApprovalDecision={() => {}}
      onUserInputSubmit={async () => {}}
      onMcpElicitationSubmit={() => {}}
      onCodexAppRequestSubmit={() => {}}
      onCancelQueuedPrompt={() => {}}
      onSessionSettingsChange={() => {}}
      conversationSearchQuery=""
      conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
      conversationSearchActiveItemKey={null}
      onConversationSearchItemMount={() => {}}
      renderCommandCard={() => null}
      renderDiffCard={() => null}
      renderMessageCard={(message) => (
        <article className="message-card">{message.id}</article>
      )}
      renderPromptSettings={() => null}
      {...panelProps}
      {...overrides}
    />
  );
}

function renderSessionPanelWithDefaults(
  props: Partial<AgentSessionPanelProps> & {
    activeSession?: Session | null;
  },
) {
  return render(createAgentSessionPanelHarness(props)());
}

function renderNavigableMessageCard(message: Message) {
  return (
    <>
      <MessageCard
        message={message}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onCodexAppRequestSubmit={() => {}}
      />
      <span>{message.id}</span>
    </>
  );
}

afterEach(() => {
  act(() => {
    resetSessionStoreForTesting();
  });
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function stubResolvedAgentCommand(response: {
  name: string;
  source: string;
  kind: "promptTemplate" | "nativeSlash";
  visiblePrompt: string;
  expandedPrompt?: string | null;
  title?: string | null;
  delegation?: {
    mode?: "reviewer" | "explorer" | "worker";
    title?: string | null;
    writePolicy?:
      | { kind: "readOnly" }
      | { kind: "sharedWorktree"; ownedPaths: string[] }
      | {
          kind: "isolatedWorktree";
          ownedPaths: string[];
          worktreePath?: string;
        };
  } | null;
}) {
  const fetchMock = vi.fn(
    async (_input: RequestInfo | URL, _init?: RequestInit) => {
      return new Response(JSON.stringify(response), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function lastJsonRequestBody(
  fetchMock: ReturnType<typeof stubResolvedAgentCommand>,
) {
  const lastCall = fetchMock.mock.calls[fetchMock.mock.calls.length - 1];
  const init = lastCall?.[1] as RequestInit | undefined;
  return JSON.parse(String(init?.body ?? "{}")) as unknown;
}

describe("AgentSessionPanel conversation caching", () => {
  it("does not render a queued prompt once the matching message is visible", () => {
    const activeSession = makeSession("session-a", {
      messages: [
        {
          id: "queued-prompt",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Queued prompt body",
        },
      ],
      pendingPrompts: [
        {
          id: "queued-prompt",
          timestamp: "10:00",
          text: "Queued prompt body",
        },
      ],
    });

    renderSessionPanelWithDefaults({ activeSession });

    expect(screen.getByText("queued-prompt")).toBeInTheDocument();
    expect(screen.queryByText("Queued prompt body")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Cancel queued prompt" }),
    ).not.toBeInTheDocument();
  });

  it.each(["Waiting/Unknown", "interrupted/unknown", "Defer", "store_corrupt"])(
    "keeps recovery actions without duplicating a retained transcript prompt after %s",
    (preview) => {
      const interrupted = preview === "interrupted/unknown" || preview === "store_corrupt";
      const onCancelQueuedPrompt = vi.fn();
      const onResumeSessionQueue = vi.fn();
      const activeSession = makeSession("session-held", {
        status: "idle",
        queuePaused: true,
        preview,
        messages: [{ id: "held", type: "text", author: "you", timestamp: "10:00", text: "Original exact prompt" }],
        pendingPrompts: [{
          id: "held",
          timestamp: "10:00",
          text: "Original exact prompt",
          isEngramRetained: true,
          engramInterrupted: interrupted,
        }],
      });
      renderSessionPanelWithDefaults({
        activeSession: preview === "Waiting/Unknown"
          ? { ...activeSession, status: "active", queuePaused: false, pendingPrompts: [] }
          : activeSession,
        onCancelQueuedPrompt,
        onResumeSessionQueue,
        renderMessageCard: (message) => <article>{message.type === "text" ? message.text : message.id}</article>,
      });
      if (preview === "Waiting/Unknown") {
        expect(screen.queryByRole("button", { name: "Cancel retained prompt" })).not.toBeInTheDocument();
        act(() => syncComposerSessionsStore({
          sessions: [activeSession], draftsBySessionId: {}, draftAttachmentsBySessionId: {},
        }));
      }
      expect(screen.getAllByText("Original exact prompt")).toHaveLength(1);
      expect(screen.getByText("Prompt retained")).toBeInTheDocument();
      if (interrupted) {
        expect(screen.queryByRole("button", { name: "Resume queued prompts" })).not.toBeInTheDocument();
        expect(screen.getByText(/Cancel this retained prompt or reconcile/)).toBeInTheDocument();
        expect(onResumeSessionQueue).not.toHaveBeenCalled();
      } else {
        fireEvent.click(screen.getByRole("button", { name: "Resume queued prompts" }));
        expect(onResumeSessionQueue).toHaveBeenCalledWith("session-held");
      }
      fireEvent.click(screen.getByRole("button", { name: "Cancel retained prompt" }));
      expect(onCancelQueuedPrompt).toHaveBeenCalledWith("session-held", "held");

      // The next server snapshot after cancellation removes only the actions.
      act(() => syncComposerSessionsStore({
        sessions: [{ ...activeSession, pendingPrompts: [] }],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      }));
      expect(screen.queryByRole("button", { name: "Cancel retained prompt" })).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Resume queued prompts" })).not.toBeInTheDocument();
      expect(screen.getAllByText("Original exact prompt")).toHaveLength(1);
    },
  );

  it("shows one retained prompt, one Defer card, and actionable Resume after the atomic SSE delta", () => {
    const preDefer = makeSession("session-held", {
      status: "active",
      preview: "Authorize exact prompt",
      queuePaused: false,
      messageCount: 1,
      sessionMutationStamp: 100,
      messages: [{
        id: "held",
        type: "text",
        author: "you",
        timestamp: "10:00",
        text: "Original exact prompt",
      }],
    });
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 7,
      sessionId: "session-held",
      messageId: "defer-card",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "defer-card",
        type: "engramControl",
        author: "assistant",
        timestamp: "10:01",
        schemaVersion: 1,
        stage: "dispatch",
        assurance: "authoritative",
        decision: "defer",
        dispatch: "queued",
        deferCode: "busy",
        latencyMs: { total: 5 },
        failMode: "enforced",
      },
      preview: "Engram deferred this prompt (busy). Prompt retained; resume to retry or cancel.",
      status: "idle",
      sessionQueue: {
        queuePaused: true,
        pendingPrompts: [{
          id: "held",
          timestamp: "10:00",
          text: "Original exact prompt",
          isEngramRetained: true,
          engramInterrupted: false,
        }],
      },
      sessionMutationStamp: 101,
    };
    const applied = applyDeltaToSessions([preDefer], delta);
    if (applied.kind !== "applied") {
      throw new Error(`expected applied Defer delta, received ${applied.kind}`);
    }
    const onResumeSessionQueue = vi.fn();

    renderSessionPanelWithDefaults({
      activeSession: applied.sessions[0],
      onResumeSessionQueue,
      renderMessageCard: (message) => (
        <article>{message.type === "text" ? message.text : message.id}</article>
      ),
    });

    expect(screen.getAllByText("Original exact prompt")).toHaveLength(1);
    expect(screen.getAllByText("defer-card")).toHaveLength(1);
    expect(screen.getByText("Prompt retained")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Resume queued prompts" }));
    expect(onResumeSessionQueue).toHaveBeenCalledOnce();
    expect(onResumeSessionQueue).toHaveBeenCalledWith("session-held");
  });

  it("keeps an unpromoted retained head before its optimistic successor", () => {
    const onCancelQueuedPrompt = vi.fn();
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-unpromoted-retained", {
        status: "idle",
        queuePaused: true,
        messages: [],
        pendingPrompts: [
          {
            id: "unpromoted-retained",
            timestamp: "10:00",
            text: "Exact retained prompt without a transcript row",
            isEngramRetained: true,
            engramInterrupted: true,
          },
          {
            id: "optimistic-successor",
            timestamp: "10:01",
            text: "Optimistic successor behind retained head",
            localOnly: true,
          },
        ],
      }),
      onCancelQueuedPrompt,
      onResumeSessionQueue: vi.fn(),
    });

    expect(screen.getByText("Prompt retained")).toBeInTheDocument();
    expect(screen.getByText(/Cancel this retained prompt or reconcile/)).toBeInTheDocument();
    const retainedHead = screen.getByText("Exact retained prompt without a transcript row");
    const optimisticSuccessor = screen.getByText("Optimistic successor behind retained head");
    expect(
      retainedHead.compareDocumentPosition(optimisticSuccessor) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Resume queued prompts" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel queued prompt" }));
    expect(onCancelQueuedPrompt).toHaveBeenCalledWith(
      "session-unpromoted-retained",
      "optimistic-successor",
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel retained prompt" }));
    expect(onCancelQueuedPrompt).toHaveBeenCalledWith(
      "session-unpromoted-retained",
      "unpromoted-retained",
    );
  });

  it("registers and highlights an unpromoted retained prompt for conversation search", () => {
    const onConversationSearchItemMount = vi.fn();
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-search-retained", {
        status: "idle",
        queuePaused: true,
        messages: [],
        pendingPrompts: [{
          id: "retained-search-target",
          timestamp: "10:00",
          text: "Searchable retained prompt",
          isEngramRetained: true,
          engramInterrupted: true,
        }],
      }),
      conversationSearchQuery: "retained",
      conversationSearchMatchedItemKeys: new Set([
        "pendingPrompt:retained-search-target",
      ]),
      conversationSearchActiveItemKey: "pendingPrompt:retained-search-target",
      onConversationSearchItemMount,
    });

    expect(onConversationSearchItemMount).toHaveBeenCalledWith(
      "pendingPrompt:retained-search-target",
      expect.any(HTMLElement),
    );
    const slot = document.querySelector(
      '[data-session-search-item-key="pendingPrompt:retained-search-target"]',
    );
    expect(slot).toHaveClass("session-search-hit", "session-search-hit-active");
    expect(slot?.querySelector("mark.search-highlight.is-active")).toHaveTextContent(
      "retained",
    );
  });

  it("suppresses a duplicate pending search slot when the retained body is in the transcript", () => {
    const onConversationSearchItemMount = vi.fn();
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-search-retained-visible", {
        status: "idle",
        queuePaused: true,
        messages: [{
          id: "retained-search-target",
          type: "text",
          author: "you",
          timestamp: "10:00",
          text: "Searchable retained prompt",
        }],
        pendingPrompts: [{
          id: "retained-search-target",
          timestamp: "10:00",
          text: "Searchable retained prompt",
          isEngramRetained: true,
        }],
      }),
      conversationSearchQuery: "retained",
      conversationSearchMatchedItemKeys: new Set([
        "message:retained-search-target",
        "pendingPrompt:retained-search-target",
      ]),
      conversationSearchActiveItemKey: "message:retained-search-target",
      onConversationSearchItemMount,
      renderMessageCard: (message) => (
        <article>{message.type === "text" ? message.text : message.id}</article>
      ),
    });

    expect(
      document.querySelector(
        '[data-session-search-item-key="pendingPrompt:retained-search-target"]',
      ),
    ).not.toBeInTheDocument();
    expect(onConversationSearchItemMount).not.toHaveBeenCalledWith(
      "pendingPrompt:retained-search-target",
      expect.anything(),
    );
    expect(screen.getAllByText("Searchable retained prompt")).toHaveLength(1);
  });

  it("keeps an evicted interrupted retained head before its queued successor", () => {
    const onCancelQueuedPrompt = vi.fn();
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-evicted-retained", {
        status: "idle",
        queuePaused: true,
        messages: [{
          id: "resident-tail",
          type: "text",
          author: "assistant",
          timestamp: "10:01",
          text: "Resident transcript tail",
        }],
        pendingPrompts: [
          {
            id: "evicted-retained-head",
            timestamp: "10:00",
            text: "Evicted retained head",
            isEngramRetained: true,
            engramInterrupted: true,
          },
          {
            id: "queued-successor",
            timestamp: "10:02",
            text: "Queued successor behind retained head",
          },
        ],
      }),
      onCancelQueuedPrompt,
      onResumeSessionQueue: vi.fn(),
    });

    expect(screen.getByText("Prompt retained")).toBeInTheDocument();
    const retainedHead = screen.getByText("Evicted retained head");
    const queuedSuccessor = screen.getByText("Queued successor behind retained head");
    expect(
      retainedHead.compareDocumentPosition(queuedSuccessor) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Cancel retained prompt" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Cancel queued prompt" })).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "Resume queued prompts" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel retained prompt" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel queued prompt" }));
    expect(onCancelQueuedPrompt).toHaveBeenNthCalledWith(
      1,
      "session-evicted-retained",
      "evicted-retained-head",
    );
    expect(onCancelQueuedPrompt).toHaveBeenNthCalledWith(
      2,
      "session-evicted-retained",
      "queued-successor",
    );
  });

  it("does not carry deferred transcript cards into an empty active session", async () => {
    const sessionA = makeSession("session-a", {
      messages: [
        {
          author: "assistant",
          id: "message-a",
          text: "Session A transcript",
          timestamp: "10:00",
          type: "text",
        },
      ],
    });
    const sessionB = makeSession("session-b", { messages: [] });
    syncComposerSessionsStore({
      sessions: [sessionA, sessionB],
      draftsBySessionId: {},
      draftAttachmentsBySessionId: {},
    });

    const renderPanel = (activeSessionId: string) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSessionId}
        isLoading={false}
        isUpdating={false}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onCancelQueuedPrompt={() => {}}
        onSessionSettingsChange={() => {}}
        conversationSearchQuery=""
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onConversationSearchItemMount={() => {}}
        renderCommandCard={() => null}
        renderDiffCard={() => null}
        renderMessageCard={(message) => (
          <article className="message-card">
            {message.type === "text" ? message.text : message.id}
          </article>
        )}
        renderPromptSettings={() => null}
      />
    );

    const { rerender } = render(renderPanel(sessionA.id));
    await waitFor(() => {
      expect(screen.getByText("Session A transcript")).toBeInTheDocument();
    });

    await act(async () => {
      rerender(renderPanel(sessionB.id));
      await Promise.resolve();
    });

    expect(screen.queryByText("Session A transcript")).not.toBeInTheDocument();
    expect(screen.getByText("Live session is ready")).toBeInTheDocument();
  });

  it("keeps pending prompts in the shared live tail without a live turn", () => {
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-prompt-a",
            timestamp: "10:02",
            text: "Queued follow-up without a live turn",
          },
        ],
      }),
    });

    const queuedPromptCard = screen
      .getByText("Queued follow-up without a live turn")
      .closest(".pending-prompt-card");
    const pendingPromptQueue = queuedPromptCard?.closest(
      ".conversation-pending-prompts",
    );

    expect(queuedPromptCard).not.toBeNull();
    expect(pendingPromptQueue).not.toBeNull();
    expect(pendingPromptQueue?.closest(".conversation-queued-tail")).not.toBeNull();
    expect(pendingPromptQueue).toContainElement(
      queuedPromptCard as HTMLElement,
    );
    expect(
      within(queuedPromptCard as HTMLElement).getByText("You"),
    ).toBeInTheDocument();
  });

  it("keeps a stable tail while an idle session advances a queued turn", () => {
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        status: "idle",
        messages: [
          {
            id: "message-user",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "First prompt",
          },
          {
            id: "message-assistant",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "First response",
          },
        ],
        pendingPrompts: [
          {
            id: "pending-prompt-a",
            timestamp: "10:02",
            text: "Queued follow-up",
          },
        ],
      }),
    });

    const queuedPromptCard = document.querySelector(".pending-prompt-card");
    const queuedTail = queuedPromptCard?.closest(".conversation-queued-tail");
    expect(queuedTail).toContainElement(queuedPromptCard as HTMLElement);
    expect(queuedTail?.querySelectorAll(".pending-prompt-card")).toHaveLength(1);
    expect(document.querySelector(".activity-card-queue-handoff")).toBeNull();
    expect(screen.getByRole("button", { name: "Cancel queued prompt" })).toBeInTheDocument();
  });

  it("shows the paused-queue card with a Resume action instead of the handoff spinner after Stop", () => {
    const onResumeSessionQueue = vi.fn();
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        status: "idle",
        queuePaused: true,
        messages: [
          {
            id: "message-user",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "First prompt",
          },
          {
            id: "message-stopped",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Turn stopped by user.",
          },
        ],
        pendingPrompts: [
          {
            id: "pending-prompt-a",
            timestamp: "10:02",
            text: "[TermAl mailbox notification] durable wake",
          },
        ],
      }),
      onResumeSessionQueue,
    });

    const pausedCard = screen
      .getByText("Queue paused")
      .closest(".activity-card-queue-paused");
    expect(pausedCard).not.toBeNull();
    expect(
      screen.getByText("Codex was stopped; the queue is paused"),
    ).toBeInTheDocument();
    expect(screen.getByText(/1 prompt waiting/)).toBeInTheDocument();
    expect(
      screen.queryByText("Codex is starting the next turn"),
    ).not.toBeInTheDocument();
    expect(pausedCard?.querySelector(".activity-spinner")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Resume queued prompts" }));
    expect(onResumeSessionQueue).toHaveBeenCalledWith("session-a");
  });

  it("does not render Engram recovery actions for an ordinary stopped queue prompt", () => {
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        status: "idle",
        queuePaused: true,
        messages: [
          {
            id: "ordinary-stopped-prompt",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Ordinary stopped prompt",
          },
        ],
        pendingPrompts: [
          {
            id: "ordinary-stopped-prompt",
            timestamp: "10:00",
            text: "Ordinary stopped prompt",
          },
        ],
      }),
    });

    expect(screen.queryByText("Prompt retained")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Cancel retained prompt" }),
    ).not.toBeInTheDocument();
  });

  it("labels a queued peer prompt with its sender session name", () => {
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-peer-prompt",
            timestamp: "10:03",
            text: "Message waiting from another session",
            source: {
              sessionId: "session-1743",
              name: "Comovo",
            },
          },
        ],
      }),
    });

    const queuedPromptCard = screen
      .getByText("Message waiting from another session")
      .closest(".pending-prompt-card") as HTMLElement;

    expect(within(queuedPromptCard).getByText("Comovo")).toBeInTheDocument();
    expect(within(queuedPromptCard).queryByText("You")).not.toBeInTheDocument();
  });

  it("renders queued mailbox wakeups as compact launch cards without changing queue controls", () => {
    const onCancelQueuedPrompt = vi.fn();
    const onOpenMailbox = vi.fn();
    const activationText = [
      "[TermAl mailbox notification]",
      "Mailbox `mailbox-queued` has 2 unread message(s). Latest inbound: #7 from Termal::Fable.",
      "First use `termal_list_mailboxes`, then `termal_read_mailbox`, then `termal_acknowledge_mailbox`.",
    ].join("\n");

    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-mailbox-wakeup",
            timestamp: "10:04",
            text: activationText,
            source: {
              kind: "mailbox",
              sessionId: "session-fable",
              name: "Termal::Fable",
              mailbox: {
                mailboxId: "mailbox-queued",
                messageId: "mailbox-message-7",
                sequence: 7,
                unreadCount: 2,
              },
            },
          },
        ],
      }),
      onCancelQueuedPrompt,
      onOpenMailbox,
    });

    expect(
      screen.queryByText(/termal_list_mailboxes/i),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Mailbox notification #7")).toBeInTheDocument();
    expect(screen.getAllByText("Termal::Fable").length).toBeGreaterThan(0);
    expect(screen.getByText("2 unread")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open mailbox →" }));
    expect(onOpenMailbox).toHaveBeenCalledWith("mailbox-queued");

    fireEvent.click(
      screen.getByRole("button", { name: "Cancel queued prompt" }),
    );
    expect(onCancelQueuedPrompt).toHaveBeenCalledWith(
      "session-a",
      "pending-mailbox-wakeup",
    );
  });

  it("collapses a long queued peer prompt inside the live tail", () => {
    const finalFinding = "Queued peer report final finding.";
    const peerText = [
      "Greg asked me to send you this review.",
      "",
      "Detailed queued finding. ".repeat(40),
      finalFinding,
    ].join("\n");

    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-long-peer-prompt",
            timestamp: "10:04",
            text: peerText,
            source: {
              sessionId: "session-investor",
              name: "Investor",
            },
          },
        ],
      }),
    });

    const queuedPromptCard = screen
      .getByText("Greg asked me to send you this review.")
      .closest(".pending-prompt-card") as HTMLElement;

    expect(within(queuedPromptCard).getByText("Investor")).toBeInTheDocument();
    expect(
      within(queuedPromptCard).queryByText(finalFinding),
    ).not.toBeInTheDocument();

    fireEvent.click(
      within(queuedPromptCard).getByRole("button", {
        name: "Show full message",
      }),
    );

    expect(
      queuedPromptCard.querySelector(".expandable-session-message"),
    ).toHaveClass("is-expanded");
    expect(
      queuedPromptCard.querySelector(".long-peer-message-copy"),
    ).toHaveTextContent(finalFinding);
  });

  it("labels and collapses a mixed-sender queued peer batch", () => {
    const finalFinding = "The second queued peer finding.";
    const peerBatchText = [
      "[TermAl cross-session message batch]",
      "2 pending messages, FIFO, newest last.",
      "Detailed queued batch message. ".repeat(30),
      finalFinding,
    ].join("\n");

    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-peer-batch",
            timestamp: "10:05",
            text: peerBatchText,
            source: { kind: "peerBatch", name: "Peer queue" },
          },
        ],
      }),
    });

    const queuedPromptCard = screen
      .getByText("[TermAl cross-session message batch]")
      .closest(".pending-prompt-card") as HTMLElement;

    expect(
      within(queuedPromptCard).getByText("Peer queue"),
    ).toBeInTheDocument();
    expect(
      within(queuedPromptCard).queryByText(finalFinding),
    ).not.toBeInTheDocument();
    expect(
      within(queuedPromptCard).getByRole("button", {
        name: "Show full message",
      }),
    ).toBeInTheDocument();
  });

  it("labels and collapses a queued delegation result prompt", () => {
    const fanInText = [
      "Codex + Claude /review-code fan-in",
      "",
      "Wait id: `delegation-wait-1234`",
      "Mode: `all`",
      "Parent session: `session-630`",
      "",
      "Delegations:",
      "- `delegation-codex`: completed - Codex /review-code",
      "- `delegation-claude`: completed - Claude /review-code",
      "",
      "Results:",
      "### Codex /review-code",
      "No findings.",
    ].join("\n");

    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-delegation-fan-in",
            timestamp: "10:04",
            text: fanInText,
          },
        ],
      }),
    });

    const queuedPromptCard = screen
      .getByText("Codex + Claude /review-code fan-in", { exact: false })
      .closest(".pending-prompt-card") as HTMLElement;

    expect(within(queuedPromptCard).getByText("Fan-in")).toBeInTheDocument();
    expect(within(queuedPromptCard).queryByText("You")).not.toBeInTheDocument();
    expect(
      within(queuedPromptCard).queryByText("No findings."),
    ).not.toBeInTheDocument();

    fireEvent.click(
      within(queuedPromptCard).getByRole("button", {
        name: "Show delegation results",
      }),
    );

    expect(
      queuedPromptCard.querySelector(".delegation-fan-in-message"),
    ).toHaveClass("is-expanded");
    expect(
      queuedPromptCard.querySelector(".delegation-fan-in-results"),
    ).not.toBeNull();
    expect(
      queuedPromptCard.querySelector(".prompt-expansion-copy"),
    ).toHaveTextContent("No findings.");
  });

  it("allows canceling a local-only optimistic pending prompt", () => {
    const onCancelQueuedPrompt = vi.fn();
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "optimistic-send-session-a-abc-1",
            timestamp: "10:02",
            text: "Optimistic follow-up",
            localOnly: true,
          },
        ],
      }),
      onCancelQueuedPrompt,
    });

    expect(screen.getByText("Optimistic follow-up")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel queued prompt" }),
    );
    expect(onCancelQueuedPrompt).toHaveBeenCalledWith(
      "session-a",
      "optimistic-send-session-a-abc-1",
    );
  });

  it("does not attach a native tooltip to queued prompt cancel buttons", () => {
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        messages: [],
        pendingPrompts: [
          {
            id: "pending-prompt-a",
            timestamp: "10:02",
            text: "Queued follow-up",
          },
        ],
      }),
    });

    expect(
      screen.getByRole("button", { name: "Cancel queued prompt" }),
    ).not.toHaveAttribute("title");
  });

  it("keeps the queued tail mounted when explicit turn state becomes idle", async () => {
    vi.useFakeTimers();
    const activeSession = makeSession("session-a", {
      status: "active",
      messages: [
        {
          id: "message-user",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
      ],
      pendingPrompts: [
        {
          id: "pending-prompt-a",
          timestamp: "10:02",
          text: "Queued follow-up after current turn",
        },
      ],
    });
    const renderPanel = createAgentSessionPanelHarness({
      activeSession,
    });
    const { rerender } = render(renderPanel({ liveTailPinned: true }));

    const liveTail = document.querySelector(".conversation-queued-tail");
    const queuedPromptCard = screen
      .getByText("Queued follow-up after current turn")
      .closest(".pending-prompt-card");
    expect(liveTail).not.toBeNull();
    expect(liveTail).toContainElement(queuedPromptCard as HTMLElement);

    await act(async () => {
      await vi.advanceTimersToNextTimerAsync();
    });

    act(() => {
      syncComposerSessionsStore({
        sessions: [{ ...activeSession, status: "idle" }],
        draftsBySessionId: {}, draftAttachmentsBySessionId: {},
      });
    });
    rerender(renderPanel());

    expect(screen.queryByText("Live turn")).not.toBeInTheDocument();
    expect(
      screen
        .getByText("Queued follow-up after current turn")
        .closest(".pending-prompt-card"),
    ).toBe(queuedPromptCard);
    expect(liveTail).toBeInTheDocument();
    expect(
      screen.getByText("Queued follow-up after current turn"),
    ).toBeInTheDocument();
    expect(
      screen
        .getByText("Queued follow-up after current turn")
        .closest(".conversation-queued-tail"),
    ).toBe(liveTail);
  });


});
