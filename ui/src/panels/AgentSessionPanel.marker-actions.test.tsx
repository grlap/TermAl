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
      "--conversation-active-marker-color": normalizeConversationMarkerColor(
        existingMarker.color,
      ),
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
    const { onCreateConversationMarker, trigger } =
      renderMarkerCreationHarness();
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
    const { onCreateConversationMarker, trigger } =
      renderMarkerCreationHarness();
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
          onUserInputSubmit={async () => {}}
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
          onUserInputSubmit={async () => {}}
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
          onUserInputSubmit={async () => {}}
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
      expect(
        container.querySelector(".session-conversation-page"),
      ).toHaveFocus();
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
          onUserInputSubmit={async () => {}}
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
          onUserInputSubmit={async () => {}}
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
    expect(
      within(markerWindow).getByText("No markers yet."),
    ).toBeInTheDocument();
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
        onUserInputSubmit={async () => {}}
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
        onUserInputSubmit={async () => {}}
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
    const requestAnimationFrameMock = vi.fn(
      (callback: FrameRequestCallback) => {
        const frameId = ++nextFrameId;
        frameCallbacks.set(frameId, callback);
        return frameId;
      },
    );
    const cancelAnimationFrameMock = vi.fn((frameId: number) => {
      frameCallbacks.delete(frameId);
    });
    const renderMessageCard = (message: Message) => (
      <MessageCard
        message={message}
        onApprovalDecision={() => {}}
        onUserInputSubmit={async () => {}}
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
      const latestFrameResult = requestAnimationFrameMock.mock.results[0];
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
          <div className="message-meta" data-conversation-marker-menu-trigger>
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
          onUserInputSubmit={async () => {}}
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
    expect(userMeta).toHaveAttribute("aria-label", "You, open marker actions");
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
          onUserInputSubmit={async () => {}}
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
          onUserInputSubmit={async () => {}}
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
          onUserInputSubmit={async () => {}}
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
    expect(
      screen.getByRole("button", { name: "Show tasks" }),
    ).toBeInTheDocument();

    const showTasksAgain = screen.getByRole("button", { name: "Show tasks" });
    showTasksAgain.focus();
    await user.keyboard(" ");

    expect(
      screen.queryByRole("menu", { name: "Conversation marker actions" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Hide tasks" }),
    ).toBeInTheDocument();
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
    const originalGetBoundingClientRectDescriptor =
      Object.getOwnPropertyDescriptor(
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
    ["ArrowUp", "Pin to board"],
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
      "Pin to board",
    ]);
  });


});
