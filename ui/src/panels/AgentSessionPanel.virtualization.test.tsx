// Owns AgentSessionPanel transcript virtualization and overview behavior tests.
// Does not own footer/composer behavior or scroll-gesture virtualization tests.
// Split from ui/src/panels/AgentSessionPanel.test.tsx.

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
  includeUndeferredMessageTail,
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

describe("AgentSessionPanel virtualization", () => {
  it("renders only the active session transcript DOM", () => {
    const cachedSession = makeSession("cached-session", {
      messages: makeTextMessages(85).map((message, index) => ({
        ...message,
        id: `cached-message-${index + 1}`,
      })),
    });
    const activeSession = makeSession("active-session", {
      messages: makeTextMessages(1),
    });

    const { container } = renderSessionPanelWithDefaults({
      activeSession,
    });

    expect(
      screen.queryByText(cachedSession.messages[0]?.id ?? ""),
    ).not.toBeInTheDocument();
    expect(
      container.querySelectorAll(".session-conversation-page"),
    ).toHaveLength(1);
    expect(container.querySelector(".virtualized-message-list")).toBeNull();
    expect(screen.getByText(activeSession.messages[0]?.id ?? "")).toBeInTheDocument();
  });

  it("renders a conversation overview rail for long active sessions after the initial transcript paint", async () => {
    const OriginalResizeObserver = window.ResizeObserver;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    try {
      const { container } = renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "active",
          messages: makeTextMessages(90),
        }),
        showWaitingIndicator: true,
        waitingIndicatorPrompt: "run the build",
      });

      expect(screen.queryByLabelText("Conversation overview")).not.toBeInTheDocument();
      expect(
        container.querySelector(
          ".session-conversation-page.has-conversation-overview-scroll",
        ),
      ).not.toBeNull();
      expect(container.querySelector(".conversation-with-overview")).not.toBeNull();
      expect(
        container.querySelector(".conversation-overview-rail.is-pending"),
      ).not.toBeNull();
      const overviewContentBefore = container.querySelector(
        ".conversation-overview-content",
      );
      const virtualizedListBefore = container.querySelector(
        ".virtualized-message-list",
      );
      expect(overviewContentBefore).not.toBeNull();
      expect(virtualizedListBefore).not.toBeNull();

      const rail = await screen.findByLabelText("Conversation overview");
      expect(screen.getAllByLabelText("Conversation overview")).toHaveLength(1);
      expect(rail).toBeInTheDocument();
      expect(container.querySelector(".conversation-overview-content")).toBe(
        overviewContentBefore,
      );
      expect(container.querySelector(".virtualized-message-list")).toBe(
        virtualizedListBefore,
      );
      expect(
        rail.closest(".conversation-with-overview")?.querySelector(".activity-card-live"),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: /^User prompt 1:/ }),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", {
          name: /^Live turn 91: Codex is working — Waiting for output/,
        }),
      ).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
    }
  });

  it("keeps the first long-session tail window until older transcript is requested", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const OriginalTouchEvent = window.TouchEvent;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    class TouchEventMock extends Event {
      readonly changedTouches: Touch[];
      readonly touches: Touch[];

      constructor(type: string, init: TouchEventInit = {}) {
        super(type, { bubbles: init.bubbles ?? true, cancelable: init.cancelable });
        this.changedTouches = init.changedTouches ?? [];
        this.touches = init.touches ?? [];
      }
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.TouchEvent = TouchEventMock as unknown as typeof TouchEvent;
    try {
      const messages = makeTextMessages(600);
      document.body.append(scrollNode);
      const { container } = renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages,
        }),
        scrollContainerRef: { current: scrollNode },
      });

      expect(screen.queryByLabelText("Conversation overview")).not.toBeInTheDocument();
      expect(screen.getByText("message-600")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });
      expect(screen.queryByLabelText("Conversation overview")).not.toBeInTheDocument();
      expect(screen.getByText("message-600")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5_000);
      });
      expect(screen.getByLabelText("Conversation overview")).toBeInTheDocument();
      expect(container.querySelector(".conversation-with-overview")).not.toBeNull();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(50);
        fireEvent.scroll(scrollNode);
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-193")).not.toBeInTheDocument();

      act(() => {
        fireEvent.wheel(scrollNode, { ctrlKey: true, deltaY: -120 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-193")).not.toBeInTheDocument();

      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: -4 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-561")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(160);
        fireEvent.wheel(scrollNode, { deltaY: -7 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-561")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(161);
        fireEvent.wheel(scrollNode, { deltaY: -8 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-561")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(20_000);
        fireEvent.wheel(scrollNode, { deltaY: -120 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-193")).not.toBeInTheDocument();

      act(() => {
        scrollNode.dispatchEvent(new TouchEvent("touchstart", {
          bubbles: true,
          touches: [{ clientY: 100 } as Touch],
          changedTouches: [{ clientY: 100 } as Touch],
        }));
        scrollNode.dispatchEvent(new TouchEvent("touchmove", {
          bubbles: true,
          touches: [{ clientY: 104 } as Touch],
          changedTouches: [{ clientY: 104 } as Touch],
        }));
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.queryByText("message-193")).not.toBeInTheDocument();

      const editable = document.createElement("textarea");
      scrollNode.append(editable);
      try {
        act(() => {
          fireEvent.keyDown(editable, { key: "PageUp" });
        });
        await act(async () => {
          await vi.advanceTimersByTimeAsync(500);
        });
        expect(screen.queryByText("message-193")).not.toBeInTheDocument();
      } finally {
        editable.remove();
      }

      const outsideTarget = document.createElement("button");
      document.body.append(outsideTarget);
      try {
        const nonDemandComposedPath = vi.fn(() => [
          outsideTarget,
          document.body,
          document,
          window,
        ]);
        const nonDemandKey = new KeyboardEvent("keydown", {
          bubbles: true,
          key: "a",
        });
        Object.defineProperty(nonDemandKey, "composedPath", {
          value: nonDemandComposedPath,
        });
        act(() => {
          outsideTarget.dispatchEvent(nonDemandKey);
        });
        expect(nonDemandComposedPath).not.toHaveBeenCalled();

        act(() => {
          fireEvent.keyDown(outsideTarget, { key: "Home" });
        });
        await act(async () => {
          await vi.advanceTimersByTimeAsync(500);
        });
        expect(screen.queryByText("message-193")).not.toBeInTheDocument();
      } finally {
        outsideTarget.remove();
      }

      expect(screen.getByLabelText("Conversation overview")).toBeInTheDocument();
      expect(screen.getByText("message-597")).toBeInTheDocument();
      expect(screen.queryByText("message-193")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(160);
        fireEvent.wheel(scrollNode, { deltaY: -8 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(screen.getByText("message-561")).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.TouchEvent = OriginalTouchEvent;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("keeps the first long-session tail window under StrictMode", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      document.body.append(scrollNode);
      render(
        <StrictMode>
          {createAgentSessionPanelHarness({
            activeSession: makeSession("active-session", {
              status: "idle",
              messages: makeTextMessages(600),
            }),
            scrollContainerRef: { current: scrollNode },
          })()}
        </StrictMode>,
      );

      expect(screen.getByText("message-600")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(screen.getByText("message-600")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("hydrates and retries prompt navigation when the target starts outside the active tail window", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      document.body.append(scrollNode);
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages: makeTextMessages(600),
        }),
        scrollContainerRef: { current: scrollNode },
        renderMessageCard: renderNavigableMessageCard,
      });

      expect(screen.getByText("message-581")).toBeInTheDocument();
      expect(screen.queryByText("message-579")).not.toBeInTheDocument();

      fireEvent.click(
        screen.getAllByRole("button", {
          name: "Jump to previous prompt",
        })[0]!,
      );
      scrollNodeMocks.setScrollHeight(90_000);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(screen.getByText("message-579")).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("hydrates and retries delegation navigation when the target starts outside the active tail window", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const messages = makeTextMessages(600).map((message) => ({
      ...message,
      author: "assistant" as const,
    }));
    messages[569] = {
      id: "message-570",
      type: "parallelAgents",
      author: "assistant",
      timestamp: "10:570",
      agents: [
        {
          id: "delegation-570",
          source: "delegation",
          title: "Off-window review",
          status: "completed",
          detail: "Done",
        },
      ],
    };
    messages[589] = {
      id: "message-590",
      type: "parallelAgents",
      author: "assistant",
      timestamp: "10:590",
      agents: [
        {
          id: "delegation-590",
          source: "delegation",
          title: "Visible review",
          status: "completed",
          detail: "Done",
        },
      ],
    };

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      document.body.append(scrollNode);
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages,
        }),
        scrollContainerRef: { current: scrollNode },
        renderMessageCard: renderNavigableMessageCard,
      });

      expect(screen.getByText("message-590")).toBeInTheDocument();
      expect(screen.queryByText("message-570")).not.toBeInTheDocument();

      fireEvent.click(
        screen.getByRole("button", {
          name: "Jump to previous delegation",
        }),
      );
      scrollNodeMocks.setScrollHeight(90_000);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(screen.getByText("message-570")).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("hydrates a long-session tail after a native-scrollbar mousedown", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const messages = makeTextMessages(600);
      document.body.append(scrollNode);
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages,
        }),
        scrollContainerRef: { current: scrollNode },
      });

      expect(screen.queryByText("message-1")).not.toBeInTheDocument();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5_000);
      });
      expect(screen.getByLabelText("Conversation overview")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      act(() => {
        fireEvent.mouseDown(scrollNode);
        scrollNodeMocks.setScrollTop(50);
        fireEvent.scroll(scrollNode);
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(screen.getByText("message-1")).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("does not hydrate a long-session tail after mousedown inside transcript content", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);
    const transcriptChild = document.createElement("div");

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const messages = makeTextMessages(600);
      document.body.append(scrollNode);
      scrollNode.append(transcriptChild);
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages,
        }),
        scrollContainerRef: { current: scrollNode },
      });

      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(50);
        fireEvent.mouseDown(transcriptChild);
        fireEvent.scroll(scrollNode);
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(screen.queryByText("message-1")).not.toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      transcriptChild.remove();
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("keeps demand-hydration listeners bound across message arrivals while tail-windowed", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeDemandEvents = [
      "scroll",
      "wheel",
      "mousedown",
      "touchstart",
      "touchmove",
      "touchend",
      "touchcancel",
    ] as const;
    const addCounts = new Map<string, number>();
    const removeCounts = new Map<string, number>();
    let documentKeydownAdds = 0;
    let documentKeydownRemoves = 0;
    const originalAdd = scrollNode.addEventListener.bind(scrollNode);
    const originalRemove = scrollNode.removeEventListener.bind(scrollNode);
    const originalDocumentAdd = document.addEventListener.bind(document);
    const originalDocumentRemove = document.removeEventListener.bind(document);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

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
      get: () => 24_000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => 20_000,
      set: () => {},
    });
    scrollNode.addEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (scrollNodeDemandEvents.includes(type as (typeof scrollNodeDemandEvents)[number])) {
        addCounts.set(type, (addCounts.get(type) ?? 0) + 1);
      }
      return originalAdd(type, listener, options);
    }) as typeof scrollNode.addEventListener;
    scrollNode.removeEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: EventListenerOptions | boolean,
    ) => {
      if (scrollNodeDemandEvents.includes(type as (typeof scrollNodeDemandEvents)[number])) {
        removeCounts.set(type, (removeCounts.get(type) ?? 0) + 1);
      }
      return originalRemove(type, listener, options);
    }) as typeof scrollNode.removeEventListener;
    document.addEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (type === "keydown") {
        documentKeydownAdds += 1;
      }
      return originalDocumentAdd(type, listener, options);
    }) as typeof document.addEventListener;
    document.removeEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: EventListenerOptions | boolean,
    ) => {
      if (type === "keydown") {
        documentKeydownRemoves += 1;
      }
      return originalDocumentRemove(type, listener, options);
    }) as typeof document.removeEventListener;

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    try {
      const initialMessages = makeTextMessages(600);
      const { unmount } = renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages: initialMessages,
        }),
        scrollContainerRef: { current: scrollNode },
      });
      await act(async () => {
        await Promise.resolve();
      });

      const baselineAddCounts = new Map(addCounts);
      const baselineRemoveCounts = new Map(removeCounts);
      const baselineDocumentKeydownAdds = documentKeydownAdds;
      const baselineDocumentKeydownRemoves = documentKeydownRemoves;
      scrollNodeDemandEvents.forEach((eventName) => {
        expect(baselineAddCounts.get(eventName)).toBeGreaterThan(0);
      });
      expect(baselineDocumentKeydownAdds).toBeGreaterThan(0);

      act(() => {
        syncComposerSessionsStore({
          sessions: [
            makeSession("active-session", {
              status: "idle",
              messages: [
                ...initialMessages,
                {
                  author: "assistant",
                  id: "message-601",
                  text: "message-601",
                  timestamp: "10:00",
                  type: "text",
                },
              ],
            }),
          ],
          draftsBySessionId: {},
          draftAttachmentsBySessionId: {},
        });
      });
      await act(async () => {
        await Promise.resolve();
      });

      scrollNodeDemandEvents.forEach((eventName) => {
        expect(addCounts.get(eventName)).toBe(baselineAddCounts.get(eventName));
        expect(removeCounts.get(eventName)).toBe(
          baselineRemoveCounts.get(eventName),
        );
      });
      expect(documentKeydownAdds).toBe(baselineDocumentKeydownAdds);
      expect(documentKeydownRemoves).toBe(baselineDocumentKeydownRemoves);

      unmount();

      scrollNodeDemandEvents.forEach((eventName) => {
        expect(removeCounts.get(eventName)).toBeGreaterThanOrEqual(
          (baselineRemoveCounts.get(eventName) ?? 0) + 1,
        );
      });
      expect(documentKeydownRemoves).toBeGreaterThanOrEqual(
        baselineDocumentKeydownRemoves + 1,
      );
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      document.addEventListener = originalDocumentAdd;
      document.removeEventListener = originalDocumentRemove;
    }
  });

  it("hydrates from retained demand listeners after a message arrives", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const initialMessages = makeTextMessages(600);
      document.body.append(scrollNode);
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages: initialMessages,
        }),
        scrollContainerRef: { current: scrollNode },
      });
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      act(() => {
        syncComposerSessionsStore({
          sessions: [
            makeSession("active-session", {
              status: "idle",
              messages: [
                ...initialMessages,
                {
                  author: "assistant",
                  id: "message-601",
                  text: "message-601",
                  timestamp: "10:00",
                  type: "text",
                },
              ],
            }),
          ],
          draftsBySessionId: {},
          draftAttachmentsBySessionId: {},
        });
      });
      await act(async () => {
        await Promise.resolve();
      });
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();
      expect(screen.queryByLabelText("Conversation overview")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(50);
        fireEvent.wheel(scrollNode, { deltaY: -120 });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(screen.getByLabelText("Conversation overview")).toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it.each(["ArrowUp", "Home", "PageUp"] as const)(
    "hydrates a long-session tail after %s inside the transcript",
    async (key) => {
      const OriginalResizeObserver = window.ResizeObserver;
      const scrollNode = document.createElement("section");
      const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

      class ResizeObserverMock {
        observe() {}
        disconnect() {}
      }

      window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

      try {
        const tailFirstPageSelector =
          '.virtualized-message-page[data-page-key="0:8:message-581:message-588"]';
        document.body.append(scrollNode);
        const { container } = renderSessionPanelWithDefaults({
          activeSession: makeSession("active-session", {
            status: "idle",
            messages: makeTextMessages(600),
          }),
          scrollContainerRef: { current: scrollNode },
        });
        expect(container.querySelector(tailFirstPageSelector)).not.toBeNull();

        const keydown = new KeyboardEvent("keydown", { bubbles: true, key });
        Object.defineProperty(keydown, "composedPath", {
          value: () => [scrollNode, document.body, document, window],
        });
        act(() => {
          scrollNode.dispatchEvent(keydown);
        });
        await act(async () => {
          await Promise.resolve();
        });

        expect(container.querySelector(tailFirstPageSelector)).toBeNull();
      } finally {
        window.ResizeObserver = OriginalResizeObserver;
        scrollNodeMocks.cleanup();
        scrollNode.remove();
      }
    },
  );

  it("hydrates a long-session tail only after a pull-down touch gesture", async () => {
    vi.useFakeTimers();
    const OriginalResizeObserver = window.ResizeObserver;
    const OriginalTouchEvent = window.TouchEvent;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    class TouchEventMock extends Event {
      readonly changedTouches: Touch[];
      readonly touches: Touch[];

      constructor(type: string, init: TouchEventInit = {}) {
        super(type, { bubbles: init.bubbles ?? true, cancelable: init.cancelable });
        this.changedTouches = init.changedTouches ?? [];
        this.touches = init.touches ?? [];
      }
    }

    function dispatchTouch(type: string, clientY: number | null) {
      scrollNode.dispatchEvent(
        new TouchEvent(type, {
          bubbles: true,
          touches: clientY === null ? [] : [{ clientY } as Touch],
          changedTouches: clientY === null ? [] : [{ clientY } as Touch],
        }),
      );
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.TouchEvent = TouchEventMock as unknown as typeof TouchEvent;

    try {
      const tailFirstPageSelector =
        '.virtualized-message-page[data-page-key="0:8:message-581:message-588"]';
      document.body.append(scrollNode);
      const { container } = renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages: makeTextMessages(600),
        }),
        scrollContainerRef: { current: scrollNode },
      });
      expect(container.querySelector(tailFirstPageSelector)).not.toBeNull();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(50);
        dispatchTouch("touchstart", 100);
        dispatchTouch("touchmove", 50);
        dispatchTouch("touchend", null);
        dispatchTouch("touchmove", 200);
        dispatchTouch("touchstart", 100);
        dispatchTouch("touchcancel", null);
        dispatchTouch("touchmove", 200);
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });
      expect(container.querySelector(tailFirstPageSelector)).not.toBeNull();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      act(() => {
        scrollNodeMocks.setScrollTop(50);
        dispatchTouch("touchstart", 100);
        dispatchTouch("touchmove", 200);
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(500);
      });

      expect(container.querySelector(tailFirstPageSelector)).toBeNull();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.TouchEvent = OriginalTouchEvent;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
      vi.useRealTimers();
    }
  });

  it("tears down demand-hydration listeners after the first full render", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const scrollNode = document.createElement("section");
    const scrollNodeMocks = installLongTranscriptScrollNodeMocks(scrollNode);
    const scrollNodeDemandEvents = [
      "scroll",
      "wheel",
      "mousedown",
      "touchstart",
      "touchmove",
      "touchend",
      "touchcancel",
    ] as const;
    const addCounts = new Map<string, number>();
    const removeCounts = new Map<string, number>();
    let documentKeydownAdds = 0;
    let documentKeydownRemoves = 0;
    const originalAdd = scrollNode.addEventListener.bind(scrollNode);
    const originalRemove = scrollNode.removeEventListener.bind(scrollNode);
    const originalDocumentAdd = document.addEventListener.bind(document);
    const originalDocumentRemove = document.removeEventListener.bind(document);

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    scrollNode.addEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (
        scrollNodeDemandEvents.includes(
          type as (typeof scrollNodeDemandEvents)[number],
        )
      ) {
        addCounts.set(type, (addCounts.get(type) ?? 0) + 1);
      }
      return originalAdd(type, listener, options);
    }) as typeof scrollNode.addEventListener;
    scrollNode.removeEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: EventListenerOptions | boolean,
    ) => {
      if (
        scrollNodeDemandEvents.includes(
          type as (typeof scrollNodeDemandEvents)[number],
        )
      ) {
        removeCounts.set(type, (removeCounts.get(type) ?? 0) + 1);
      }
      return originalRemove(type, listener, options);
    }) as typeof scrollNode.removeEventListener;
    document.addEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (type === "keydown") {
        documentKeydownAdds += 1;
      }
      return originalDocumentAdd(type, listener, options);
    }) as typeof document.addEventListener;
    document.removeEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: EventListenerOptions | boolean,
    ) => {
      if (type === "keydown") {
        documentKeydownRemoves += 1;
      }
      return originalDocumentRemove(type, listener, options);
    }) as typeof document.removeEventListener;
    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const tailFirstPageSelector =
        '.virtualized-message-page[data-page-key="0:8:message-581:message-588"]';
      document.body.append(scrollNode);
      const { container } = renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "idle",
          messages: makeTextMessages(600),
        }),
        scrollContainerRef: { current: scrollNode },
      });
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });

      const baselineRemoveCounts = new Map(removeCounts);
      const baselineDocumentKeydownRemoves = documentKeydownRemoves;
      scrollNodeDemandEvents.forEach((eventName) => {
        expect(addCounts.get(eventName)).toBeGreaterThan(0);
      });
      expect(documentKeydownAdds).toBeGreaterThan(0);
      expect(container.querySelector(tailFirstPageSelector)).not.toBeNull();

      act(() => {
        scrollNodeMocks.setScrollTop(50);
        fireEvent.wheel(scrollNode, { deltaY: -120 });
      });
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(container.querySelector(tailFirstPageSelector)).toBeNull();
      scrollNodeDemandEvents.forEach((eventName) => {
        expect(removeCounts.get(eventName), eventName).toBeGreaterThanOrEqual(
          (baselineRemoveCounts.get(eventName) ?? 0) + 1,
        );
      });
      expect(documentKeydownRemoves).toBeGreaterThanOrEqual(
        baselineDocumentKeydownRemoves + 1,
      );

      const postHydrationRemoveCounts = new Map(removeCounts);
      const postHydrationDocumentKeydownRemoves = documentKeydownRemoves;
      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: -120 });
      });
      await act(async () => {
        await Promise.resolve();
      });

      scrollNodeDemandEvents.forEach((eventName) => {
        expect(removeCounts.get(eventName)).toBe(
          postHydrationRemoveCounts.get(eventName),
        );
      });
      expect(documentKeydownRemoves).toBe(postHydrationDocumentKeydownRemoves);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      document.addEventListener = originalDocumentAdd;
      document.removeEventListener = originalDocumentRemove;
      scrollNodeMocks.cleanup();
      scrollNode.remove();
    }
  });

  it.each([
    ["Claude", "new prompt", "Claude is working — Waiting for output"],
    ["Cursor", "new prompt", "Cursor is working — Waiting for output"],
    ["Gemini", "new prompt", "Gemini is working — Waiting for output"],
    ["Codex", "/review-local", "Codex is working — Executing a command"],
  ] as const)(
    "builds the conversation overview live-turn sample for %s",
    (agent, waitingIndicatorPrompt, expectedSample) => {
      expect(
        buildConversationOverviewTailItems({
          agent,
          sessionId: "active-session",
          showWaitingIndicator: true,
          waitingIndicatorPrompt,
        })[0],
      ).toEqual(expect.objectContaining({ textSample: expectedSample }));
    },
  );

  it("navigates the virtualized transcript from conversation overview items", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const scrollNode = document.createElement("section");
    let scrollTop = 0;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 20_000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      callback(performance.now());
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;

    try {
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "active",
          messages: makeTextMessages(90),
        }),
        scrollContainerRef: { current: scrollNode },
        showWaitingIndicator: true,
        waitingIndicatorPrompt: "run the build",
      });

      await waitFor(() => {
        expect(screen.getByLabelText("Conversation overview")).toBeInTheDocument();
      });

      act(() => {
        fireEvent.click(
          screen.getByRole("button", { name: /^Assistant response 80:/ }),
        );
      });

      expect(scrollTop).toBeGreaterThan(0);
      const scrollAfterMessageJump = scrollTop;

      act(() => {
        fireEvent.click(
          screen.getByRole("button", {
            name: /^Live turn 91: Codex is working — Waiting for output/,
          }),
        );
      });

      expect(scrollTop).toBe(19_500);
      expect(scrollWrites.some((write) => write > scrollAfterMessageJump)).toBe(
        true,
      );
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("refreshes the conversation overview viewport and max height from scroll and resize events", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const scrollNode = document.createElement("section");
    let clientHeight = 500;
    let scrollTop = 0;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 20_000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      callback(performance.now());
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;

    try {
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "active",
          messages: makeTextMessages(90),
        }),
        scrollContainerRef: { current: scrollNode },
      });

      const rail = await screen.findByLabelText("Conversation overview");
      const viewport = screen.getByTestId("conversation-overview-viewport");

      await waitFor(() => {
        expect(rail).toHaveStyle({ height: "476px" });
      });
      const initialViewportTop = viewport.style.top;

      act(() => {
        scrollTop = 1_500;
        fireEvent.scroll(scrollNode);
      });

      await waitFor(() => {
        expect(viewport.style.top).not.toBe(initialViewportTop);
      });

      act(() => {
        clientHeight = 700;
        window.dispatchEvent(new Event("resize"));
      });

      await waitFor(() => {
        expect(rail).toHaveStyle({ height: "676px" });
      });
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("does not carry deferred transcript cards across session switches", async () => {
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
    const sessionB = makeSession("session-b", {
      messages: [
        {
          author: "assistant",
          id: "message-b",
          text: "Session B transcript",
          timestamp: "10:00",
          type: "text",
        },
      ],
    });
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
          <article className="message-card">{message.id}</article>
        )}
        renderPromptSettings={() => null}
      />
    );

    const { rerender } = render(renderPanel(sessionA.id));
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.getByText("message-a")).toBeInTheDocument();

    await act(async () => {
      rerender(renderPanel(sessionB.id));
      await Promise.resolve();
    });

    expect(screen.queryByText("message-a")).not.toBeInTheDocument();
    expect(screen.getByText("message-b")).toBeInTheDocument();
  });

  it("keeps long session find virtualized while typing a query", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const scrollNode = document.createElement("section");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 500,
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      queueMicrotask(() => callback(0));
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;

    try {
      const messages = makeTextMessages(180);
      const activeSession = makeSession("session-a", { messages });
      const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));

      const { container } = renderSessionPanelWithDefaults({
        activeSession,
        scrollContainerRef: {
          current: scrollNode,
        } as RefObject<HTMLElement | null>,
        conversationSearchQuery: "Message",
        conversationSearchMatchedItemKeys: matchedKeys,
        conversationSearchActiveItemKey: "message:message-150",
      });

      await waitFor(() => {
        expect(screen.getByText("message-150")).toBeInTheDocument();
      });

      expect(container.querySelectorAll(".message-card").length).toBeLessThan(80);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("renders the bottom window immediately after a virtualized bottom-pin scroll write", () => {
    // Regression guard for the "half screen, then blank" failure: the
    // virtualizer rendered rows for the old viewport, a layout effect wrote
    // `scrollTop` to the bottom, and the browser could paint before a native
    // scroll event caused the virtualizer to render rows for the new viewport.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const messages = makeTextMessages(120);
    const estimatedLayout = buildVirtualizedMessageLayout(
      messages.map((message) => estimateConversationMessageHeight(message)),
    );
    const clientHeight = 500;
    let scrollTop = 0;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => estimatedLayout.totalHeight,
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
    // Keep first measurements unresolved so the only state update that can
    // move the rendered window is the immediate viewport sync after the
    // programmatic bottom scroll.
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        const element = this as HTMLElement;
        const height = element.classList.contains("virtualized-message-slot")
          ? 0
          : clientHeight;
        return {
          bottom: height,
          height,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      expect(scrollTop).toBe(estimatedLayout.totalHeight - clientHeight);
      expect(screen.queryByText("message-120")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("renders the new window immediately after a parent-owned scroll write", async () => {
    // Regression guard for flicker during pane-level scroll writes. The
    // parent pane owns wheel scrolling, saved-position restores, and
    // scroll-to-boundary jumps; those writes move the same scroll container
    // the virtualizer observes. If the virtualizer waits for a later native
    // scroll event, it can paint rows for the previous viewport for one
    // frame, which appears as a blank or half-filled transcript.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const messages = makeTextMessages(120);
    const estimatedLayout = buildVirtualizedMessageLayout(
      messages.map((message) => estimateConversationMessageHeight(message)),
    );
    const clientHeight = 500;
    let scrollTop = 0;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => estimatedLayout.totalHeight,
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
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        const element = this as HTMLElement;
        const height = element.classList.contains("virtualized-message-slot")
          ? 0
          : clientHeight;
        return {
          bottom: height,
          height,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      expect(scrollTop).toBe(estimatedLayout.totalHeight - clientHeight);
      expect(screen.queryByText("message-120")).toBeInTheDocument();

      await act(async () => {
        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode);
        await Promise.resolve();
      });

      expect(screen.queryByText("message-1")).toBeInTheDocument();
      expect(screen.queryByText("message-120")).not.toBeInTheDocument();

      await act(async () => {
        scrollTop = estimatedLayout.totalHeight - clientHeight;
        notifyMessageStackScrollWrite(scrollNode, {
          scrollKind: "bottom_boundary",
        });
        await Promise.resolve();
      });

      expect(screen.queryByText("message-120")).toBeInTheDocument();
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("does not flush virtualizer range updates from layout-effect scroll writes", async () => {
    // Regression guard for React's "flushSync was called from inside a
    // lifecycle method" warning. SessionPaneView restores saved positions from
    // a layout effect and immediately dispatches the message-stack scroll-write
    // event; the virtualizer must reconcile that programmatic write without
    // routing through its user-scroll flush path.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const messages = makeTextMessages(120);
    const estimatedLayout = buildVirtualizedMessageLayout(
      messages.map((message) => estimateConversationMessageHeight(message)),
    );
    const clientHeight = 500;
    let scrollTop = 0;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => estimatedLayout.totalHeight,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });

    function LayoutEffectScrollWriter({ enabled }: { enabled: boolean }) {
      useLayoutEffect(() => {
        if (!enabled) {
          return;
        }

        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode, {
          scrollKind: "seek",
        });
      }, [enabled]);

      return null;
    }

    function Harness({ restoreToTop }: { restoreToTop: boolean }) {
      return (
        <>
          <VirtualizedConversationMessageList
            isActive
            renderMessageCard={(message) => (
              <article className="message-card">{message.id}</article>
            )}
            sessionId="session-a"
            messages={messages}
            scrollContainerRef={{
              current: scrollNode,
            } as RefObject<HTMLElement | null>}
            onApprovalDecision={() => {}}
            onUserInputSubmit={() => {}}
            onMcpElicitationSubmit={() => {}}
            onCodexAppRequestSubmit={() => {}}
          />
          <LayoutEffectScrollWriter enabled={restoreToTop} />
        </>
      );
    }

    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        const element = this as HTMLElement;
        const height = element.classList.contains("virtualized-message-slot")
          ? 0
          : clientHeight;
        return {
          bottom: height,
          height,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    const consoleErrorSpy = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    try {
      const { rerender } = render(<Harness restoreToTop={false} />);
      expect(scrollTop).toBe(estimatedLayout.totalHeight - clientHeight);
      expect(screen.queryByText("message-120")).toBeInTheDocument();

      await act(async () => {
        rerender(<Harness restoreToTop />);
        await Promise.resolve();
      });

      expect(screen.queryByText("message-1")).toBeInTheDocument();
      const consoleErrorMessages = consoleErrorSpy.mock.calls.map((call) =>
        call.map((part) => String(part)).join(" "),
      );
      expect(
        consoleErrorMessages.filter(
          (message) =>
            message.includes("flushSync was called from inside a lifecycle method") ||
            message.includes("not wrapped in act") ||
            message.startsWith("Warning:"),
        ),
      ).toEqual([]);
    } finally {
      consoleErrorSpy.mockRestore();
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("does not snap back to the bottom after a programmatic boundary jump to the top", async () => {
    // Regression guard for the "Ctrl+PgUp takes 4 presses" symptom.
    // When the list is pinned to the bottom and a parent-owned
    // scroll write (Ctrl+PgUp / Ctrl+Home / "scroll to top" button)
    // moves scrollTop to 0, the ResizeObserver callbacks for the
    // newly-exposed top rows fire on subsequent microtasks. If
    // `shouldKeepBottomAfterLayoutRef` is still `true` when those
    // microtasks run, `handleHeightChange` writes scrollTop back
    // to the bottom — undoing the boundary jump. The native
    // `scroll` event that would clear the flag fires later than
    // the ResizeObserver microtasks, so the race favors the
    // measurement re-pin and the user has to press the key
    // multiple times (or nudge the scroll manually) before the
    // jump sticks.
    //
    // The fix clears `shouldKeepBottomAfterLayoutRef` inside the
    // synchronous custom-event dispatch from
    // `notifyMessageStackScrollWrite` — specifically when the new
    // scrollTop is NOT near the bottom. This test exercises the
    // full sequence: initial bottom pin, programmatic jump to
    // top, subsequent measurement callback, and asserts the
    // measurement did NOT re-pin to the bottom.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 80;
    let scrollTop = 0;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const messages = makeTextMessages(120);
    const estimatedLayout = buildVirtualizedMessageLayout(
      messages.map((message) => estimateConversationMessageHeight(message)),
    );
    const clientHeight = 500;
    const totalHeight = estimatedLayout.totalHeight;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => totalHeight,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        const element = this as HTMLElement;
        const height = element.classList.contains("virtualized-message-slot")
          ? measuredSlotHeight
          : clientHeight;
        return {
          bottom: height,
          height,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // Initial mount lands at the bottom via the re-pin effect.
      // `shouldKeepBottomAfterLayoutRef` is now `true`.
      expect(scrollTop).toBe(totalHeight - clientHeight);

      // Simulate Ctrl+PgUp: parent sets scrollTop=0, then
      // notifies the virtualizer via the custom event. The
      // listener must clear the bottom-pin intent synchronously
      // inside this dispatch, BEFORE the upcoming `flushSync`
      // re-render mounts new top rows whose measurements would
      // otherwise see the still-set flag and snap back to the
      // bottom.
      scrollWrites.length = 0;
      act(() => {
        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode);
      });

      // The re-rendered window now shows the top rows — their
      // slots mount and the ResizeObserver callback is queued.
      // Simulate the measurement microtask firing: if the bug
      // were still present, `handleHeightChange`'s bottom-pin
      // branch would fire and push a scrollTop write targeted
      // at the bottom.
      const topSlot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        expect(resizeCallbacks.has(candidate!)).toBe(true);
        return candidate!;
      });

      measuredSlotHeight = 120;
      await act(async () => {
        resizeCallbacks
          .get(topSlot)
          ?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      // Critical invariant: no scrollTop write to the bottom
      // fired from `handleHeightChange`. The only acceptable
      // writes here are small height-adjustment writes via the
      // anchor-preserving branch. A write that targets
      // `totalHeight - clientHeight` (or anywhere near the
      // bottom) would indicate the bottom-pin intent was NOT
      // cleared by the programmatic-jump dispatch.
      const bottomTarget = totalHeight - clientHeight;
      const wroteToBottom = scrollWrites.some(
        (value) => Math.abs(value - bottomTarget) < clientHeight,
      );
      expect(wroteToBottom).toBe(false);
      // And scrollTop itself stays at or near the top (anchor-
      // preserving adjustments can move it by a small delta but
      // must not land near the bottom).
      expect(scrollTop).toBeLessThan(clientHeight * 2);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("keeps the virtualized bottom pinned after a programmatic bottom scroll from a detached viewport", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 80;
    let scrollTop = 0;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const messages = makeTextMessages(120);
    const estimatedLayout = buildVirtualizedMessageLayout(
      messages.map((message) => estimateConversationMessageHeight(message)),
    );
    const clientHeight = 500;
    let reportedTotalHeight = estimatedLayout.totalHeight;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => reportedTotalHeight,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      queueMicrotask(() => callback(0));
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        const element = this as HTMLElement;
        const height = element.classList.contains("virtualized-message-slot")
          ? measuredSlotHeight
          : clientHeight;
        return {
          bottom: height,
          height,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      await waitFor(() => {
        expect(scrollTop).toBe(reportedTotalHeight - clientHeight);
      });

      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: -120 });
        scrollTop = 120;
        fireEvent.scroll(scrollNode);
      });
      expect(scrollTop).toBe(120);

      act(() => {
        scrollTop = reportedTotalHeight - clientHeight;
        notifyMessageStackScrollWrite(scrollNode);
      });
      expect(scrollTop).toBe(reportedTotalHeight - clientHeight);

      const mountedSlot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        expect(resizeCallbacks.has(candidate!)).toBe(true);
        return candidate!;
      });

      reportedTotalHeight += 600;
      measuredSlotHeight = 140;
      scrollWrites.length = 0;
      await act(async () => {
        resizeCallbacks
          .get(mountedSlot)
          ?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
        await new Promise((resolve) => window.setTimeout(resolve, 0));
        await Promise.resolve();
      });

      expect(scrollTop).toBe(reportedTotalHeight - clientHeight);
      expect(scrollWrites).toContain(reportedTotalHeight - clientHeight);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("resyncs the rendered window after a silent startup scrollTop change", () => {
    // Startup/restart can leave the browser's scrollTop clamped or
    // restored after the virtualizer's first render without emitting a
    // native scroll event. The short startup resync loop must notice the
    // real scrollTop and render the matching rows even when no user input
    // occurs.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;
    const frameCallbacks: FrameRequestCallback[] = [];

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const messages = makeTextMessages(120);
    const estimatedLayout = buildVirtualizedMessageLayout(
      messages.map((message) => estimateConversationMessageHeight(message)),
    );
    const clientHeight = 500;
    let scrollTop = 0;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => clientHeight,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => estimatedLayout.totalHeight,
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
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      frameCallbacks.push(callback);
      return frameCallbacks.length;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        const element = this as HTMLElement;
        const height = element.classList.contains("virtualized-message-slot")
          ? 0
          : clientHeight;
        return {
          bottom: height,
          height,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      expect(scrollTop).toBe(estimatedLayout.totalHeight - clientHeight);
      expect(screen.queryByText("message-120")).toBeInTheDocument();

      act(() => {
        scrollTop = 0;
        frameCallbacks.shift()?.(0);
      });

      expect(screen.queryByText("message-1")).toBeInTheDocument();
      expect(screen.queryByText("message-120")).not.toBeInTheDocument();
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("keeps the bottom pin across successive virtualized height commits", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 180;
    let scrollTop = 400;
    let scrollHeight = 500;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
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
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? measuredSlotHeight
        : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(3)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      scrollWrites.length = 0;
      scrollTop = 400;
      measuredSlotHeight = 260;
      scrollHeight = 580;
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toContain(480);

      const secondSlot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        expect(resizeCallbacks.has(candidate!)).toBe(true);
        return candidate!;
      });
      scrollWrites.length = 0;
      scrollTop = 480;
      measuredSlotHeight = 340;
      scrollHeight = 660;
      await act(async () => {
        resizeCallbacks.get(secondSlot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toContain(560);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("uses live measured tops when multiple height changes land before the next layout commit", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(4);
    const measuredHeights = new Map<string, number>([
      ["message-1", 100],
      ["message-2", 40],
      ["message-3", 100],
      ["message-4", 100],
    ]);
    let scrollTop = 0;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const getTotalHeight = () =>
      buildVirtualizedMessageLayout(
        messages.map((message) => measuredHeights.get(message.id) ?? 100),
      ).totalHeight;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => getTotalHeight(),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const messageId = element.textContent?.match(/message-\d+/)?.[0];
      const height =
        element.classList.contains("virtualized-message-slot") && messageId
          ? (measuredHeights.get(messageId) ?? 100)
          : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      act(() => {
        scrollTop = 160;
        notifyMessageStackScrollWrite(scrollNode);
      });

      const slots = await waitFor(() => {
        const candidates = Array.from(
          container.querySelectorAll(".virtualized-message-slot"),
        );
        expect(candidates).toHaveLength(4);
        expect(resizeCallbacks.has(candidates[0]!)).toBe(true);
        expect(resizeCallbacks.has(candidates[1]!)).toBe(true);
        return candidates;
      });

      scrollWrites.length = 0;
      measuredHeights.set("message-1", 220);
      measuredHeights.set("message-2", 120);
      await act(async () => {
        resizeCallbacks
          .get(slots[0]!)
          ?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        resizeCallbacks
          .get(slots[1]!)
          ?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toHaveLength(0);
      expect(scrollTop).toBe(160);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("skips the bottom pin write only while the user is actively scrolling", async () => {
    // Regression guard for the "near-bottom streaming re-pins the
    // viewport and fights user scroll-up" symptom. Inside
    // `VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS` (200 ms) of a `wheel` /
    // `touchmove` / `keydown` event on the scroll container, a
    // streaming-driven height measurement must not snap `scrollTop`
    // back to the bottom — even when the user is still within the
    // 72 px near-bottom band. After the cooldown expires (or when no
    // user input has occurred), pinning resumes.
    //
    // Two code paths must honour the cooldown: `handleHeightChange`'s
    // `shouldKeepBottom` branch (which would `node.scrollTop = target`
    // inline), AND the separate re-pin `useLayoutEffect` keyed on
    // `layout.totalHeight` (which fires on the follow-up commit from
    // `setLayoutVersion` and would otherwise re-pin a frame later).
    // The harness uses a live `scrollHeight` derived from
    // `measuredSlotHeight` so both paths see the post-measurement
    // geometry — reverting either gate reproduces a write we can
    // assert against.
    //
    // The test has two phases to prove the wheel event is
    // load-bearing:
    //   1. No user input → measurement → scrollWrites must CONTAIN
    //      the pin target. If the wheel listener were broken but the
    //      ref still initialised to `0`, `performance.now() - 0` in a
    //      fast-start test could be below 200 ms and spuriously
    //      suppress this write. The production code now initialises
    //      `lastUserScrollInputTimeRef` to `Number.NEGATIVE_INFINITY`
    //      so this phase always sees the pin fire.
    //   2. Fire wheel → measurement → scrollWrites must be empty.
    //      This pins the other direction: the cooldown does suppress
    //      the write when a real direct-scroll input occurred.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 180;
    let scrollTop = 400;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      // Live `scrollHeight` derived from the current measured slot
      // height. Matches what real DOM exposes once React commits the
      // wrapper's `style={{ height: layout.totalHeight }}`: the
      // measurement updates `messageHeightsRef`, `setLayoutVersion`
      // fires, `layout.totalHeight` recomputes, the wrapper re-renders
      // with the new height, and subsequent `scrollHeight` reads
      // reflect the new geometry. Encoding that dependency directly
      // here stops the test from accidentally passing against a stale
      // pre-measurement value.
      get: () => 500 + (measuredSlotHeight - 180),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? measuredSlotHeight
        : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(3)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      // Phase 1 — negative control. WITHOUT any wheel/touch/key
      // input, trigger a streaming-style measurement. `scrollTop` is
      // at 400 (clientHeight 100 / pre-measurement scrollHeight 500
      // → gap 0 → `isScrollContainerNearBottom` is true, pin
      // heuristic armed). The measurement grows the card from 180 →
      // 260 px; the live `scrollHeight` getter then reports 580.
      // With no direct-scroll input the cooldown must NOT fire, so
      // `scrollTop` must be written to the pin target (580 − 100 =
      // 480). This proves the measurement path is live inside the
      // test harness — a false pass here would indicate the
      // cooldown is spuriously active on mount.
      scrollWrites.length = 0;
      scrollTop = 400;
      measuredSlotHeight = 260;
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toContain(480);

      // Phase 2 — positive assertion. Simulate the user wheeling up
      // inside the scroll container. The component's `syncViewport`
      // effect attaches a `wheel` listener on `scrollNode` that
      // timestamps `lastUserScrollInputTimeRef`.
      fireEvent.wheel(scrollNode, { deltaY: -50 });

      // Trigger another streaming measurement. The card grows from
      // 260 → 340 px; the live `scrollHeight` getter now reports
      // 660. `scrollTop` is still near the bottom (480 from phase
      // 1) so `shouldKeepBottom` stays true. Without the cooldown
      // on EITHER the inline `handleHeightChange` write path OR
      // the follow-up re-pin `useLayoutEffect`, `scrollTop` would
      // be written to 660 − 100 = 560. The cooldown must suppress
      // both paths.
      scrollWrites.length = 0;
      measuredSlotHeight = 340;
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toEqual([]);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("treats native scroll events as user input when they do not match a programmatic write", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 180;
    let scrollTop = 400;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 500 + (measuredSlotHeight - 180),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? measuredSlotHeight
        : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(3)}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      scrollWrites.length = 0;
      act(() => {
        scrollTop = 360;
        fireEvent.scroll(scrollNode);
      });

      measuredSlotHeight = 340;
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toEqual([]);
      expect(scrollTop).toBe(360);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("does not let bottom-follow recapture later inertial native scroll ticks", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRectDescriptor = Object.getOwnPropertyDescriptor(
      Element.prototype,
      "getBoundingClientRect",
    );
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 180;
    let scrollTop = 400;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 500 + (measuredSlotHeight - 180),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? measuredSlotHeight
        : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(3)}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      act(() => {
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "bottom_follow" });
      });

      act(() => {
        scrollTop = 350;
        fireEvent.scroll(scrollNode);
      });
      act(() => {
        scrollTop = 360;
        fireEvent.scroll(scrollNode);
      });

      scrollWrites.length = 0;
      measuredSlotHeight = 340;
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toEqual([]);
      expect(scrollTop).toBe(360);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      if (originalGetBoundingClientRectDescriptor) {
        Object.defineProperty(
          Element.prototype,
          "getBoundingClientRect",
          originalGetBoundingClientRectDescriptor,
        );
      } else {
        Reflect.deleteProperty(Element.prototype, "getBoundingClientRect");
      }
    }
  });

  it("does not give back upward wheel progress when a row above the viewport grows", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(4);
    const measuredHeights = new Map<string, number>([
      ["message-1", 100],
      ["message-2", 100],
      ["message-3", 100],
      ["message-4", 100],
    ]);
    let scrollTop = 0;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const getTotalHeight = () =>
      buildVirtualizedMessageLayout(
        messages.map((message) => measuredHeights.get(message.id) ?? 100),
      ).totalHeight;

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => getTotalHeight(),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const messageId = element.textContent?.match(/message-\d+/)?.[0];
      const height =
        element.classList.contains("virtualized-message-slot") && messageId
          ? (measuredHeights.get(messageId) ?? 100)
          : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slots = await waitFor(() => {
        const candidates = Array.from(
          container.querySelectorAll(".virtualized-message-slot"),
        );
        expect(candidates).toHaveLength(4);
        return candidates;
      });

      // Establish an upward native scroll gesture: the viewport moves from 320
      // to 260, placing message-2 safely above the viewport. If that row grows
      // during the active-scroll cooldown, the virtualizer must not push
      // `scrollTop` back down and cancel the user's upward wheel progress.
      act(() => {
        scrollTop = 320;
        fireEvent.scroll(scrollNode);
      });
      scrollWrites.length = 0;
      act(() => {
        scrollTop = 260;
        fireEvent.scroll(scrollNode);
      });
      scrollWrites.length = 0;

      measuredHeights.set("message-2", 220);
      await act(async () => {
        resizeCallbacks
          .get(slots[1]!)
          ?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toEqual([]);
      expect(scrollTop).toBe(260);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("does not introduce an extra scroll adjustment when deferred virtual layout catches up after scroll idle", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(40);
    const measuredHeights = new Map<string, number>(
      messages.map((message) => [message.id, 100]),
    );
    let scrollTop = 0;
    const scrollWrites: number[] = [];

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const getMessageHeight = (messageId: string) =>
      measuredHeights.get(messageId) ?? 100;

    const getVirtualNodeHeight = (node: HTMLElement): number => {
      if (node.classList.contains("virtualized-message-spacer")) {
        return Number.parseFloat(node.style.height) || 0;
      }
      if (node.classList.contains("virtualized-message-slot")) {
        const messageId = node.dataset.messageId;
        return messageId ? getMessageHeight(messageId) : 0;
      }

      const children = Array.from(node.children).filter(
        (child): child is HTMLElement => child instanceof HTMLElement,
      );
      let total = 0;
      children.forEach((child, index) => {
        total += getVirtualNodeHeight(child);
        if (
          node.classList.contains("virtualized-message-range") &&
          index < children.length - 1
        ) {
          total += VIRTUALIZED_MESSAGE_GAP_PX;
        }
      });
      return total;
    };

    const getVirtualContentTop = (root: HTMLElement, target: HTMLElement): number => {
      const walk = (node: HTMLElement): number | null => {
        if (node === target) {
          return 0;
        }

        const children = Array.from(node.children).filter(
          (child): child is HTMLElement => child instanceof HTMLElement,
        );
        let offset = 0;
        for (let index = 0; index < children.length; index += 1) {
          const child = children[index]!;
          const childOffset = walk(child);
          if (childOffset !== null) {
            return offset + childOffset;
          }
          offset += getVirtualNodeHeight(child);
          if (
            node.classList.contains("virtualized-message-range") &&
            index < children.length - 1
          ) {
            offset += VIRTUALIZED_MESSAGE_GAP_PX;
          }
        }

        return null;
      };

      const top = walk(root);
      if (top === null) {
        throw new Error("target slot not found in virtualized message list");
      }
      return top;
    };

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () =>
        buildVirtualizedMessageLayout(
          messages.map((message) => getMessageHeight(message.id)),
        ).totalHeight,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      if (element === scrollNode) {
        return {
          bottom: 100,
          height: 100,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }

      if (element.classList.contains("virtualized-message-slot")) {
        const root = element.closest(".virtualized-message-list") as HTMLElement | null;
        const messageId = element.dataset.messageId;
        if (!root || !messageId) {
          throw new Error("slot missing virtualized root or message id");
        }
        const top = getVirtualContentTop(root, element) - scrollTop;
        const height = getMessageHeight(messageId);
        return {
          bottom: top + height,
          height,
          left: 0,
          right: 100,
          top,
          width: 100,
          x: 0,
          y: top,
          toJSON: () => ({}),
        } as DOMRect;
      }

      return {
        bottom: 0,
        height: 0,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    vi.useFakeTimers();

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={messages}
          scrollContainerRef={{
            current: scrollNode,
          } as RefObject<HTMLElement | null>}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );
      expect(container.querySelectorAll(".virtualized-message-slot").length).toBeGreaterThan(0);

      const initialLayout = buildVirtualizedMessageLayout(
        messages.map((message) => getMessageHeight(message.id)),
      );
      const middleScrollTop = (initialLayout.tops[18] ?? 0) + 10;
      act(() => {
        scrollTop = middleScrollTop;
        fireEvent.wheel(scrollNode, { deltaY: -120 });
        fireEvent.scroll(scrollNode);
      });

      const slotsBefore = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-slot"),
      );
      expect(slotsBefore.length).toBeGreaterThan(4);
      const firstVisibleSlotIndex = slotsBefore.findIndex((slot) => {
        const rect = slot.getBoundingClientRect();
        return rect.bottom > 0 && rect.top < 100;
      });
      expect(firstVisibleSlotIndex).toBeGreaterThan(0);
      const firstVisibleSlot = slotsBefore[firstVisibleSlotIndex]!;
      const anchorMessageId = firstVisibleSlot.dataset.messageId!;
      const measuredAboveViewportSlot = slotsBefore[0]!;

      scrollWrites.length = 0;
      measuredHeights.set(measuredAboveViewportSlot.dataset.messageId!, 320);
      await act(async () => {
        resizeCallbacks
          .get(measuredAboveViewportSlot)
          ?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toEqual([]);
      expect(scrollTop).toBe(middleScrollTop);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(200);
        await Promise.resolve();
      });

      expect(
        container.querySelectorAll(".virtualized-message-page").length,
      ).toBeGreaterThan(0);
      const lastScrollWrite =
        scrollWrites.length > 0 ? scrollWrites[scrollWrites.length - 1] : middleScrollTop;
      expect(lastScrollWrite).toBe(middleScrollTop);
      expect(scrollTop).toBe(middleScrollTop);
    } finally {
      vi.useRealTimers();
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });
});
