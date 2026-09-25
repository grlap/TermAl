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

describe("AgentSessionPanel response-board actions", () => {
  it("drags from the message header while leaving the message body selectable", () => {
    const onPinResponseBoardMessage = vi.fn();
    const activeSession = makeSession("session-board-source", {
      messages: [
        {
          id: "message-board-source",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Do not serialize this body into the drag payload",
        },
      ],
    });
    const renderPanel = createAgentSessionPanelHarness({
      activeSession,
      onPinResponseBoardMessage,
      renderMessageCard: (message) => (
        <MessageCard
          message={message}
          onApprovalDecision={() => {}}
          onUserInputSubmit={async () => {}}
        />
      ),
    });
    const { container } = render(renderPanel());

    fireEvent.click(
      screen.getByRole("button", { name: "Agent, open marker actions" }),
    );
    const messageActions = screen.getByRole("menu", {
      name: "Conversation marker actions",
    });
    fireEvent.click(
      within(messageActions).getByRole("menuitem", { name: "Pin to board" }),
    );
    expect(onPinResponseBoardMessage).toHaveBeenCalledWith(
      "session-board-source",
      "message-board-source",
    );

    const values = new Map<string, string>();
    const setDragImage = vi.fn();
    const dataTransfer = {
      effectAllowed: "none",
      getData: (type: string) => values.get(type) ?? "",
      setData: (type: string, value: string) => values.set(type, value),
      setDragImage,
    } as unknown as DataTransfer;
    const shell = container.querySelector(".conversation-message-marker-shell");
    const header = container.querySelector(".message-meta");
    expect(shell).toBeTruthy();
    expect(header).toBeTruthy();
    expect(shell).not.toHaveAttribute("draggable");
    expect(header).toHaveAttribute("draggable", "true");

    fireEvent.dragStart(
      screen.getByText("Do not serialize this body into the drag payload"),
      { dataTransfer },
    );
    expect(values.has(RESPONSE_BOARD_MESSAGE_MIME)).toBe(false);

    fireEvent.dragStart(header as Element, { dataTransfer });
    expect(
      JSON.parse(values.get(RESPONSE_BOARD_MESSAGE_MIME) ?? "null"),
    ).toEqual({
      sessionId: "session-board-source",
      messageId: "message-board-source",
    });
    expect(setDragImage).toHaveBeenCalledWith(
      shell,
      expect.any(Number),
      expect.any(Number),
    );
  });
});

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

describe("AgentSessionPanel initial loading state", () => {
  it("shows connecting instead of an empty ready state before sessions arrive", () => {
    render(createAgentSessionPanelHarness()({ isLoading: true }));

    expect(screen.getByText("Connecting to backend")).toBeInTheDocument();
    expect(
      screen.getByText("Fetching session state from the Rust backend."),
    ).toBeInTheDocument();
    expect(screen.queryByText("Ready for a session")).toBeNull();
  });
});

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
    ["attached dash is an argument", "3 --flag", { argumentsText: "3 --flag" }],
    [
      "command example",
      "compare `git diff --no-index -- <round1> <current>`",
      { argumentsText: "compare `git diff --no-index -- <round1> <current>`" },
    ],
    [
      "note after command example",
      "compare `git diff -- a b` -- include tests",
      { argumentsText: "compare `git diff -- a b`", noteText: "include tests" },
    ],
    [
      "matching backtick widths",
      "compare ``literal ` -- example`` -- include tests",
      { argumentsText: "compare ``literal ` -- example``", noteText: "include tests" },
    ],
    [
      "unfinished example",
      "unfinished `git diff -- a b",
      { argumentsText: "unfinished `git diff -- a b" },
    ],
    ["unfinished double backtick", "`` -- note", { argumentsText: "`` -- note" }],
    [
      "multiple examples",
      "compare `a -- b` and `c -- d` -- note",
      { argumentsText: "compare `a -- b` and `c -- d`", noteText: "note" },
    ],
    ["Unicode text", "review → `diff -- a b`", { argumentsText: "review → `diff -- a b`" }],
  ])("splits %s separators", (_caseName, input, expected) => {
    expect(splitAgentCommandResolverTail(input)).toEqual(expected);
  });
});

