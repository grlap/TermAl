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
  splitAgentCommandResolverTail,
} from "./AgentSessionPanel";
import { buildConversationOverviewTailItems } from "./conversation-overview-controller";
import { VirtualizedConversationMessageList } from "./VirtualizedConversationMessageList";
import { RunningIndicator } from "./session-activity-cards";
import { notifyMessageStackScrollWrite } from "../message-stack-scroll-sync";
import { MessageCard } from "../message-cards";
import {
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
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
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import type {
  CommandMessage,
  ConversationMarker,
  DiffMessage,
  Message,
  Session,
} from "../types";

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
  const originalGetBoundingClientRect =
    Element.prototype.getBoundingClientRect;
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
  input: Partial<ConversationMarker> & Pick<ConversationMarker, "id" | "messageId" | "name">,
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
      showWaitingIndicator={false}
      waitingIndicatorPrompt={null}
      commandMessages={[]}
      diffMessages={[]}
      scrollContainerRef={scrollContainerRef}
      onApprovalDecision={() => {}}
      onUserInputSubmit={() => {}}
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
        onUserInputSubmit={() => {}}
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
      | { kind: "isolatedWorktree"; ownedPaths: string[]; worktreePath?: string };
  } | null;
}) {
  const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => {
    return new Response(JSON.stringify(response), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function lastJsonRequestBody(fetchMock: ReturnType<typeof stubResolvedAgentCommand>) {
  const lastCall = fetchMock.mock.calls[fetchMock.mock.calls.length - 1];
  const init = lastCall?.[1] as RequestInit | undefined;
  return JSON.parse(String(init?.body ?? "{}")) as unknown;
}

describe("splitAgentCommandResolverTail", () => {
  it.each([
    [
      "space-delimited",
      "3 -- Please add tests.",
      { argumentsText: "3", noteText: "Please add tests." },
    ],
    [
      "newline-delimited",
      "3\n--\nPlease add tests.",
      { argumentsText: "3", noteText: "Please add tests." },
    ],
    [
      "tab-delimited",
      "3\t--\tPlease add tests.",
      { argumentsText: "3", noteText: "Please add tests." },
    ],
    [
      "note-only",
      "-- Please add tests.",
      { argumentsText: "", noteText: "Please add tests." },
    ],
    ["empty trailing note", "3 --", { argumentsText: "3" }],
    [
      "attached dash is an argument",
      "3 --flag",
      { argumentsText: "3 --flag" },
    ],
  ])("splits %s separators", (_caseName, input, expected) => {
    expect(splitAgentCommandResolverTail(input)).toEqual(expected);
  });
});

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
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
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

  it("refreshes same-id assistant text through the virtualized component path", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const messages = makeTextMessages(82);
      const oldAssistant: Extract<Message, { type: "text" }> = {
        author: "assistant",
        id: messages[81].id,
        timestamp: messages[81].timestamp,
        type: "text",
        text: "Old streamed answer",
      };
      const initialSession = makeSession("session-a", {
        messages: [...messages.slice(0, 81), oldAssistant],
        pendingPrompts: [
          {
            id: "message-81",
            text: "Queued prompt duplicate",
            timestamp: "11:21",
          },
        ],
      });
      const currentAssistant: Extract<Message, { type: "text" }> = {
        ...oldAssistant,
        text: "Old streamed answer plus the latest chunk",
      };
      const updatedSession = makeSession("session-a", {
        messages: [...messages.slice(0, 81), currentAssistant],
        pendingPrompts: initialSession.pendingPrompts,
      });
      const renderPanel = () => (
        <AgentSessionPanel
          paneId="pane-1"
          viewMode="session"
          activeSessionId="session-a"
          isLoading={false}
          isUpdating={false}
          showWaitingIndicator={false}
          waitingIndicatorPrompt={null}
          commandMessages={[]}
          diffMessages={[]}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
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
              <span>{message.id}</span>
              <span>{message.type === "text" ? message.text : message.id}</span>
            </article>
          )}
          renderPromptSettings={() => null}
        />
      );

      syncComposerSessionsStore({
        sessions: [initialSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
      const { container } = render(renderPanel());

      await waitFor(() => {
        expect(container.querySelector(".virtualized-message-list")).not.toBeNull();
        expect(screen.getByText("Old streamed answer")).toBeInTheDocument();
      });
      expect(screen.queryByText("Queued prompt duplicate")).not.toBeInTheDocument();

      act(() => {
        syncComposerSessionsStore({
          sessions: [updatedSession],
          draftsBySessionId: {},
          draftAttachmentsBySessionId: {},
        });
      });

      await waitFor(() => {
        expect(
          screen.getByText("Old streamed answer plus the latest chunk"),
        ).toBeInTheDocument();
      });
      expect(screen.queryByText("Old streamed answer")).not.toBeInTheDocument();
      expect(screen.queryByText("Queued prompt duplicate")).not.toBeInTheDocument();
      expect(container.querySelector(".virtualized-message-list")).not.toBeNull();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
    }
  });

  it("renders pending prompts outside the live tail when no live turn is visible", () => {
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
      showWaitingIndicator: false,
    });

    const queuedPromptCard = screen
      .getByText("Queued follow-up without a live turn")
      .closest(".pending-prompt-card");
    const pendingPromptQueue = queuedPromptCard?.closest(
      ".conversation-pending-prompts",
    );

    expect(queuedPromptCard).not.toBeNull();
    expect(pendingPromptQueue).not.toBeNull();
    expect(
      document.querySelector(".conversation-live-tail"),
    ).not.toBeInTheDocument();
    expect(pendingPromptQueue).toContainElement(
      queuedPromptCard as HTMLElement,
    );
  });

  it("does not expose cancel for local-only optimistic pending prompts", () => {
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
      showWaitingIndicator: false,
    });

    expect(screen.getByText("Optimistic follow-up")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Cancel queued prompt" }),
    ).not.toBeInTheDocument();
  });

  it("keeps the live-turn tail calm while assistant output grows above queued prompts", () => {
    const scrollNode = document.createElement("section");
    let scrollTop = 120;
    const scrollWrites: number[] = [];
    const userMessage: Message = {
      id: "message-user",
      type: "text",
      timestamp: "10:00",
      author: "you",
      text: "Current prompt",
    };
    const firstPendingPrompt = {
      id: "pending-prompt-a",
      timestamp: "10:02",
      text: "Queued follow-up A",
    };
    const secondPendingPrompt = {
      id: "pending-prompt-b",
      timestamp: "10:03",
      text: "Queued follow-up B",
    };

    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 600,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        status: "active",
        messages: [
          userMessage,
          {
            id: "message-assistant",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Partial reply",
          },
        ],
        pendingPrompts: [firstPendingPrompt, secondPendingPrompt],
      }),
      scrollContainerRef: { current: scrollNode },
      showWaitingIndicator: true,
      waitingIndicatorPrompt: "Current prompt",
    });

    const liveTail = screen
      .getByText("Live turn")
      .closest(".conversation-live-tail");
    const liveTurnCard = screen
      .getByText("Live turn")
      .closest(".activity-card-live");
    const firstQueuedPromptCard = screen
      .getByText("Queued follow-up A")
      .closest(".pending-prompt-card");
    const secondQueuedPromptCard = screen
      .getByText("Queued follow-up B")
      .closest(".pending-prompt-card");
    const pendingPromptQueue = firstQueuedPromptCard?.closest(
      ".conversation-pending-prompts",
    );
    expect(liveTail).not.toBeNull();
    expect(firstQueuedPromptCard).not.toBeNull();
    expect(secondQueuedPromptCard).not.toBeNull();
    expect(pendingPromptQueue).not.toBeNull();
    expect(liveTail).toHaveClass("is-pinned");
    expect(liveTurnCard).not.toBeNull();
    const liveTailChildren = Array.from((liveTail as HTMLElement).children);
    expect(liveTailChildren[liveTailChildren.length - 1]).toHaveTextContent(
      "Live turn",
    );
    expect(pendingPromptQueue?.closest(".conversation-live-tail")).toBe(
      liveTail,
    );
    expect(
      Boolean(
        firstQueuedPromptCard!.compareDocumentPosition(secondQueuedPromptCard!) &
          Node.DOCUMENT_POSITION_FOLLOWING,
      ),
    ).toBe(true);
    expect(
      Boolean(
        secondQueuedPromptCard!.compareDocumentPosition(liveTurnCard!) &
          Node.DOCUMENT_POSITION_FOLLOWING,
      ),
    ).toBe(true);
    expect(liveTail).toContainElement(firstQueuedPromptCard as HTMLElement);
    expect(liveTail).toContainElement(secondQueuedPromptCard as HTMLElement);
    expect(liveTail).toContainElement(liveTurnCard as HTMLElement);
    expect(pendingPromptQueue).toContainElement(
      firstQueuedPromptCard as HTMLElement,
    );
    expect(pendingPromptQueue).toContainElement(
      secondQueuedPromptCard as HTMLElement,
    );
    expect(scrollTop).toBe(120);
    scrollWrites.length = 0;

    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-a", {
            status: "active",
            messages: [
              userMessage,
              {
                id: "message-assistant",
                type: "text",
                timestamp: "10:01",
                author: "assistant",
                text: "Partial reply with enough streamed content to grow above the live tail",
              },
            ],
            pendingPrompts: [firstPendingPrompt, secondPendingPrompt],
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    expect(scrollTop).toBe(120);
    expect(scrollWrites).toEqual([]);
  });

  it("only pins the live-turn tail while bottom follow is active", () => {
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
    });
    const renderPanel = createAgentSessionPanelHarness({
      activeSession,
      showWaitingIndicator: true,
      waitingIndicatorPrompt: "Current prompt",
    });
    const { rerender } = render(renderPanel({ liveTailPinned: true }));

    const liveTail = screen
      .getByText("Live turn")
      .closest(".conversation-live-tail");
    expect(liveTail).not.toBeNull();
    expect(liveTail).toHaveClass("is-pinned");

    rerender(renderPanel({ liveTailPinned: false }));
    expect(liveTail).not.toHaveClass("is-pinned");
  });

  it("removes the live-turn tail when the waiting indicator clears", () => {
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
      showWaitingIndicator: true,
      waitingIndicatorPrompt: "Current prompt",
    });
    const { rerender } = render(renderPanel({ liveTailPinned: true }));

    const liveTail = screen
      .getByText("Live turn")
      .closest(".conversation-live-tail");
    const queuedPromptCard = screen
      .getByText("Queued follow-up after current turn")
      .closest(".pending-prompt-card");
    expect(liveTail).not.toBeNull();
    expect(liveTail).toContainElement(queuedPromptCard as HTMLElement);

    rerender(
      renderPanel({
        showWaitingIndicator: false,
        waitingIndicatorPrompt: null,
      }),
    );

    expect(screen.queryByText("Live turn")).not.toBeInTheDocument();
    expect(
      document.querySelector(".conversation-live-tail"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("Queued follow-up after current turn"),
    ).toBeInTheDocument();
    expect(
      screen
        .getByText("Queued follow-up after current turn")
        .closest(".conversation-live-tail"),
    ).toBeNull();
  });

  it("suppresses a stale idle live-turn tail after visible agent output", () => {
    const activeSession = makeSession("session-a", {
      status: "idle",
      messages: [
        {
          id: "message-user",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-files",
          type: "fileChanges",
          timestamp: "10:01",
          author: "assistant",
          title: "Agent changed 1 file",
          files: [{ path: "ui/src/styles.css", kind: "modified" }],
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      showWaitingIndicator: true,
      waitingIndicatorPrompt: null,
    });

    expect(screen.queryByText("Live turn")).not.toBeInTheDocument();
  });

  it("keeps delegation wait tails visible after prior assistant output", () => {
    const activeSession = makeSession("session-a", {
      status: "idle",
      messages: [
        {
          id: "message-user",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Run delegated review",
        },
        {
          id: "message-files",
          type: "fileChanges",
          timestamp: "10:01",
          author: "assistant",
          title: "Agent changed 1 file",
          files: [{ path: "ui/src/styles.css", kind: "modified" }],
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      showWaitingIndicator: true,
      waitingIndicatorKind: "delegationWait",
      waitingIndicatorPrompt:
        "Waiting on 1 delegation wait covering 2 delegated sessions: review fan-in",
    });

    expect(screen.getByText("Live turn")).toBeInTheDocument();
    expect(screen.getByRole("tooltip")).toHaveTextContent(
      "Waiting on 1 delegation wait covering 2 delegated sessions: review fan-in",
    );
  });

  it("keeps send waiting feedback visible after prior assistant output", () => {
    const activeSession = makeSession("session-a", {
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
          text: "First answer",
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      showWaitingIndicator: true,
      waitingIndicatorKind: "send",
      waitingIndicatorPrompt: null,
    });

    expect(screen.getByText("Live turn")).toBeInTheDocument();
    expect(screen.getByText("Waiting for the next chunk of output...")).toBeInTheDocument();
  });

  it("keeps send waiting feedback visible during an active turn after file output", () => {
    const activeSession = makeSession("session-a", {
      status: "active",
      messages: [
        {
          id: "message-user",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First prompt",
        },
        {
          id: "message-files",
          type: "fileChanges",
          timestamp: "10:01",
          author: "assistant",
          title: "Agent changed 1 file",
          files: [{ path: "ui/src/app-session-actions.ts", kind: "modified" }],
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      showWaitingIndicator: true,
      waitingIndicatorKind: "send",
      waitingIndicatorPrompt: null,
    });

    expect(screen.getByText("Live turn")).toBeInTheDocument();
    expect(screen.getByText("Waiting for the next chunk of output...")).toBeInTheDocument();
  });

  it("suppresses a stale active live-turn tail after turn-finalizing file output", () => {
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
        {
          id: "message-files",
          type: "fileChanges",
          timestamp: "10:01",
          author: "assistant",
          title: "Agent changed 2 files",
          files: [
            { path: "ui/src/SessionPaneView.tsx", kind: "modified" },
            { path: "ui/src/panels/AgentSessionPanel.tsx", kind: "modified" },
          ],
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      showWaitingIndicator: true,
      waitingIndicatorPrompt: null,
    });

    expect(screen.queryByText("Live turn")).not.toBeInTheDocument();
  });

  it("renders conversation marker chips and navigates between markers", () => {
    const scrollIntoView = vi.fn();
    const originalScrollIntoView = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = scrollIntoView;
    const messages = makeTextMessages(3);
    const activeSession = makeSession("session-1", {
      messages,
      markers: [
        makeConversationMarker({
          id: "marker-2",
          messageId: "message-3",
          name: "Later issue",
          kind: "bug",
          color: "#ef4444",
          messageIndexHint: 2,
        }),
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-1",
          name: "Accepted direction",
          kind: "decision",
          color: DEFAULT_CONVERSATION_MARKER_COLOR,
          messageIndexHint: 0,
        }),
      ],
    });

    try {
      renderSessionPanelWithDefaults({ activeSession });

      const markerNavigator = screen.getByRole("navigation", {
        name: "Conversation markers",
      });
      expect(markerNavigator).toBeInTheDocument();
      const markerNavigatorQueries = within(markerNavigator);
      expect(screen.getByText("Markers")).toBeInTheDocument();
      expect(screen.getByText("2")).toBeInTheDocument();
      expect(
        markerNavigatorQueries.getByRole("button", {
          name: "Jump to Decision marker Accepted direction",
        }),
      ).toBeInTheDocument();
      expect(
        markerNavigatorQueries.getByRole("button", {
          name: "Jump to Bug marker Later issue",
        }),
      ).toBeInTheDocument();

      fireEvent.click(screen.getByRole("button", { name: "Next marker" }));

      expect(
        markerNavigatorQueries.getByRole("button", {
          name: "Jump to Decision marker Accepted direction",
        }),
      ).toHaveClass("is-active");
      expect(
        screen
          .getByText("message-1")
          .closest(".conversation-message-marker-shell"),
      ).toHaveClass("is-active-marker");
      expect(
        screen
          .getByText("message-1")
          .closest(".conversation-message-marker-shell"),
      ).toHaveStyle({
        "--conversation-active-marker-color":
          normalizeConversationMarkerColor(DEFAULT_CONVERSATION_MARKER_COLOR),
      });
      expect(
        screen
          .getByText("message-3")
          .closest(".conversation-message-marker-shell"),
      ).not.toHaveClass("is-active-marker");
    } finally {
      if (originalScrollIntoView) {
        Element.prototype.scrollIntoView = originalScrollIntoView;
      } else {
        delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
      }
    }
  });

  it("jumps to the cached marker slot when scroll-root lookup cannot see the panel", () => {
    let scrolledNode: Element | null = null;
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(function scrollIntoView(this: HTMLElement) {
        scrolledNode = this;
      });
    const detachedScrollRoot = document.createElement("section");
    const onConversationSearchItemMount = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(1),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-1",
          name: "Cached target",
        }),
      ],
    });

    try {
      renderSessionPanelWithDefaults({
        activeSession,
        scrollContainerRef: { current: detachedScrollRoot },
        onConversationSearchItemMount,
      });
      expect(onConversationSearchItemMount).toHaveBeenCalledWith(
        "message:message-1",
        expect.any(HTMLElement),
      );

      fireEvent.click(
        within(
          screen.getByRole("navigation", { name: "Conversation markers" }),
        ).getByRole("button", {
          name: "Jump to Decision marker Cached target",
        }),
      );

      const node = scrolledNode as unknown;
      expect(node).toBeInstanceOf(HTMLElement);
      if (!(node instanceof HTMLElement)) {
        throw new Error("Expected marker jump to scroll a mounted HTMLElement");
      }
      expect(node.getAttribute("data-session-search-item-key")).toBe(
        "message:message-1",
      );
    } finally {
      scrollIntoViewSpy.mockRestore();
    }
  });

  it("uses mounted marker slots for short non-virtualized conversations", () => {
    const scrollIntoViewCalls: Array<{
      itemKey: string | null;
      options: ScrollIntoViewOptions | boolean | undefined;
    }> = [];
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(function scrollIntoView(
        this: HTMLElement,
        options?: ScrollIntoViewOptions | boolean,
      ) {
        scrollIntoViewCalls.push({
          itemKey: this.getAttribute("data-session-search-item-key"),
          options,
        });
      });
    const messages = makeTextMessages(5);
    const activeSession = makeSession("session-1", {
      messages,
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-5",
          name: "Short transcript target",
          messageIndexHint: 4,
        }),
      ],
    });

    try {
      const { container } = renderSessionPanelWithDefaults({ activeSession });

      expect(container.querySelector(".virtualized-message-list")).toBeNull();
      fireEvent.click(
        screen.getByRole("button", {
          name: "Jump to Decision marker Short transcript target",
        }),
      );

      expect(scrollIntoViewCalls).toEqual([
        {
          itemKey: "message:message-5",
          options: { block: "center", behavior: "smooth" },
        },
      ]);
    } finally {
      scrollIntoViewSpy.mockRestore();
    }
  });

  it("jumps to a virtualized marker target in one click without redundant correction", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollIntoViewTargets: Array<string | null> = [];
    const scrollNode = document.createElement("section");
    let scrollTop = 80_000;
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(function scrollIntoView(this: HTMLElement) {
        scrollIntoViewTargets.push(
          this.getAttribute("data-session-search-item-key"),
        );
      });

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      value: 720,
    });
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      value: 900,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 90_000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });
    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(96),
      markers: [
        makeConversationMarker({
          id: "marker-top",
          messageId: "message-1",
          name: "Top checkpoint",
          kind: "checkpoint",
          messageIndexHint: 0,
        }),
      ],
    });

    try {
      renderSessionPanelWithDefaults({
        activeSession,
        scrollContainerRef: { current: scrollNode },
      });

      fireEvent.click(
        screen.getByRole("button", {
          name: "Jump to Checkpoint marker Top checkpoint",
        }),
      );

      await waitFor(() => {
        expect(scrollIntoViewTargets).toContain("message:message-1");
      });
      expect(
        scrollIntoViewTargets.filter((target) => target === "message:message-1"),
      ).toHaveLength(1);
      await act(async () => {
        await new Promise<void>((resolve) => {
          window.requestAnimationFrame(() => {
            window.requestAnimationFrame(() => resolve());
          });
        });
      });
      expect(
        scrollIntoViewTargets.filter((target) => target === "message:message-1"),
      ).toHaveLength(1);
      expect(screen.getByText("message-1")).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollIntoViewSpy.mockRestore();
    }
  });

  it("keeps marker jumps working after switching sessions with the same message ids", () => {
    let scrolledText = "";
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(function scrollIntoView(this: HTMLElement) {
        scrolledText = this.textContent ?? "";
      });
    const detachedScrollRoot = document.createElement("section");
    const firstSession = makeSession("session-a", {
      messages: [
        {
          id: "message-1",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Session A message",
        },
      ],
      markers: [
        makeConversationMarker({
          id: "marker-a",
          messageId: "message-1",
          name: "Session A marker",
          sessionId: "session-a",
        }),
      ],
    });
    const secondSession = makeSession("session-b", {
      messages: [
        {
          id: "message-1",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Session B message",
        },
      ],
      markers: [
        makeConversationMarker({
          id: "marker-b",
          messageId: "message-1",
          name: "Session B marker",
          sessionId: "session-b",
        }),
      ],
    });

    try {
      const rendered = renderSessionPanelWithDefaults({
        activeSession: firstSession,
        scrollContainerRef: { current: detachedScrollRoot },
        renderMessageCard: (message) => (
          <article className="message-card">
            {message.type === "text" ? message.text : message.id}
          </article>
        ),
      });
      act(() => {
        syncComposerSessionsStore({
          sessions: [secondSession],
          draftsBySessionId: {},
          draftAttachmentsBySessionId: {},
        });
      });
      rendered.rerender(
        <AgentSessionPanel
          paneId="pane-1"
          viewMode="session"
          activeSessionId="session-b"
          isLoading={false}
          isUpdating={false}
          showWaitingIndicator={false}
          waitingIndicatorPrompt={null}
          commandMessages={[]}
          diffMessages={[]}
          scrollContainerRef={{ current: detachedScrollRoot }}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
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
        />,
      );

      fireEvent.click(
        within(
          screen.getByRole("navigation", { name: "Conversation markers" }),
        ).getByRole("button", {
          name: "Jump to Decision marker Session B marker",
        }),
      );

      expect(scrolledText).toContain("Session B message");
    } finally {
      act(() => {
        resetSessionStoreForTesting();
      });
      scrollIntoViewSpy.mockRestore();
    }
  });

  it("scopes marker fallback lookup to the active panel scroll root", () => {
    let scrolledText = "";
    const scrollIntoViewSpy = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(function scrollIntoView(this: HTMLElement) {
        scrolledText = this.textContent ?? "";
      });
    const leftRoot = document.createElement("section");
    const rightRoot = document.createElement("section");
    leftRoot.innerHTML =
      '<article data-session-search-item-key="message:shared">left pane</article>';
    rightRoot.innerHTML =
      '<article data-session-search-item-key="message:shared">right pane</article>';
    document.body.append(leftRoot, rightRoot);
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(1),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "shared",
          name: "Fallback target",
        }),
      ],
    });

    try {
      renderSessionPanelWithDefaults({
        activeSession,
        scrollContainerRef: { current: rightRoot },
      });

      fireEvent.click(
        screen.getByRole("button", {
          name: "Jump to Decision marker Fallback target",
        }),
      );

      expect(scrolledText).toBe("right pane");
    } finally {
      scrollIntoViewSpy.mockRestore();
      leftRoot.remove();
      rightRoot.remove();
    }
  });

  function renderAssistantMarkerMenuHarness() {
    const onCreateConversationMarker = vi.fn();
    const onDeleteConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-2",
          name: "Review point",
        }),
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      onDeleteConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    const assistantLabel = () => screen.getByText("Agent message-2");
    const assistantTrigger = () =>
      assistantLabel().closest(
        "[data-conversation-marker-menu-trigger='true']",
      ) as HTMLElement;
    const openAssistantMenu = (init?: MouseEventInit) => {
      fireEvent.contextMenu(assistantLabel(), init);
      return screen.getByRole("menu", {
        name: "Conversation marker actions",
      });
    };

    return {
      assistantLabel,
      assistantTrigger,
      onCreateConversationMarker,
      onDeleteConversationMarker,
      openAssistantMenu,
    };
  }

  it("opens marker actions only from the assistant response context trigger", async () => {
    const { openAssistantMenu } = renderAssistantMarkerMenuHarness();

    fireEvent.contextMenu(screen.getByText("You message-1"));
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();

    fireEvent.contextMenu(screen.getByText("message-2 body"));
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();

    const addMenu = openAssistantMenu({
      clientX: 123,
      clientY: 234,
    });
    expect(addMenu).toHaveStyle({ left: "123px", top: "234px" });
    const addMenuItem = within(addMenu).getByRole("menuitem", {
      name: "Add checkpoint marker",
    });
    await waitFor(() => {
      expect(addMenuItem).toHaveFocus();
    });
  });

  it("navigates marker actions with ArrowDown and restores trigger focus on Escape", async () => {
    const { assistantTrigger, openAssistantMenu } =
      renderAssistantMarkerMenuHarness();

    const addMenu = openAssistantMenu();
    const addMenuItem = within(addMenu).getByRole("menuitem", {
      name: "Add checkpoint marker",
    });
    await waitFor(() => {
      expect(addMenuItem).toHaveFocus();
    });
    fireEvent.keyDown(addMenuItem, { key: "ArrowDown" });
    expect(
      within(addMenu).getByRole("menuitem", { name: "Remove Review point" }),
    ).toHaveFocus();
    fireEvent.keyDown(
      within(addMenu).getByRole("menuitem", { name: "Remove Review point" }),
      { key: "Escape" },
    );
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();
    await waitFor(() => {
      expect(assistantTrigger()).toHaveFocus();
    });
  });

  it("clamps marker action menu coordinates from measured dimensions", async () => {
    const { openAssistantMenu } = renderAssistantMarkerMenuHarness();

    const originalInnerWidth = window.innerWidth;
    const originalInnerHeight = window.innerHeight;
    const originalGetBoundingClientRect =
      HTMLElement.prototype.getBoundingClientRect;
    const rectSpy = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function getBoundingClientRect(this: HTMLElement) {
        if (this.classList.contains("conversation-marker-context-menu")) {
          return {
            x: 0,
            y: 0,
            width: 180,
            height: 120,
            top: 0,
            right: 180,
            bottom: 120,
            left: 0,
            toJSON: () => ({}),
          } as DOMRect;
        }
        return originalGetBoundingClientRect.call(this);
      });
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 320,
    });
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      value: 260,
    });
    try {
      openAssistantMenu({
        clientX: 500,
        clientY: 500,
      });
      const clampedMenu = screen.getByRole("menu", {
        name: "Conversation marker actions",
      });
      await waitFor(() => {
        expect(clampedMenu).toHaveStyle({ left: "132px", top: "132px" });
      });
      fireEvent.keyDown(clampedMenu, { key: "Escape" });
      await waitFor(() => {
        expect(
          screen.queryByRole("menu", { name: "Conversation marker actions" }),
        ).not.toBeInTheDocument();
      });
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        value: originalInnerWidth,
      });
      Object.defineProperty(window, "innerHeight", {
        configurable: true,
        value: originalInnerHeight,
      });
    }
  });

  it("creates message markers from the assistant marker action menu", () => {
    const { onCreateConversationMarker, openAssistantMenu } =
      renderAssistantMarkerMenuHarness();

    const reopenedAddMenu = openAssistantMenu();
    fireEvent.click(
      within(reopenedAddMenu).getByRole("menuitem", {
        name: "Add checkpoint marker",
      }),
    );
    const markerLabelInput = screen.getByLabelText("Marker label");
    expect(markerLabelInput).toHaveValue("Checkpoint");
    fireEvent.change(markerLabelInput, {
      target: { value: "Review later" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(onCreateConversationMarker).toHaveBeenCalledWith(
      "session-1",
      "message-2",
      { name: "Review later" },
    );
  });

  it("removes existing message markers from the assistant marker action menu", () => {
    const { onDeleteConversationMarker, openAssistantMenu } =
      renderAssistantMarkerMenuHarness();

    const removeMenu = openAssistantMenu();
    fireEvent.click(
      within(removeMenu).getByRole("menuitem", { name: "Remove Review point" }),
    );

    expect(onDeleteConversationMarker).toHaveBeenCalledWith(
      "session-1",
      "marker-1",
    );
  });

  it("highlights the source message immediately after creating a marker", () => {
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    const messageShell = screen
      .getByText("message-2 body")
      .closest(".conversation-message-marker-shell");
    expect(messageShell).not.toHaveClass("is-active-marker");

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(onCreateConversationMarker).toHaveBeenCalledWith(
      "session-1",
      "message-2",
      { name: "Checkpoint" },
    );
    expect(messageShell).toHaveClass("is-active-marker");
  });

  it("uses the newly created marker color when the message already has markers", async () => {
    const onCreateConversationMarker = vi.fn();
    const existingMarker = makeConversationMarker({
      id: "marker-existing",
      messageId: "message-2",
      name: "Review point",
      color: "#ef4444",
    });
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [existingMarker],
    });
    const renderPanel = createAgentSessionPanelHarness({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });
    const { rerender } = render(renderPanel());
    const messageShell = () =>
      screen
        .getByText("message-2 body")
        .closest(".conversation-message-marker-shell");

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(messageShell()).toHaveClass("is-active-marker");
    expect(messageShell()).not.toHaveStyle({
      "--conversation-active-marker-color":
        normalizeConversationMarkerColor(existingMarker.color),
    });

    const createdMarker = makeConversationMarker({
      id: "marker-created",
      messageId: "message-2",
      name: "Checkpoint",
      color: "#2563eb",
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-1", {
            ...activeSession,
            markers: [existingMarker, createdMarker],
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });
    rerender(renderPanel());

    await waitFor(() => {
      expect(messageShell()).toHaveStyle({
        "--conversation-active-marker-color": normalizeConversationMarkerColor(
          createdMarker.color,
        ),
      });
    });
  });

  it("keeps the latest overlapping marker create active when labels match", async () => {
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });
    const renderPanel = createAgentSessionPanelHarness({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });
    const { rerender } = render(renderPanel());
    const messageShell = () =>
      screen
        .getByText("message-2 body")
        .closest(".conversation-message-marker-shell");

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));
    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    const firstCreatedMarker = makeConversationMarker({
      id: "marker-created-first",
      messageId: "message-2",
      name: "Checkpoint",
      color: "#ef4444",
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-1", {
            ...activeSession,
            markers: [firstCreatedMarker],
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });
    rerender(renderPanel());

    await waitFor(() => {
      expect(
        screen.getByRole("button", {
          name: "Jump to Decision marker Checkpoint",
        }),
      ).not.toHaveClass("is-active");
    });
    expect(messageShell()).not.toHaveStyle({
      "--conversation-active-marker-color": normalizeConversationMarkerColor(
        firstCreatedMarker.color,
      ),
    });

    const secondCreatedMarker = makeConversationMarker({
      id: "marker-created-second",
      messageId: "message-2",
      name: "Checkpoint",
      color: "#2563eb",
      createdAt: "2026-05-01 10:00:01",
      updatedAt: "2026-05-01 10:00:01",
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-1", {
            ...activeSession,
            markers: [firstCreatedMarker, secondCreatedMarker],
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });
    rerender(renderPanel());

    await waitFor(() => {
      expect(messageShell()).toHaveStyle({
        "--conversation-active-marker-color": normalizeConversationMarkerColor(
          secondCreatedMarker.color,
        ),
      });
    });
  });

  it("clears a create-driven marker highlight when the marker is deleted", async () => {
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });
    const renderPanel = createAgentSessionPanelHarness({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });
    const { rerender } = render(renderPanel());
    const messageShell = () =>
      screen
        .getByText("message-2 body")
        .closest(".conversation-message-marker-shell");

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(messageShell()).toHaveClass("is-active-marker");

    const createdMarker = makeConversationMarker({
      id: "marker-created",
      messageId: "message-2",
      name: "Checkpoint",
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-1", {
            ...activeSession,
            markers: [createdMarker],
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });
    rerender(renderPanel());

    await waitFor(() => {
      expect(
        screen.getByRole("button", {
          name: "Jump to Decision marker Checkpoint",
        }),
      ).toHaveClass("is-active");
    });

    act(() => {
      syncComposerSessionsStore({
        sessions: [makeSession("session-1", activeSession)],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });
    rerender(renderPanel());

    await waitFor(() => {
      expect(messageShell()).not.toHaveClass("is-active-marker");
    });
  });

  it("clears a create-driven marker highlight when marker creation fails", async () => {
    const onCreateConversationMarker = vi.fn(async () => false);
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    const messageShell = screen
      .getByText("message-2 body")
      .closest(".conversation-message-marker-shell");

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(messageShell).toHaveClass("is-active-marker");
    await waitFor(() => {
      expect(messageShell).not.toHaveClass("is-active-marker");
    });
  });

  function renderMarkerCreationHarness() {
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    const trigger = screen
      .getByText("Agent message-2")
      .closest("[data-conversation-marker-menu-trigger='true']") as HTMLElement;

    return { onCreateConversationMarker, trigger };
  }

  function openMarkerCreateDialog(trigger: HTMLElement) {
    fireEvent.contextMenu(trigger);
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );

    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();
    const dialog = screen.getByRole("dialog", {
      name: "Create conversation marker",
    });
    const markerLabelInput = screen.getByLabelText(
      "Marker label",
    ) as HTMLInputElement;
    const submitButton = screen.getByRole("button", { name: "Create marker" });
    const cancelButton = screen.getByRole("button", { name: "Cancel" });

    return { cancelButton, dialog, markerLabelInput, submitButton };
  }

  it("opens marker label creation as a focused dialog with selected default text", async () => {
    const { trigger } = renderMarkerCreationHarness();
    const { dialog, markerLabelInput } = openMarkerCreateDialog(trigger);

    expect(dialog).toBeInTheDocument();
    await waitFor(() => {
      expect(markerLabelInput).toHaveFocus();
    });
    expect(markerLabelInput.selectionStart).toBe(0);
    expect(markerLabelInput.selectionEnd).toBe("Checkpoint".length);
  });

  it("validates and submits trimmed marker labels without closing on resize", () => {
    const { onCreateConversationMarker, trigger } = renderMarkerCreationHarness();
    const { markerLabelInput, submitButton } = openMarkerCreateDialog(trigger);

    fireEvent.change(markerLabelInput, { target: { value: "🙂".repeat(121) } });
    expect(Array.from(markerLabelInput.value)).toHaveLength(120);
    fireEvent.change(markerLabelInput, { target: { value: "   " } });
    expect(submitButton).toBeDisabled();

    fireEvent.change(markerLabelInput, {
      target: { value: "  Review later  " },
    });
    fireEvent.resize(window);
    expect(
      screen.getByRole("dialog", { name: "Create conversation marker" }),
    ).toBeInTheDocument();
    fireEvent.click(submitButton);

    expect(onCreateConversationMarker).toHaveBeenCalledWith(
      "session-1",
      "message-2",
      { name: "Review later" },
    );
  });

  it("keeps marker create dialog keyboard handling local to dialog controls", () => {
    const { trigger } = renderMarkerCreationHarness();
    const { cancelButton } = openMarkerCreateDialog(trigger);

    cancelButton.focus();
    fireEvent.keyDown(cancelButton, { key: "ArrowDown" });
    expect(cancelButton).toHaveFocus();
  });

  it("restores marker trigger focus after canceling marker label creation", async () => {
    const { onCreateConversationMarker, trigger } = renderMarkerCreationHarness();
    openMarkerCreateDialog(trigger);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onCreateConversationMarker).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(trigger).toHaveFocus();
    });
  });

  it("restores marker trigger focus after escaping marker label creation", async () => {
    const { trigger } = renderMarkerCreationHarness();
    openMarkerCreateDialog(trigger);

    fireEvent.keyDown(screen.getByLabelText("Marker label"), { key: "Escape" });

    expect(
      screen.queryByRole("dialog", { name: "Create conversation marker" }),
    ).not.toBeInTheDocument();
    await waitFor(() => {
      expect(trigger).toHaveFocus();
    });
  });

  it("shows marker label length feedback while creating a marker", async () => {
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    renderSessionPanelWithDefaults({
      activeSession,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    const trigger = screen
      .getByText("Agent message-2")
      .closest("[data-conversation-marker-menu-trigger='true']") as HTMLElement;
    fireEvent.contextMenu(trigger);
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );

    const dialog = screen.getByRole("dialog", {
      name: "Create conversation marker",
    });
    const markerLabelInput = within(dialog).getByRole("textbox", {
      name: "Marker label",
    }) as HTMLInputElement;

    const limitHint = within(dialog).getByText("10/120 characters");
    expect(limitHint).toBeInTheDocument();
    expect(limitHint).toHaveAttribute("aria-live", "polite");
    expect(markerLabelInput).toHaveAttribute("aria-describedby", limitHint.id);
    fireEvent.change(markerLabelInput, { target: { value: "x".repeat(121) } });

    expect(markerLabelInput.value).toHaveLength(120);
    expect(
      within(dialog).getByText("120/120 characters maximum"),
    ).toBeInTheDocument();
  });

  it("toggles the floating marker window from the message context menu", () => {
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(4),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-2",
          name: "Review point",
        }),
        makeConversationMarker({
          id: "marker-2",
          messageId: "message-4",
          name: "Follow-up point",
        }),
      ],
    });
    const { container } = renderSessionPanelWithDefaults({
      activeSession,
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    expect(container.querySelector(".conversation-message-markers")).toBeNull();
    expect(
      screen.getByRole("navigation", { name: "Conversation markers" }),
    ).toBeInTheDocument();

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));
    const assistantShell = assistantMeta?.closest(
      ".conversation-message-marker-shell",
    );

    expect(assistantMeta).toBeTruthy();
    expect(assistantShell).toBeTruthy();
    fireEvent.contextMenu(assistantMeta!);
    expect(
      screen.getByRole("menuitem", { name: "Hide markers window" }),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Hide markers window" }),
    );

    expect(
      screen.queryByRole("navigation", { name: "Conversation markers" }),
    ).not.toBeInTheDocument();

    fireEvent.contextMenu(assistantMeta!);
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Show markers window" }),
    );

    const markerWindow = screen.getByRole("navigation", {
      name: "Conversation markers",
    });
    expect(
      within(markerWindow).getByRole("button", {
        name: "Jump to Decision marker Review point",
      }),
    ).toBeInTheDocument();
    expect(
      within(markerWindow).getByRole("button", {
        name: "Jump to Decision marker Follow-up point",
      }),
    ).toBeInTheDocument();

    fireEvent.click(
      within(markerWindow).getByRole("button", {
        name: "Hide markers window",
      }),
    );

    expect(
      screen.queryByRole("navigation", { name: "Conversation markers" }),
    ).not.toBeInTheDocument();
  });

  it("restores marker action menu focus to the trigger and reopens from keyboard", async () => {
    const user = userEvent.setup();
    const { container } = renderSessionPanelWithDefaults({
      activeSession: makeSession("session-1", {
        messages: makeTextMessages(2),
      }),
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));
    expect(assistantMeta).toBeTruthy();

    fireEvent.contextMenu(assistantMeta!);
    fireEvent.keyDown(
      screen.getByRole("menu", { name: "Conversation marker actions" }),
      { key: "Escape" },
    );
    await waitFor(() => {
      expect(assistantMeta).toHaveFocus();
    });

    await user.keyboard("{Enter}");
    expect(
      screen.getByRole("menu", { name: "Conversation marker actions" }),
    ).toBeInTheDocument();
    fireEvent.keyDown(
      screen.getByRole("menu", { name: "Conversation marker actions" }),
      { key: "Escape" },
    );
    await waitFor(() => {
      expect(assistantMeta).toHaveFocus();
    });

    fireEvent.keyDown(assistantMeta!, { key: " " });
    expect(
      screen.getByRole("menu", { name: "Conversation marker actions" }),
    ).toBeInTheDocument();
  });

  it("restores focus to the conversation page when closing the marker window", async () => {
    const { container } = renderSessionPanelWithDefaults({
      activeSession: makeSession("session-1", {
        messages: makeTextMessages(2),
        markers: [
          makeConversationMarker({
            id: "marker-1",
            messageId: "message-2",
            name: "Review point",
          }),
        ],
      }),
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const markerWindow = screen.getByRole("navigation", {
      name: "Conversation markers",
    });

    fireEvent.click(
      within(markerWindow).getByRole("button", {
        name: "Hide markers window",
      }),
    );

    expect(
      screen.queryByRole("navigation", { name: "Conversation markers" }),
    ).not.toBeInTheDocument();
    await waitFor(() => {
      expect(container.querySelector(".session-conversation-page")).toHaveFocus();
    });
  });

  it("uses dialog controls instead of menuitem children while creating a marker", async () => {
    const { container } = renderSessionPanelWithDefaults({
      activeSession: makeSession("session-1", {
        messages: makeTextMessages(2),
      }),
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));
    expect(assistantMeta).toBeTruthy();

    fireEvent.contextMenu(assistantMeta!);
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );

    const dialog = screen.getByRole("dialog", {
      name: "Create conversation marker",
    });
    expect(within(dialog).queryAllByRole("menuitem")).toHaveLength(0);
    await waitFor(() => {
      expect(
        within(dialog).getByRole("textbox", { name: "Marker label" }),
      ).toHaveFocus();
    });
    expect(
      within(dialog).getByRole("button", { name: "Create marker" }),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: "Cancel" }),
    ).toBeInTheDocument();
  });

  it("starts with the marker window hidden for empty marker sets and can show it from the context menu", () => {
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [],
    });
    const { container } = renderSessionPanelWithDefaults({
      activeSession,
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    expect(
      screen.queryByRole("navigation", { name: "Conversation markers" }),
    ).not.toBeInTheDocument();

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));

    expect(assistantMeta).toBeTruthy();
    fireEvent.contextMenu(assistantMeta!);
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Show markers window" }),
    );

    const markerWindow = screen.getByRole("navigation", {
      name: "Conversation markers",
    });
    expect(within(markerWindow).getByText("0")).toBeInTheDocument();
    expect(within(markerWindow).getByText("No markers yet.")).toBeInTheDocument();
    expect(
      within(markerWindow).getByRole("button", { name: "Previous marker" }),
    ).toBeDisabled();
    expect(
      within(markerWindow).getByRole("button", { name: "Next marker" }),
    ).toBeDisabled();
  });

  it("resets explicit marker-window visibility when switching sessions", () => {
    const renderMessageCard = (message: Message) => (
      <MessageCard
        message={message}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
      />
    );
    const firstSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-2",
          name: "First marker",
        }),
      ],
    });
    const secondSession = makeSession("session-2", {
      messages: makeTextMessages(2),
      markers: [
        makeConversationMarker({
          id: "marker-2",
          messageId: "message-2",
          name: "Second marker",
          sessionId: "session-2",
        }),
      ],
    });

    const { container, rerender } = render(
      createAgentSessionPanelHarness({
        activeSession: firstSession,
        renderMessageCard,
      })(),
    );

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));
    expect(assistantMeta).toBeTruthy();
    act(() => {
      fireEvent.contextMenu(assistantMeta!);
    });
    act(() => {
      fireEvent.click(
        screen.getByRole("menuitem", { name: "Hide markers window" }),
      );
    });
    expect(
      screen.queryByRole("navigation", { name: "Conversation markers" }),
    ).not.toBeInTheDocument();

    act(() => {
      rerender(
        createAgentSessionPanelHarness({
          activeSession: secondSession,
          renderMessageCard,
        })(),
      );
    });

    const markerWindow = screen.getByRole("navigation", {
      name: "Conversation markers",
    });
    expect(
      within(markerWindow).getByRole("button", {
        name: "Jump to Decision marker Second marker",
      }),
    ).toBeInTheDocument();
  });

  it("resets explicit marker-window show override when switching sessions", () => {
    const renderMessageCard = (message: Message) => (
      <MessageCard
        message={message}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
      />
    );
    const firstSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [],
    });
    const secondSession = makeSession("session-2", {
      messages: makeTextMessages(2),
      markers: [],
    });

    const { container, rerender } = render(
      createAgentSessionPanelHarness({
        activeSession: firstSession,
        renderMessageCard,
      })(),
    );

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));
    expect(assistantMeta).toBeTruthy();
    act(() => {
      fireEvent.contextMenu(assistantMeta!);
    });
    act(() => {
      fireEvent.click(
        screen.getByRole("menuitem", { name: "Show markers window" }),
      );
    });
    expect(
      screen.getByRole("navigation", { name: "Conversation markers" }),
    ).toBeInTheDocument();

    act(() => {
      rerender(
        createAgentSessionPanelHarness({
          activeSession: secondSession,
          renderMessageCard,
        })(),
      );
    });

    expect(
      screen.queryByRole("navigation", { name: "Conversation markers" }),
    ).not.toBeInTheDocument();
  });

  it("cancels the floating marker window focus restore when switching sessions", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    let nextFrameId = 0;
    const frameCallbacks = new Map<number, FrameRequestCallback>();
    const requestAnimationFrameMock = vi.fn((callback: FrameRequestCallback) => {
      const frameId = ++nextFrameId;
      frameCallbacks.set(frameId, callback);
      return frameId;
    });
    const cancelAnimationFrameMock = vi.fn((frameId: number) => {
      frameCallbacks.delete(frameId);
    });
    const renderMessageCard = (message: Message) => (
      <MessageCard
        message={message}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
      />
    );
    const firstSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-2",
          name: "First marker",
        }),
      ],
    });
    const secondSession = makeSession("session-2", {
      messages: makeTextMessages(2),
      markers: [],
    });

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;

    try {
      const { rerender } = render(
        createAgentSessionPanelHarness({
          activeSession: firstSession,
          renderMessageCard,
        })(),
      );

      const markerWindow = screen.getByRole("navigation", {
        name: "Conversation markers",
      });
      requestAnimationFrameMock.mockClear();
      cancelAnimationFrameMock.mockClear();

      act(() => {
        fireEvent.click(
          within(markerWindow).getByRole("button", {
            name: "Hide markers window",
          }),
        );
      });
      expect(requestAnimationFrameMock).toHaveBeenCalledTimes(1);
      const latestFrameResult =
        requestAnimationFrameMock.mock.results[0];
      const focusRestoreFrameId = latestFrameResult?.value;

      expect(typeof focusRestoreFrameId).toBe("number");
      expect(frameCallbacks.has(focusRestoreFrameId as number)).toBe(true);

      act(() => {
        rerender(
          createAgentSessionPanelHarness({
            activeSession: secondSession,
            renderMessageCard,
          })(),
        );
      });

      expect(cancelAnimationFrameMock).toHaveBeenCalledWith(
        focusRestoreFrameId,
      );
      expect(frameCallbacks.has(focusRestoreFrameId as number)).toBe(false);
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("preserves native context menu behavior for selected text, links, and code", () => {
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: [
        {
          author: "assistant",
          id: "message-1",
          text: "Assistant output",
          timestamp: "10:00",
          type: "text",
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: () => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger
          >
            <span>Agent header</span>
            <span>10:00</span>
          </div>
          <a href="https://example.test">native link</a>
          <code>native code</code>
          <span>plain assistant text</span>
        </article>
      ),
    });

    fireEvent.contextMenu(screen.getByText("native link"));
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();

    fireEvent.contextMenu(screen.getByText("native code"));
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();

    const plainText = screen.getByText("plain assistant text");
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(plainText);
    selection?.removeAllRanges();
    selection?.addRange(range);

    fireEvent.contextMenu(plainText);
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();

    selection?.removeAllRanges();
    fireEvent.contextMenu(plainText);
    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();

    fireEvent.contextMenu(screen.getByText("Agent header"));
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(onCreateConversationMarker).toHaveBeenCalledWith(
      "session-1",
      "message-1",
      { name: "Checkpoint" },
    );
  });

  it("exposes marker context-menu triggers on real message headers", () => {
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });
    const { container } = renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const messageMetas = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author"),
    );
    const userMeta = messageMetas.find((meta) =>
      meta.textContent?.includes("You"),
    );
    const assistantMeta = messageMetas.find((meta) =>
      meta.textContent?.includes("Agent"),
    );

    expect(userMeta).toBeTruthy();
    expect(assistantMeta).toBeTruthy();
    expect(userMeta).toHaveAttribute(
      "data-conversation-marker-menu-trigger",
      "true",
    );
    expect(assistantMeta).toHaveAttribute(
      "data-conversation-marker-menu-trigger",
      "true",
    );
    expect(userMeta).toHaveAttribute(
      "aria-label",
      "You, open marker actions",
    );
    expect(assistantMeta).toHaveAttribute(
      "aria-label",
      "Agent, open marker actions",
    );

    fireEvent.contextMenu(userMeta!);
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    fireEvent.contextMenu(assistantMeta!);
    expect(
      screen.getByRole("menu", { name: "Conversation marker actions" }),
    ).toBeInTheDocument();

    expect(onCreateConversationMarker).toHaveBeenCalledWith(
      "session-1",
      "message-1",
      { name: "Checkpoint" },
    );
  });

  it("names marker trigger buttons by author and action", () => {
    const { container } = renderSessionPanelWithDefaults({
      activeSession: makeSession("session-1", {
        messages: makeTextMessages(2),
      }),
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));

    expect(assistantMeta).toBeTruthy();
    expect(assistantMeta).toHaveAccessibleName("Agent, open marker actions");
    expect(
      screen.queryByRole("button", { name: "Agent" }),
    ).not.toBeInTheDocument();
  });

  it("opens marker actions from the assistant header click and keyboard trigger", async () => {
    const user = userEvent.setup();
    const onCreateConversationMarker = vi.fn();
    const { container } = renderSessionPanelWithDefaults({
      activeSession: makeSession("session-1", {
        messages: makeTextMessages(2),
      }),
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const assistantMeta = Array.from(
      container.querySelectorAll<HTMLElement>(".message-meta-author-agent"),
    ).find((meta) => meta.textContent?.includes("Agent"));

    expect(assistantMeta).toBeTruthy();
    expect(assistantMeta).toHaveAttribute("role", "button");
    expect(assistantMeta).toHaveAttribute("tabindex", "0");
    expect(assistantMeta).toHaveAttribute("aria-haspopup", "menu");
    expect(assistantMeta).toHaveAttribute(
      "aria-label",
      "Agent, open marker actions",
    );

    fireEvent.click(assistantMeta!, { clientX: 140, clientY: 80 });
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    assistantMeta!.focus();
    await user.keyboard("{Enter}");
    fireEvent.click(
      screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create marker" }));

    expect(onCreateConversationMarker).toHaveBeenNthCalledWith(
      1,
      "session-1",
      "message-2",
      { name: "Checkpoint" },
    );
    expect(onCreateConversationMarker).toHaveBeenNthCalledWith(
      2,
      "session-1",
      "message-2",
      { name: "Checkpoint" },
    );
  });

  it("does not open marker actions from interactive controls inside assistant metadata", async () => {
    const user = userEvent.setup();
    const onCreateConversationMarker = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: [
        {
          id: "message-parallel-agents",
          type: "parallelAgents",
          author: "assistant",
          timestamp: "10:02",
          agents: [
            {
              id: "delegation-completed",
              source: "delegation",
              title: "Review frontend",
              status: "completed",
              detail: "No issues found",
            },
          ],
        },
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      onCreateConversationMarker,
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />
      ),
    });

    const showTasks = screen.getByRole("button", { name: "Show tasks" });
    fireEvent.click(showTasks);

    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();
    expect(onCreateConversationMarker).not.toHaveBeenCalled();

    const hideTasks = screen.getByRole("button", { name: "Hide tasks" });
    hideTasks.focus();
    await user.keyboard("{Enter}");

    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show tasks" })).toBeInTheDocument();

    const showTasksAgain = screen.getByRole("button", { name: "Show tasks" });
    showTasksAgain.focus();
    await user.keyboard(" ");

    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hide tasks" })).toBeInTheDocument();
  });

  it("does not render the removed right-side marker toolbar", () => {
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    renderSessionPanelWithDefaults({
      activeSession,
    });

    expect(screen.queryByRole("toolbar")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Add checkpoint marker" }),
    ).not.toBeInTheDocument();
  });

  it("stops Escape from leaking out of the marker action menu", () => {
    const documentKeydownSpy = vi.fn();
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    renderSessionPanelWithDefaults({
      activeSession,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    const menu = screen.getByRole("menu", {
      name: "Conversation marker actions",
    });

    document.addEventListener("keydown", documentKeydownSpy);
    try {
      fireEvent.keyDown(menu, { key: "Escape" });

      expect(documentKeydownSpy).not.toHaveBeenCalled();
      expect(
        screen.queryByRole("menu", { name: "Conversation marker actions" }),
      ).not.toBeInTheDocument();
    } finally {
      document.removeEventListener("keydown", documentKeydownSpy);
    }
  });

  it("closes the portaled marker menu only for transcript scrolls or viewport resizes", () => {
    const transcriptScrollRoot = document.createElement("section");
    const unrelatedScrollRoot = document.createElement("section");
    document.body.append(transcriptScrollRoot, unrelatedScrollRoot);
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    try {
      renderSessionPanelWithDefaults({
        activeSession,
        scrollContainerRef: { current: transcriptScrollRoot },
        renderMessageCard: (message) => (
          <article className="message-card">
            <div
              className="message-meta"
              data-conversation-marker-menu-trigger={
                message.author === "assistant" ? true : undefined
              }
            >
              <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
              <span>{message.timestamp}</span>
            </div>
            <p>{`${message.id} body`}</p>
          </article>
        ),
      });

      fireEvent.contextMenu(screen.getByText("Agent message-2"));
      expect(
        screen.getByRole("menu", { name: "Conversation marker actions" }),
      ).toBeInTheDocument();

      fireEvent.scroll(unrelatedScrollRoot);
      expect(
        screen.getByRole("menu", { name: "Conversation marker actions" }),
      ).toBeInTheDocument();

      fireEvent.scroll(document);
      expect(
        screen.getByRole("menu", { name: "Conversation marker actions" }),
      ).toBeInTheDocument();

      fireEvent.scroll(window);
      expect(
        screen.queryByRole("menu", { name: "Conversation marker actions" }),
      ).not.toBeInTheDocument();

      fireEvent.contextMenu(screen.getByText("Agent message-2"));
      expect(
        screen.getByRole("menu", { name: "Conversation marker actions" }),
      ).toBeInTheDocument();

      fireEvent.scroll(transcriptScrollRoot);
      expect(
        screen.queryByRole("menu", { name: "Conversation marker actions" }),
      ).not.toBeInTheDocument();

      fireEvent.contextMenu(screen.getByText("Agent message-2"));
      expect(
        screen.getByRole("menu", { name: "Conversation marker actions" }),
      ).toBeInTheDocument();

      fireEvent.resize(window);
      expect(
        screen.queryByRole("menu", { name: "Conversation marker actions" }),
      ).not.toBeInTheDocument();
    } finally {
      transcriptScrollRoot.remove();
      unrelatedScrollRoot.remove();
    }
  });

  it("closes the portaled marker menu when its session becomes inactive", async () => {
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });
    const harness = createAgentSessionPanelHarness({
      activeSession,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });
    const { rerender } = render(harness());

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    expect(
      screen.getByRole("menu", { name: "Conversation marker actions" }),
    ).toBeInTheDocument();

    rerender(harness({ activeSessionId: null }));

    await waitFor(() => {
      expect(
        screen.queryByRole("menu", { name: "Conversation marker actions" }),
      ).not.toBeInTheDocument();
    });
  });

  it("clamps the marker menu from offset dimensions when DOMRect has no size", async () => {
    const originalInnerWidth = window.innerWidth;
    const originalInnerHeight = window.innerHeight;
    const originalOffsetWidth = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetWidth",
    );
    const originalOffsetHeight = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetHeight",
    );
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });

    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 320,
    });
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      value: 260,
    });
    Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
      configurable: true,
      get: () => 180,
    });
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get: () => 120,
    });

    try {
      renderSessionPanelWithDefaults({
        activeSession,
        renderMessageCard: (message) => (
          <article className="message-card">
            <div
              className="message-meta"
              data-conversation-marker-menu-trigger={
                message.author === "assistant" ? true : undefined
              }
            >
              <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
              <span>{message.timestamp}</span>
            </div>
            <p>{`${message.id} body`}</p>
          </article>
        ),
      });

      fireEvent.contextMenu(screen.getByText("Agent message-2"), {
        clientX: 500,
        clientY: 500,
      });
      const clampedMenu = screen.getByRole("menu", {
        name: "Conversation marker actions",
      });
      await waitFor(() => {
        expect(clampedMenu).toHaveStyle({ left: "132px", top: "132px" });
      });
    } finally {
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        value: originalInnerWidth,
      });
      Object.defineProperty(window, "innerHeight", {
        configurable: true,
        value: originalInnerHeight,
      });
      if (originalOffsetWidth) {
        Object.defineProperty(
          HTMLElement.prototype,
          "offsetWidth",
          originalOffsetWidth,
        );
      }
      if (originalOffsetHeight) {
        Object.defineProperty(
          HTMLElement.prototype,
          "offsetHeight",
          originalOffsetHeight,
        );
      }
    }
  });

  it("reclamps the marker create dialog from rect dimensions on viewport resize", async () => {
    const originalInnerWidth = window.innerWidth;
    const originalInnerHeight = window.innerHeight;
    const originalGetBoundingClientRectDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "getBoundingClientRect",
    );
    const originalGetBoundingClientRect =
      HTMLElement.prototype.getBoundingClientRect;
    const originalOffsetWidth = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetWidth",
    );
    const originalOffsetHeight = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetHeight",
    );
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
    });
    let targetDialog: HTMLElement | null = null;

    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 800,
    });
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      value: 600,
    });
    Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
      configurable: true,
      get: () => 999,
    });
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get: () => 999,
    });
    Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", {
      configurable: true,
      value: function getBoundingClientRectMock(this: HTMLElement) {
        if (this === targetDialog) {
          return {
            bottom: 620,
            height: 120,
            left: 500,
            right: 680,
            top: 500,
            width: 180,
            x: 500,
            y: 500,
            toJSON: () => ({}),
          };
        }
        return originalGetBoundingClientRect.call(this);
      },
    });

    try {
      renderSessionPanelWithDefaults({
        activeSession,
        renderMessageCard: (message) => (
          <article className="message-card">
            <div
              className="message-meta"
              data-conversation-marker-menu-trigger={
                message.author === "assistant" ? true : undefined
              }
            >
              <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
              <span>{message.timestamp}</span>
            </div>
            <p>{`${message.id} body`}</p>
          </article>
        ),
      });

      fireEvent.contextMenu(screen.getByText("Agent message-2"), {
        clientX: 500,
        clientY: 500,
      });
      fireEvent.click(
        screen.getByRole("menuitem", { name: "Add checkpoint marker" }),
      );
      expect(
        screen.getByRole("dialog", { name: "Create conversation marker" }),
      ).toBeInTheDocument();
      const matchingDialogs = screen.getAllByRole("dialog", {
        name: "Create conversation marker",
      });
      expect(matchingDialogs).toHaveLength(1);
      targetDialog = matchingDialogs[0];

      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        value: 320,
      });
      Object.defineProperty(window, "innerHeight", {
        configurable: true,
        value: 260,
      });
      fireEvent.resize(window);

      const dialog = screen.getByRole("dialog", {
        name: "Create conversation marker",
      });
      await waitFor(() => {
        expect(dialog).toHaveStyle({ left: "132px", top: "132px" });
      });
    } finally {
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        value: originalInnerWidth,
      });
      Object.defineProperty(window, "innerHeight", {
        configurable: true,
        value: originalInnerHeight,
      });
      if (originalGetBoundingClientRectDescriptor) {
        Object.defineProperty(
          HTMLElement.prototype,
          "getBoundingClientRect",
          originalGetBoundingClientRectDescriptor,
        );
      } else {
        delete (
          HTMLElement.prototype as unknown as {
            getBoundingClientRect?: typeof HTMLElement.prototype.getBoundingClientRect;
          }
        ).getBoundingClientRect;
      }
      if (originalOffsetWidth) {
        Object.defineProperty(
          HTMLElement.prototype,
          "offsetWidth",
          originalOffsetWidth,
        );
      }
      if (originalOffsetHeight) {
        Object.defineProperty(
          HTMLElement.prototype,
          "offsetHeight",
          originalOffsetHeight,
        );
      }
    }
  });

  it.each([
    ["ArrowDown", "Add checkpoint marker"],
    ["ArrowUp", "Hide markers window"],
  ])(
    "starts marker-menu %s navigation from the nearest boundary when focus is outside menu items",
    (key, expectedItemName) => {
      const activeSession = makeSession("session-1", {
        messages: makeTextMessages(2),
        markers: [
          makeConversationMarker({
            id: "marker-1",
            messageId: "message-2",
            name: "Review point",
          }),
        ],
      });

      renderSessionPanelWithDefaults({
        activeSession,
        renderMessageCard: (message) => (
          <article className="message-card">
            <div
              className="message-meta"
              data-conversation-marker-menu-trigger={
                message.author === "assistant" ? true : undefined
              }
            >
              <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
              <span>{message.timestamp}</span>
            </div>
            <p>{`${message.id} body`}</p>
          </article>
        ),
      });

      fireEvent.contextMenu(screen.getByText("Agent message-2"));
      const menu = screen.getByRole("menu", {
        name: "Conversation marker actions",
      });
      const firstItem = within(menu).getByRole("menuitem", {
        name: "Add checkpoint marker",
      });
      firstItem.blur();

      fireEvent.keyDown(menu, { key });

      expect(
        within(menu).getByRole("menuitem", { name: expectedItemName }),
      ).toHaveFocus();
    },
  );

  it("keeps marker removal items between add and window visibility actions", () => {
    const activeSession = makeSession("session-1", {
      messages: makeTextMessages(2),
      markers: [
        makeConversationMarker({
          id: "marker-1",
          messageId: "message-2",
          name: "Review point",
        }),
      ],
    });

    renderSessionPanelWithDefaults({
      activeSession,
      renderMessageCard: (message) => (
        <article className="message-card">
          <div
            className="message-meta"
            data-conversation-marker-menu-trigger={
              message.author === "assistant" ? true : undefined
            }
          >
            <span>{`${message.author === "assistant" ? "Agent" : "You"} ${message.id}`}</span>
            <span>{message.timestamp}</span>
          </div>
          <p>{`${message.id} body`}</p>
        </article>
      ),
    });

    fireEvent.contextMenu(screen.getByText("Agent message-2"));
    const menu = screen.getByRole("menu", {
      name: "Conversation marker actions",
    });

    expect(
      within(menu)
        .getAllByRole("menuitem")
        .map((item) => item.textContent?.trim()),
    ).toEqual([
      "Add checkpoint marker",
      "Remove Review point",
      "Hide markers window",
    ]);
  });

  it("dispatches visible message actions through the latest parent callbacks", async () => {
    const initialApproval = vi.fn();
    const latestApproval = vi.fn();
    const activeSession = makeSession("session-a", {
      messages: [
        {
          author: "assistant",
          id: "approval-message",
          text: "Needs approval",
          timestamp: "10:00",
          type: "text",
        },
      ],
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const renderPanel = (
      onApprovalDecision: Parameters<typeof AgentSessionPanel>[0]["onApprovalDecision"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={() => {}}
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
        renderMessageCard={(message, _isLive, approve) => (
          <button
            type="button"
            onClick={() => approve(message.id, "accepted")}
          >
            Approve latest
          </button>
        )}
        renderPromptSettings={() => null}
      />
    );

    const { rerender } = render(renderPanel(initialApproval));
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      rerender(renderPanel(latestApproval));
      await Promise.resolve();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Approve latest" }));
      await Promise.resolve();
    });

    expect(initialApproval).not.toHaveBeenCalled();
    expect(latestApproval).toHaveBeenCalledWith(
      "session-a",
      "approval-message",
      "accepted",
    );
  });

  it("dispatches prompt settings through the latest parent callback after handler-only rerenders", async () => {
    const initialSessionSettingsChange = vi.fn();
    const latestSessionSettingsChange = vi.fn();
    const activeSession = makeSession("session-a", {
      agent: "Codex",
      model: "gpt-5.4",
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const renderPanel = (
      onSessionSettingsChange: Parameters<typeof AgentSessionPanel>[0]["onSessionSettingsChange"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="prompt"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onCancelQueuedPrompt={() => {}}
        onSessionSettingsChange={onSessionSettingsChange}
        conversationSearchQuery=""
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onConversationSearchItemMount={() => {}}
        renderCommandCard={() => null}
        renderDiffCard={() => null}
        renderMessageCard={(message) => (
          <article className="message-card">{message.id}</article>
        )}
        renderPromptSettings={(_paneId, session, _isUpdating, onChange) => (
          <button
            type="button"
            onClick={() => onChange(session.id, "model", "gpt-5.3-codex")}
          >
            Apply latest settings
          </button>
        )}
      />
    );

    const { rerender } = render(renderPanel(initialSessionSettingsChange));
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      rerender(renderPanel(latestSessionSettingsChange));
      await Promise.resolve();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Apply latest settings" }));
      await Promise.resolve();
    });

    expect(initialSessionSettingsChange).not.toHaveBeenCalled();
    expect(latestSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "model",
      "gpt-5.3-codex",
    );
  });

  it("refreshes prompt settings when only the prompt renderer changes", async () => {
    const activeSession = makeSession("session-a", {
      agent: "Codex",
      model: "gpt-5.4",
    });

    const renderPanelWithDefaults = createAgentSessionPanelHarness({
      activeSession,
      viewMode: "prompt",
    });
    const renderPanel = (promptLabel: string) =>
      renderPanelWithDefaults({
        renderPromptSettings: () => <p>{promptLabel}</p>,
      });

    const { rerender } = render(renderPanel("Initial prompt renderer"));
    expect(screen.getByText("Initial prompt renderer")).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel("Latest prompt renderer"));
      await Promise.resolve();
    });

    expect(screen.queryByText("Initial prompt renderer")).not.toBeInTheDocument();
    expect(screen.getByText("Latest prompt renderer")).toBeInTheDocument();
  });

  it("refreshes command cards when only the command renderer changes", async () => {
    const activeSession = makeSession("session-a");
    const commandMessages = makeCommandMessages(1);

    const renderPanelWithDefaults = createAgentSessionPanelHarness({
      activeSession,
      viewMode: "commands",
      commandMessages,
    });
    const renderPanel = (label: string) =>
      renderPanelWithDefaults({
        renderCommandCard: (message) => (
          <article>{`${label}: ${message.id}`}</article>
        ),
      });

    const { rerender } = render(renderPanel("Initial command renderer"));
    expect(screen.getByText("Initial command renderer: command-1")).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel("Latest command renderer"));
      await Promise.resolve();
    });

    expect(screen.queryByText("Initial command renderer: command-1")).not.toBeInTheDocument();
    expect(screen.getByText("Latest command renderer: command-1")).toBeInTheDocument();
  });

  it("refreshes diff cards when only the diff renderer changes", async () => {
    const activeSession = makeSession("session-a");
    const diffMessages = makeDiffMessages(1);

    const renderPanelWithDefaults = createAgentSessionPanelHarness({
      activeSession,
      viewMode: "diffs",
      diffMessages,
    });
    const renderPanel = (label: string) =>
      renderPanelWithDefaults({
        renderDiffCard: (message) => (
          <article>{`${label}: ${message.id}`}</article>
        ),
      });

    const { rerender } = render(renderPanel("Initial diff renderer"));
    expect(screen.getByText("Initial diff renderer: diff-1")).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel("Latest diff renderer"));
      await Promise.resolve();
    });

    expect(screen.queryByText("Initial diff renderer: diff-1")).not.toBeInTheDocument();
    expect(screen.getByText("Latest diff renderer: diff-1")).toBeInTheDocument();
  });

  it("uses the latest message renderer after a renderer-only parent rerender", async () => {
    const firstMessage: Message = {
      id: "message-1",
      type: "text",
      timestamp: "10:00",
      author: "assistant",
      text: "First",
    };
    const secondMessage: Message = {
      id: "message-2",
      type: "text",
      timestamp: "10:01",
      author: "assistant",
      text: "Second",
    };
    const activeSession = makeSession("session-a", {
      messages: [firstMessage],
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const scrollContainerRef = { current: document.createElement("section") };
    const matchedItemKeys = new Set<string>();
    const noopApproval = () => {};
    const noopUserInput = () => {};
    const noopElicitation = () => {};
    const noopAppRequest = () => {};
    const noopCancel = () => {};
    const noopSettingsChange = () => {};
    const noopSearchMount = () => {};

    const renderPanel = (label: string) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={scrollContainerRef}
        onApprovalDecision={noopApproval}
        onUserInputSubmit={noopUserInput}
        onMcpElicitationSubmit={noopElicitation}
        onCodexAppRequestSubmit={noopAppRequest}
        onCancelQueuedPrompt={noopCancel}
        onSessionSettingsChange={noopSettingsChange}
        conversationSearchQuery=""
        conversationSearchMatchedItemKeys={matchedItemKeys}
        conversationSearchActiveItemKey={null}
        onConversationSearchItemMount={noopSearchMount}
        renderCommandCard={() => null}
        renderDiffCard={() => null}
        renderMessageCard={(message) => (
          <article className="message-card">{`${label}: ${message.id}`}</article>
        )}
        renderPromptSettings={() => null}
      />
    );

    const { rerender } = render(renderPanel("Initial renderer"));
    expect(screen.getByText("Initial renderer: message-1")).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel("Latest renderer"));
      await Promise.resolve();
    });

    await act(async () => {
      syncComposerSessionsStore({
        sessions: [
          {
            ...activeSession,
            messages: [firstMessage, secondMessage],
          },
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
      await Promise.resolve();
    });

    expect(screen.getByText("Latest renderer: message-2")).toBeInTheDocument();
    expect(screen.queryByText("Initial renderer: message-1")).not.toBeInTheDocument();
    expect(screen.getByText("Latest renderer: message-1")).toBeInTheDocument();
  });

  it("dispatches Codex app requests through the latest parent callback after handler-only rerenders", async () => {
    const initialCodexAppRequestSubmit = vi.fn();
    const latestCodexAppRequestSubmit = vi.fn();
    const activeSession = makeSession("session-a", {
      messages: [
        {
          author: "assistant",
          id: "app-request-message",
          text: "App request",
          timestamp: "10:00",
          type: "text",
        },
      ],
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const renderPanel = (
      onCodexAppRequestSubmit: Parameters<typeof AgentSessionPanel>[0]["onCodexAppRequestSubmit"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={onCodexAppRequestSubmit}
        onCancelQueuedPrompt={() => {}}
        onSessionSettingsChange={() => {}}
        conversationSearchQuery=""
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onConversationSearchItemMount={() => {}}
        renderCommandCard={() => null}
        renderDiffCard={() => null}
        renderMessageCard={(message, _isLive, _approve, _input, _elicitation, submitAppRequest) => (
          <button
            type="button"
            onClick={() => submitAppRequest(message.id, { decision: "accepted" })}
          >
            Submit app request
          </button>
        )}
        renderPromptSettings={() => null}
      />
    );

    const { rerender } = render(renderPanel(initialCodexAppRequestSubmit));
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      rerender(renderPanel(latestCodexAppRequestSubmit));
      await Promise.resolve();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Submit app request" }));
      await Promise.resolve();
    });

    expect(initialCodexAppRequestSubmit).not.toHaveBeenCalled();
    expect(latestCodexAppRequestSubmit).toHaveBeenCalledWith(
      "session-a",
      "app-request-message",
      { decision: "accepted" },
    );
  });

  it("cancels queued prompts through the latest parent callback after handler-only rerenders", async () => {
    const initialCancelQueuedPrompt = vi.fn();
    const latestCancelQueuedPrompt = vi.fn();
    const activeSession = makeSession("session-a", {
      pendingPrompts: [
        {
          id: "queued-prompt",
          text: "queued prompt",
          timestamp: "10:00",
        },
      ],
    });
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const renderPanel = (
      onCancelQueuedPrompt: Parameters<typeof AgentSessionPanel>[0]["onCancelQueuedPrompt"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={() => {}}
        onMcpElicitationSubmit={() => {}}
        onCodexAppRequestSubmit={() => {}}
        onCancelQueuedPrompt={onCancelQueuedPrompt}
        onSessionSettingsChange={() => {}}
        conversationSearchQuery=""
        conversationSearchMatchedItemKeys={new Set()}
        conversationSearchActiveItemKey={null}
        onConversationSearchItemMount={() => {}}
        renderCommandCard={() => null}
        renderDiffCard={() => null}
        renderMessageCard={(message) => (
          <article className="message-card">{message.id}</article>
        )}
        renderPromptSettings={() => null}
      />
    );

    const { rerender } = render(renderPanel(initialCancelQueuedPrompt));
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      rerender(renderPanel(latestCancelQueuedPrompt));
      await Promise.resolve();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Cancel queued prompt" }));
      await Promise.resolve();
    });

    expect(initialCancelQueuedPrompt).not.toHaveBeenCalled();
    expect(latestCancelQueuedPrompt).toHaveBeenCalledWith(
      "session-a",
      "queued-prompt",
    );
  });

  it("refreshes the live turn tooltip from the latest session store record", () => {
    const initialSession = makeSession("active-session", {
      status: "active",
      messages: [
        {
          author: "you",
          id: "message-old",
          text: "old prompt",
          timestamp: "10:00",
          type: "text",
        },
      ],
    });
    renderSessionPanelWithDefaults({
      activeSession: initialSession,
      showWaitingIndicator: true,
      waitingIndicatorPrompt: "old prompt",
    });

    expect(screen.getByRole("tooltip")).toHaveTextContent("old prompt");

    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("active-session", {
            status: "active",
            messages: [
              ...initialSession.messages,
              {
                author: "assistant",
                id: "message-assistant",
                text: "working",
                timestamp: "10:01",
                type: "text",
              },
              {
                author: "you",
                id: "message-new",
                text: "new prompt",
                timestamp: "10:02",
                type: "text",
              },
            ],
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    expect(screen.getByRole("tooltip")).toHaveTextContent("new prompt");
    expect(screen.getByRole("tooltip")).not.toHaveTextContent("old prompt");
  });

});

describe("getAdjustedVirtualizedScrollTopForHeightChange", () => {
  it("preserves the viewport anchor when a measured message above the viewport changes height", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 900,
        nextHeight: 260,
        previousHeight: 180,
      }),
    ).toBe(1280);
  });
  it("does not jump the viewport when a partially visible message above the fold changes height", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1190,
        nextHeight: 200,
        previousHeight: 100,
      }),
    ).toBe(1200);
  });
  it("does not jump when a message has only just crossed above the viewport top", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1000,
        nextHeight: 260,
        previousHeight: 200,
      }),
    ).toBe(1200);
  });
  it("does not jump when a recently-passed message grows just above the viewport top", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 990,
        nextHeight: 260,
        previousHeight: 200,
      }),
    ).toBe(1200);
  });
  it("does not adjust when a message starts exactly at the viewport top", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1200,
        nextHeight: 260,
        previousHeight: 180,
      }),
    ).toBe(1200);
  });
  it("does not snap back when a newly visible message below the current viewport is measured", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1320,
        nextHeight: 260,
        previousHeight: 180,
      }),
    ).toBe(1200);
  });
  it("adjusts when a partially visible message above the fold shrinks", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 100,
        messageTop: 50,
        nextHeight: 60,
        previousHeight: 100,
      }),
    ).toBe(60);
  });
  it("floors negative height deltas at zero when the anchor would move above the top", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 50,
        messageTop: 20,
        nextHeight: 100,
        previousHeight: 200,
      }),
    ).toBe(0);
  });
});

