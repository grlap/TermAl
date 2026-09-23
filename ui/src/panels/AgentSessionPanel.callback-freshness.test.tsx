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
      onApprovalDecision: Parameters<
        typeof AgentSessionPanel
      >[0]["onApprovalDecision"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={onApprovalDecision}
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
        renderMessageCard={(message, _isLive, approve) => (
          <button type="button" onClick={() => approve(message.id, "accepted")}>
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
      onSessionSettingsChange: Parameters<
        typeof AgentSessionPanel
      >[0]["onSessionSettingsChange"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="prompt"
        activeSessionId={activeSession.id}
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
      fireEvent.click(
        screen.getByRole("button", { name: "Apply latest settings" }),
      );
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

    expect(
      screen.queryByText("Initial prompt renderer"),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Latest prompt renderer")).toBeInTheDocument();
  });

  it("refreshes command cards when only the command renderer changes", async () => {
    const commandMessages = makeCommandMessages(1);
    const activeSession = makeSession("session-a", {
      messages: commandMessages,
    });

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
    expect(
      screen.getByText("Initial command renderer: command-1"),
    ).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel("Latest command renderer"));
      await Promise.resolve();
    });

    expect(
      screen.queryByText("Initial command renderer: command-1"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("Latest command renderer: command-1"),
    ).toBeInTheDocument();
  });

  it("refreshes diff cards when only the diff renderer changes", async () => {
    const diffMessages = makeDiffMessages(1);
    const activeSession = makeSession("session-a", {
      messages: diffMessages,
    });

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
    expect(
      screen.getByText("Initial diff renderer: diff-1"),
    ).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel("Latest diff renderer"));
      await Promise.resolve();
    });

    expect(
      screen.queryByText("Initial diff renderer: diff-1"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("Latest diff renderer: diff-1"),
    ).toBeInTheDocument();
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
    const noopUserInput = async () => {};
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
    expect(
      screen.queryByText("Initial renderer: message-1"),
    ).not.toBeInTheDocument();
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
      onCodexAppRequestSubmit: Parameters<
        typeof AgentSessionPanel
      >[0]["onCodexAppRequestSubmit"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
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
        renderMessageCard={(
          message,
          _isLive,
          _approve,
          _input,
          _elicitation,
          submitAppRequest,
        ) => (
          <button
            type="button"
            onClick={() =>
              submitAppRequest(message.id, { decision: "accepted" })
            }
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
      fireEvent.click(
        screen.getByRole("button", { name: "Submit app request" }),
      );
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
      onCancelQueuedPrompt: Parameters<
        typeof AgentSessionPanel
      >[0]["onCancelQueuedPrompt"],
    ) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="session"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        commandMessages={[]}
        diffMessages={[]}
        scrollContainerRef={{ current: document.createElement("section") }}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
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
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel queued prompt" }),
      );
      await Promise.resolve();
    });

    expect(initialCancelQueuedPrompt).not.toHaveBeenCalled();
    expect(latestCancelQueuedPrompt).toHaveBeenCalledWith(
      "session-a",
      "queued-prompt",
    );
  });

  it("refreshes the activity tooltip from the latest session store record", () => {
    function StoredActivity() {
      const session = useSessionRecordSnapshot("active-session");
      return session ? <SessionActivityStrip session={session} /> : null;
    }
    const initialSession = makeSession("active-session", {
      status: "active",
      liveActivity: { prompt: "old prompt" },
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
    });
    render(<StoredActivity />);

    expect(screen.getByRole("tooltip")).toHaveTextContent("old prompt");

    act(() => {
      syncComposerSessionsStore({
        sessions: [
          makeSession("active-session", {
            status: "active",
            liveActivity: { prompt: "new prompt" },
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