describe("AgentSessionPanel conversation caching", () => {
  it("keeps the activity status node stable while explicit prompt context changes", () => {
    const { rerender } = render(
      <SessionActivityStrip session={{ agent: "Codex", status: "active", liveActivity: { prompt: "First prompt" } }} />,
    );
    const content = screen.getByRole("status");

    rerender(
      <SessionActivityStrip session={{ agent: "Codex", status: "active", liveActivity: { prompt: "Second prompt" } }} />,
    );

    expect(screen.getByRole("status")).toHaveTextContent("Second prompt");
    expect(screen.getByRole("status")).toBe(content);
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
          commandMessages={[]}
          diffMessages={[]}
          scrollContainerRef={
            {
              current: scrollNode,
            } as RefObject<HTMLElement | null>
          }
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
        expect(
          container.querySelector(".virtualized-message-list"),
        ).not.toBeNull();
        expect(screen.getByText("Old streamed answer")).toBeInTheDocument();
      });
      expect(
        screen.queryByText("Queued prompt duplicate"),
      ).not.toBeInTheDocument();

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
      expect(
        screen.queryByText("Queued prompt duplicate"),
      ).not.toBeInTheDocument();
      expect(
        container.querySelector(".virtualized-message-list"),
      ).not.toBeNull();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
    }
  });

  it("keeps the queued-prompt group calm while assistant output grows above queued prompts", () => {
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
    });

    const liveTail = document.querySelector(".conversation-queued-tail");
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
    expect(liveTail).not.toHaveAttribute("data-tail-follow");
    expect(document.querySelector(".activity-card-live")).toBeNull();
    expect(pendingPromptQueue?.closest(".conversation-queued-tail")).toBe(
      liveTail,
    );
    expect(
      Boolean(
        firstQueuedPromptCard!.compareDocumentPosition(
          secondQueuedPromptCard!,
        ) & Node.DOCUMENT_POSITION_FOLLOWING,
      ),
    ).toBe(true);
    expect(liveTail).toContainElement(firstQueuedPromptCard as HTMLElement);
    expect(liveTail).toContainElement(
      secondQueuedPromptCard as HTMLElement,
    );
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

    expect(document.querySelector(".conversation-queued-tail")).toBe(liveTail);
    expect(screen.getByText("Queued follow-up A").closest(".pending-prompt-card")).toBe(firstQueuedPromptCard);
    expect(scrollTop).toBe(120);
    expect(scrollWrites).toEqual([]);
  });

  it("keeps the queued-prompt group in normal flow across attachment state", async () => {
    const nodeFsModule = "node:fs";
    const { readFileSync } = (await import(nodeFsModule)) as {
      readFileSync: (path: string, encoding: "utf8") => string;
    };
    const runtimeProcess = (
      globalThis as typeof globalThis & {
        process: { cwd: () => string };
      }
    ).process;
    const stylesCss = readFileSync(
      `${runtimeProcess.cwd()}/src/styles.css`,
      "utf8",
    );
    const messageStackDeclarations = stylesCss.match(
      /\.message-stack\s*\{([^}]*)\}/s,
    )?.[1];
    expect(messageStackDeclarations).toMatch(
      /--message-stack-block-padding\s*:\s*var\(--space-2xl\)\s*;/,
    );
    const baseDeclarations = stylesCss.match(
      /\.conversation-queued-tail\s*\{([^}]*)\}/s,
    )?.[1];
    expect(baseDeclarations).toBeDefined();
    expect(baseDeclarations).not.toMatch(
      /(?:^|;)\s*position\s*:\s*(?:sticky|fixed|absolute)\s*(?:;|$)/,
    );
    const attachedDeclarations = stylesCss.match(
      /\.conversation-queued-tail\[data-tail-follow="attached"\]\s*\{([^}]*)\}/s,
    )?.[1];
    // Attachment is scroll-controller intent only. Synchronous pre-paint
    // growth correction keeps the in-flow tail stable without a second
    // positioning authority during the first manual movement.
    expect(attachedDeclarations).toBeUndefined();

    const activeSession = makeSession("session-a", {
      status: "active",
      pendingPrompts: [{ id: "queued-flow", timestamp: "10:01", text: "Queued in flow" }],
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
    });
    const { rerender } = render(renderPanel({ liveTailPinned: true }));

    const liveTail = screen.getByText("Queued in flow").closest(".conversation-queued-tail");
    expect(liveTail).not.toBeNull();
    expect(liveTail).not.toHaveAttribute("data-tail-follow");

    rerender(renderPanel({ liveTailPinned: false }));
    expect(liveTail).not.toHaveAttribute("data-tail-follow");
    expect(liveTail).toHaveClass("conversation-queued-tail");
  });

  it("reveals only appended transcript entries with paint-only motion", async () => {
    const nodeFsModule = "node:fs";
    const { readFileSync } = (await import(nodeFsModule)) as {
      readFileSync: (path: string, encoding: "utf8") => string;
    };
    const runtimeProcess = (
      globalThis as typeof globalThis & {
        process: { cwd: () => string };
      }
    ).process;
    const stylesCss = readFileSync(
      `${runtimeProcess.cwd()}/src/styles.css`,
      "utf8",
    );
    const initialMessages = makeTextMessages(30);
    const activeSession = makeSession("session-message-reveal", {
      messages: initialMessages,
      messagesLoaded: true,
    });
    const renderPanel = createAgentSessionPanelHarness({ activeSession });
    const { container } = render(renderPanel());

    expect(container.querySelector(".virtualized-message-list")).not.toBeNull();
    const initialShell = screen
      .getByText("message-30")
      .closest(".conversation-message-marker-shell");
    expect(initialShell).not.toBeNull();
    expect(initialShell).not.toHaveClass("conversation-message-entry-reveal");

    const appendedMessage: Message = {
      id: "message-reveal-new",
      type: "text",
      timestamp: "10:02",
      author: "assistant",
      text: "New reply",
    };
    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-message-reveal", {
            messages: [...initialMessages, appendedMessage],
            messagesLoaded: true,
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const appendedShell = await waitFor(() => {
      const shell = screen
        .getByText("message-reveal-new")
        .closest(".conversation-message-marker-shell");
      expect(shell).toHaveClass("conversation-message-entry-reveal");
      return shell;
    });
    expect(initialShell).not.toHaveClass("conversation-message-entry-reveal");
    expect(appendedShell?.closest(".message-slot")).not.toHaveClass(
      "conversation-message-entry-reveal",
    );
    expect(
      container.querySelector(
        ".message-stack.conversation-message-entry-reveal",
      ),
    ).toBeNull();
    expect(
      container.querySelector(
        ".conversation-queued-tail.conversation-message-entry-reveal",
      ),
    ).toBeNull();

    const revealDeclarations = extractCssBlock(
      stylesCss,
      /(?:^|\n)\.conversation-message-entry-reveal\s*(?=\{)/,
    );
    expect(revealDeclarations).toMatch(
      /animation\s*:\s*conversation-message-entry-reveal\s+180ms\s+ease-out\s+backwards\s*;/,
    );
    const cancelledRevealDeclarations = extractCssBlock(
      stylesCss,
      /(?:^|\n)\.conversation-message-entry-reveal\[\s*data-conversation-message-entry-reveal-cancelled\s*\]\s*(?=\{)/,
    );
    expect(cancelledRevealDeclarations).toMatch(/animation\s*:\s*none\s*;/);
    const revealKeyframes = extractCssBlock(
      stylesCss,
      /@keyframes\s+conversation-message-entry-reveal\s*(?=\{)/,
    );
    expect(revealKeyframes).toMatch(/from\s*\{\s*opacity\s*:\s*0\s*;/s);
    expect(revealKeyframes).toMatch(/transform\s*:\s*translateY\(6px\)\s*;/);
    expect(revealKeyframes).toMatch(/to\s*\{\s*opacity\s*:\s*1\s*;/s);
    expect(revealKeyframes).toMatch(/transform\s*:\s*translateY\(0\)\s*;/);

    const forbiddenGeometryProperty =
      /(?:^|[;{}\s])(?:block-size|bottom|gap|height|inset|left|margin|max-height|min-height|padding|position|right|scroll-margin|top)\s*:/m;
    expect(revealDeclarations).not.toMatch(forbiddenGeometryProperty);
    expect(revealDeclarations).not.toMatch(/(?:transform|translate)\s*:/);
    expect(revealKeyframes).not.toMatch(forbiddenGeometryProperty);

    const reducedMotionRules = extractCssBlock(
      stylesCss,
      /@media\s*\(prefers-reduced-motion:\s*reduce\)\s*(?=\{)/,
    );
    expect(reducedMotionRules).toMatch(
      /\.conversation-message-entry-reveal\s*\{\s*animation\s*:\s*none\s*;/s,
    );
  });

  it("keeps append identity through a metadata-first message-count update", async () => {
    const initialMessages = makeTextMessages(30);
    const activeSession = makeSession("session-message-reveal-metadata", {
      messageCount: 30,
      messages: initialMessages,
      messagesLoaded: true,
    });
    const renderPanel = createAgentSessionPanelHarness({ activeSession });
    render(renderPanel());

    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-message-reveal-metadata", {
            messageCount: 31,
            messages: initialMessages,
            messagesLoaded: true,
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const appendedMessage: Message = {
      id: "message-metadata-late",
      type: "text",
      timestamp: "10:30",
      author: "assistant",
      text: "Arrived after metadata",
    };
    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("session-message-reveal-metadata", {
            messageCount: 31,
            messages: [...initialMessages, appendedMessage],
            messagesLoaded: true,
          }),
        ],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    await waitFor(() => {
      expect(
        screen
          .getByText("message-metadata-late")
          .closest(".conversation-message-marker-shell"),
      ).toHaveClass("conversation-message-entry-reveal");
    });
  });

  it("stacks the prompt above composer actions based on pane width", async () => {
    const nodeFsModule = "node:fs";
    const { readFileSync } = (await import(nodeFsModule)) as {
      readFileSync: (path: string, encoding: "utf8") => string;
    };
    const runtimeProcess = (
      globalThis as typeof globalThis & {
        process: { cwd: () => string };
      }
    ).process;
    const stylesCss = readFileSync(
      `${runtimeProcess.cwd()}/src/styles.css`,
      "utf8",
    );

    const composerDeclarations = extractCssBlock(
      stylesCss,
      /(?:^|\n)\.composer\s*(?=\{)/,
    );
    expect(composerDeclarations).toMatch(
      /container-name\s*:\s*session-composer\s*;/,
    );
    expect(composerDeclarations).toMatch(/container-type\s*:\s*inline-size\s*;/);

    const composerActionSplit = extractCssBlock(
      stylesCss,
      /(?:^|\n)\.composer-action-split\s*(?=\{)/,
    );
    expect(composerActionSplit).toMatch(/justify-self\s*:\s*end\s*;/);
    expect(composerActionSplit).toMatch(/width\s*:\s*max-content\s*;/);
    expect(composerActionSplit).toMatch(/min-width\s*:\s*12\.5rem\s*;/);

    const composerActionTrigger = extractCssBlock(
      stylesCss,
      /(?:^|\n)\.composer-action-split\s*>\s*\.composer-action-menu-trigger\s*(?=\{)/,
    );
    expect(composerActionTrigger).toMatch(/flex\s*:\s*0\s+0\s+2\.35rem\s*;/);
    expect(composerActionTrigger).toMatch(/width\s*:\s*2\.35rem\s*;/);

    const composerActionMenu = extractCssBlock(
      stylesCss,
      /(?:^|\n)\.composer-action-menu\s*(?=\{)/,
    );
    expect(composerActionMenu).toMatch(
      /background\s*:\s*linear-gradient\([^}]*var\(--paper-strong\)/,
    );

    const narrowComposerRules = extractCssBlock(
      stylesCss,
      /@container\s+session-composer\s*\(\s*max-width\s*:\s*46rem\s*\)(?=\s*\{)/,
    );
    const narrowComposerRow = extractCssBlock(
      narrowComposerRules,
      /(?:^|\n)\s*\.composer-row\s*(?=\{)/,
    );
    expect(narrowComposerRow).toMatch(
      /grid-template-columns\s*:\s*minmax\(0,\s*1fr\)\s*;/,
    );
    const narrowComposerActions = extractCssBlock(
      narrowComposerRules,
      /(?:^|\n)\s*\.composer-actions\s*(?=\{)/,
    );
    expect(narrowComposerActions).toMatch(
      /grid-template-columns\s*:\s*repeat\(2,\s*minmax\(0,\s*1fr\)\)\s*;/,
    );
    const narrowComposerActionSplit = extractCssBlock(
      narrowComposerRules,
      /(?:^|\n)\s*\.composer-actions\s*>\s*\.composer-action-split\s*(?=\{)/,
    );
    expect(narrowComposerActionSplit).toMatch(/width\s*:\s*max-content\s*;/);
    expect(narrowComposerActionSplit).toMatch(/min-width\s*:\s*12\.5rem\s*;/);
    expect(narrowComposerActionSplit).toMatch(/justify-self\s*:\s*end\s*;/);

    const compactComposerRules = extractCssBlock(
      stylesCss,
      /@container\s+session-composer\s*\(\s*max-width\s*:\s*20rem\s*\)(?=\s*\{)/,
    );
    const compactComposerActions = extractCssBlock(
      compactComposerRules,
      /(?:^|\n)\s*\.composer-actions\s*(?=\{)/,
    );
    expect(compactComposerActions).toMatch(
      /grid-template-columns\s*:\s*minmax\(0,\s*1fr\)\s*;/,
    );
  });

  it("does not splice live-only cards beneath a historical window", async () => {
    renderSessionPanelWithDefaults({
      activeSession: makeSession("session-a", {
        status: "active",
        messages: makeTextMessages(64),
        messagesLoaded: false,
        hasOlderHistory: false,
        hasNewerHistory: true,
        messageCount: 1_000,
        pendingPrompts: [
          {
            id: "pending-live-prompt",
            timestamp: "10:02",
            text: "Queued at the live tail",
          },
        ],
      }),
      liveTailPinned: true,
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(screen.queryByText("Live turn")).not.toBeInTheDocument();
    expect(
      screen.queryByText("Queued at the live tail"),
    ).not.toBeInTheDocument();
    expect(
      document.querySelector(".conversation-queued-tail"),
    ).not.toBeInTheDocument();
  });

  it("shows idle status after visible agent output", () => {
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
    });
    render(<SessionActivityStrip session={activeSession} />);

    expect(screen.getByRole("status")).toHaveTextContent("Codex is idle");
    expect(document.querySelector(".activity-card-live")).toBeNull();
  });

  it("keeps delegation status available after prior assistant output", () => {
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
    });
    render(<SessionActivityStrip session={activeSession} delegationWaitPrompt="Waiting on 1 delegation wait covering 2 delegated sessions: review fan-in" />);

    expect(screen.getByRole("status")).toHaveTextContent("Codex is waiting for delegated sessions");
    expect(document.querySelector(".activity-card-live")).toBeNull();
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
    });
    render(<SessionActivityStrip session={activeSession} isSending />);

    expect(screen.getByRole("status")).toHaveTextContent("Codex is sending a prompt");
    expect(document.querySelector(".activity-card-live")).toBeNull();
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
    });
    render(<SessionActivityStrip session={activeSession} isSending />);

    expect(screen.getByRole("status")).toHaveTextContent("Codex is working");
    expect(document.querySelector(".activity-card-live")).toBeNull();
  });

  it("keeps working status while active, even after file-change output", () => {
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
    });
    render(<SessionActivityStrip session={activeSession} />);

    expect(screen.getByRole("status")).toHaveTextContent("Codex is working");
    expect(document.querySelector(".activity-card-live")).toBeNull();
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
        "--conversation-active-marker-color": normalizeConversationMarkerColor(
          DEFAULT_CONVERSATION_MARKER_COLOR,
        ),
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
        delete (Element.prototype as { scrollIntoView?: unknown })
          .scrollIntoView;
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
    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;

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
        scrollIntoViewTargets.filter(
          (target) => target === "message:message-1",
        ),
      ).toHaveLength(1);
      await act(async () => {
        await new Promise<void>((resolve) => {
          window.requestAnimationFrame(() => {
            window.requestAnimationFrame(() => resolve());
          });
        });
      });
      expect(
        scrollIntoViewTargets.filter(
          (target) => target === "message:message-1",
        ),
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
          commandMessages={[]}
          diffMessages={[]}
          scrollContainerRef={{ current: detachedScrollRoot }}
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
  function node(scrollHeight: number, clientHeight: number, scrollTop: number) {
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
  function node(scrollHeight: number, clientHeight: number, scrollTop: number) {
    return { scrollHeight, clientHeight, scrollTop };
  }

  // The near-bottom threshold uses the same shared sticky-bottom band as the
  // parent pane. A previous revision used
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

describe("isScrollContainerAtPhysicalBottom", () => {
  function node(scrollHeight: number, clientHeight: number, scrollTop: number) {
    return { scrollHeight, clientHeight, scrollTop };
  }

  it("accepts the reachable fractional bottom tolerance", () => {
    expect(isScrollContainerAtPhysicalBottom(node(1000, 200, 796.5))).toBe(
      true,
    );
  });

  it("does not treat the wider sticky layout band as physical bottom", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 760))).toBe(true);
    expect(isScrollContainerAtPhysicalBottom(node(1000, 200, 760))).toBe(
      false,
    );
  });
});