describe("estimateConversationMessageHeight", () => {
  it("accounts for wrapped long plain-text messages", () => {
    const longLine = "x".repeat(160);

    expect(
      estimateConversationMessageHeight({
        id: "message-you",
        type: "text",
        timestamp: "2026-04-20T00:00:00.000Z",
        author: "you",
        text: longLine,
      }),
    ).toBe(126);

    expect(
      estimateConversationMessageHeight({
        id: "message-assistant",
        type: "text",
        timestamp: "2026-04-20T00:00:00.000Z",
        author: "assistant",
        text: longLine,
      }),
    ).toBe(136);
  });

  it("adds space for the expanded prompt toggle", () => {
    const base = estimateConversationMessageHeight({
      id: "message-base",
      type: "text",
      timestamp: "2026-04-20T00:00:00.000Z",
      author: "you",
      text: "hello",
    });
    const expanded = estimateConversationMessageHeight({
      id: "message-expanded",
      type: "text",
      timestamp: "2026-04-20T00:00:00.000Z",
      author: "you",
      text: "hello",
      expandedText: "details",
    });

    expect(expanded - base).toBe(40);
  });

  it("treats assistant text messages as markdown-shaped content", () => {
    const assistantMarkdown = [
      "## Goal",
      "",
      "1. Keep `App.tsx` exports listed explicitly.",
      "2. Split the reconnect/watchdog flow after deltas.",
      "3. Leave a smoke test in place.",
      "",
      "After that, hand the slice back for verification.",
    ].join("\n");
    const userCopy = assistantMarkdown;

    expect(
      estimateConversationMessageHeight({
        id: "message-assistant-markdown",
        type: "text",
        timestamp: "2026-04-20T00:00:00.000Z",
        author: "assistant",
        text: assistantMarkdown,
      }),
    ).toBeGreaterThan(
      estimateConversationMessageHeight({
        id: "message-user-copy",
        type: "text",
        timestamp: "2026-04-20T00:00:00.000Z",
        author: "you",
        text: userCopy,
      }),
    );
  });
});

describe("clampVirtualizedViewportScrollTop", () => {
  it("clamps stale restored scroll positions to the current virtualized layout", () => {
    expect(
      clampVirtualizedViewportScrollTop({
        scrollTop: 10_000,
        viewportHeight: 500,
        totalHeight: 2_000,
      }),
    ).toBe(1_500);
  });

  it("floors negative and non-finite scroll positions", () => {
    expect(
      clampVirtualizedViewportScrollTop({
        scrollTop: -200,
        viewportHeight: 500,
        totalHeight: 2_000,
      }),
    ).toBe(0);
    expect(
      clampVirtualizedViewportScrollTop({
        scrollTop: Number.NaN,
        viewportHeight: 500,
        totalHeight: 2_000,
      }),
    ).toBe(0);
  });
});

describe("getScrollContainerBottomGap", () => {
  // Small helper that accepts a plain object rather than a real DOM
  // element — the production signature is `Pick<HTMLElement,
  // "clientHeight" | "scrollHeight" | "scrollTop">` for exactly this
  // reason, so the boundary tests can pin behavior without touching
  // jsdom geometry.
  function node(
    scrollHeight: number,
    clientHeight: number,
    scrollTop: number,
  ) {
    return { scrollHeight, clientHeight, scrollTop };
  }

  it("returns the raw distance from scrollTop to the bottom when positive", () => {
    expect(getScrollContainerBottomGap(node(1000, 200, 0))).toBe(800);
    expect(getScrollContainerBottomGap(node(1000, 200, 100))).toBe(700);
    expect(getScrollContainerBottomGap(node(1000, 200, 799))).toBe(1);
  });

  it("floors negative gaps at zero when content is shorter than the viewport", () => {
    // Content shorter than viewport: `scrollHeight - clientHeight -
    // scrollTop` is negative. The helper must clamp to `0` so downstream
    // `<= N` / `< N` checks treat the viewport as "at bottom" without
    // special-casing the short-content layout.
    expect(getScrollContainerBottomGap(node(300, 400, 0))).toBe(0);
    expect(getScrollContainerBottomGap(node(200, 200, 0))).toBe(0);
  });
});

describe("isScrollContainerNearBottom", () => {
  function node(
    scrollHeight: number,
    clientHeight: number,
    scrollTop: number,
  ) {
    return { scrollHeight, clientHeight, scrollTop };
  }

  // The near-bottom threshold is `< 72 px`, intentionally chosen to
  // match the parent pane's sticky threshold in
  // `syncMessageStackScrollPosition`. A previous revision used
  // `<= 96 px` which created a 72-96 px "dead band" where the parent
  // had recorded `shouldStick: false` (the user scrolled up past the
  // 72 threshold) but a later virtualized measurement would still
  // re-pin the viewport to the latest message — the user felt
  // "snatched back" on code-block tokenization or image loads. These
  // boundary tests lock in the 72 value so anyone changing it in only
  // one file has to change it here too.
  it("returns true at gap 0 (exactly at bottom)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 800))).toBe(true);
  });
  it("returns true at gap 71 (1 px inside the near-bottom boundary)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 729))).toBe(true);
  });
  it("returns false at gap 72 (matches App.tsx strict-less-than sticky boundary)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 728))).toBe(false);
  });
  it("returns false at gap 96 (proves the 96-px band is no longer near-bottom)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 704))).toBe(false);
  });
  it("returns true for short content whose gap clamps to 0", () => {
    expect(isScrollContainerNearBottom(node(300, 400, 0))).toBe(true);
  });
});
