// Owns scroll-heavy AgentSessionPanel virtualization regression tests.
// Does not own footer/composer behavior or non-scroll virtualization coverage.
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

describe("AgentSessionPanel virtualization scroll behavior", () => {
  it("mounts more pages below when compact command pages shrink during scroll cooldown", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRectDescriptor = Object.getOwnPropertyDescriptor(
      Element.prototype,
      "getBoundingClientRect",
    );
    const performanceNowSpy = vi.spyOn(performance, "now").mockReturnValue(1000);
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeCommandMessages(80);
    const clientHeight = 500;
    const tallMeasuredSlotHeight = 220;
    const compactMeasuredSlotHeight = 40;
    let useCompactMeasurements = false;
    let exposeVirtualScrollHeight = false;
    let scrollTop = 0;

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
      disconnect() {}
    }

    const getMessageHeight = () =>
      useCompactMeasurements ? compactMeasuredSlotHeight : tallMeasuredSlotHeight;

    const getVirtualNodeHeight = (node: HTMLElement): number => {
      if (node.classList.contains("virtualized-message-spacer")) {
        return Number.parseFloat(node.style.height) || 0;
      }
      if (node.classList.contains("virtualized-message-page-gap")) {
        return Number.parseFloat(node.style.height) || 0;
      }
      if (node.classList.contains("virtualized-message-slot")) {
        return getMessageHeight();
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
        }

        return null;
      };

      const top = walk(root);
      if (top === null) {
        throw new Error("target node not found in virtualized message list");
      }
      return top;
    };

    const scrollNode = document.createElement("div");
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
      get: () => {
        if (!exposeVirtualScrollHeight) {
          return clientHeight;
        }
        const list = document.querySelector(".virtualized-message-list");
        return list instanceof HTMLElement
          ? getVirtualNodeHeight(list)
          : buildVirtualizedMessageLayout(messages.map(() => tallMeasuredSlotHeight)).totalHeight;
      },
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
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
          bottom: clientHeight,
          height: clientHeight,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }

      const root = element.closest(".virtualized-message-list") as HTMLElement | null;
      if (root) {
        const top = getVirtualContentTop(root, element) - scrollTop;
        const height = getVirtualNodeHeight(element);
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
        expect(container.querySelectorAll(".virtualized-message-slot").length).toBeGreaterThan(0);
      });

      exposeVirtualScrollHeight = true;
      act(() => {
        scrollTop = 3600;
        fireEvent.wheel(scrollNode, { deltaY: 3600 });
        fireEvent.scroll(scrollNode);
      });

      await waitFor(() => {
        expect(screen.getByText("command-48")).toBeInTheDocument();
      });
      expect(screen.queryByText("command-80")).not.toBeInTheDocument();

      useCompactMeasurements = true;
      await act(async () => {
        new Set(resizeCallbacks.values()).forEach((callback) => {
          callback([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        });
        await Promise.resolve();
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(screen.getByText("command-80")).toBeInTheDocument();
      });

      const renderedPages = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      );
      const lastPage = renderedPages[renderedPages.length - 1];
      expect(lastPage?.getBoundingClientRect().bottom).toBeGreaterThanOrEqual(
        clientHeight,
      );
    } finally {
      performanceNowSpy.mockRestore();
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

  it("prewarms pages above on large upward wheel and touch gestures before native scroll paints", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const OriginalTouchEvent = window.TouchEvent;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRectDescriptor = Object.getOwnPropertyDescriptor(
      Element.prototype,
      "getBoundingClientRect",
    );
    const messages = makeCommandMessages(80);
    const clientHeight = 500;
    const compactMeasuredSlotHeight = 40;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let scrollTop = 0;

    class ResizeObserverMock {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        resizeCallbacks.set(target, this.callback);
      }
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

    const getVirtualNodeHeight = (node: HTMLElement): number => {
      if (node.classList.contains("virtualized-message-spacer")) {
        return Number.parseFloat(node.style.height) || 0;
      }
      if (node.classList.contains("virtualized-message-page-gap")) {
        return Number.parseFloat(node.style.height) || 0;
      }
      if (node.classList.contains("virtualized-message-slot")) {
        return compactMeasuredSlotHeight;
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
        }

        return null;
      };

      const top = walk(root);
      if (top === null) {
        throw new Error("target node not found in virtualized message list");
      }
      return top;
    };

    const getFirstMountedMessageIndex = (container: HTMLElement) => {
      const firstSlot = container.querySelector<HTMLElement>(".virtualized-message-slot");
      const messageId = firstSlot?.dataset.messageId ?? "";
      const index = Number.parseInt(messageId.replace("command-", ""), 10);
      return Number.isFinite(index) ? index : 0;
    };

    const scrollNode = document.createElement("div");
    scrollNode.style.lineHeight = "40px";
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
      get: () => {
        const list = document.querySelector(".virtualized-message-list");
        return list instanceof HTMLElement
          ? getVirtualNodeHeight(list)
          : buildVirtualizedMessageLayout(messages.map(() => 180)).totalHeight;
      },
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.TouchEvent = TouchEventMock as unknown as typeof TouchEvent;
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
          bottom: clientHeight,
          height: clientHeight,
          left: 0,
          right: 100,
          top: 0,
          width: 100,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }

      const root = element.closest(".virtualized-message-list") as HTMLElement | null;
      if (root) {
        const top = getVirtualContentTop(root, element) - scrollTop;
        const height = getVirtualNodeHeight(element);
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

      await act(async () => {
        new Set(resizeCallbacks.values()).forEach((callback) => {
          callback([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        });
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(container.querySelectorAll(".virtualized-message-slot").length).toBeGreaterThan(0);
      });

      const wheelInputs: WheelEventInit[] = [
        { deltaY: -1800 },
        { deltaMode: WheelEvent.DOM_DELTA_LINE, deltaY: -45 },
        { deltaMode: WheelEvent.DOM_DELTA_PAGE, deltaY: -3 },
      ];
      const resolveWheelDelta = (wheelInput: WheelEventInit) => {
        if (wheelInput.deltaMode === WheelEvent.DOM_DELTA_LINE) {
          const computedLineHeight = Number.parseFloat(
            window.getComputedStyle(scrollNode).lineHeight,
          );
          const lineHeight = Number.isFinite(computedLineHeight) ? computedLineHeight : 16;
          return (wheelInput.deltaY ?? 0) * lineHeight;
        }
        if (wheelInput.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
          return (wheelInput.deltaY ?? 0) * clientHeight;
        }
        return wheelInput.deltaY ?? 0;
      };
      const dispatchTouchGesture = (
        touchStartClientY: number,
        touchMoveClientY: number,
      ) => {
        dispatchTouch("touchstart", [touchStartClientY], [touchStartClientY]);
        dispatchTouch("touchmove", [touchMoveClientY], [touchMoveClientY]);
      };
      const dispatchTouch = (
        type: string,
        touchClientYs: number[],
        changedTouchClientYs = touchClientYs,
      ) => {
        scrollNode.dispatchEvent(new TouchEvent(type, {
          bubbles: true,
          changedTouches: changedTouchClientYs.map((clientY) => ({ clientY }) as Touch),
          touches: touchClientYs.map((clientY) => ({ clientY }) as Touch),
        }));
      };

      for (const wheelInput of wheelInputs) {
        await act(async () => {
          scrollTop = 0;
          notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
          await Promise.resolve();
        });
        await act(async () => {
          scrollTop = 3600;
          notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
          await Promise.resolve();
        });

        await waitFor(() => {
          expect(getFirstMountedMessageIndex(container)).toBeGreaterThan(1);
        });
        const firstMountedBeforeWheel = getFirstMountedMessageIndex(container);

        await act(async () => {
          fireEvent.wheel(scrollNode, wheelInput);
          scrollTop = Math.max(3600 + resolveWheelDelta(wheelInput), 0);
          notifyMessageStackScrollWrite(scrollNode);
          await Promise.resolve();
        });

        await waitFor(() => {
          expect(getFirstMountedMessageIndex(container)).toBeLessThan(firstMountedBeforeWheel);
          const firstMountedPage = container.querySelector<HTMLElement>(
            ".virtualized-message-page",
          );
          expect(firstMountedPage?.getBoundingClientRect().top).toBeLessThanOrEqual(0);
        });
      }

      await act(async () => {
        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });
      await act(async () => {
        scrollTop = 3600;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(getFirstMountedMessageIndex(container)).toBeGreaterThan(1);
      });
      const firstMountedBeforeStaleTouchMove = getFirstMountedMessageIndex(container);

      await act(async () => {
        dispatchTouch("touchstart", [100], [100]);
        dispatchTouch("touchend", [], [100]);
        dispatchTouch("touchmove", [1900], [1900]);
        await Promise.resolve();
      });

      expect(getFirstMountedMessageIndex(container)).toBe(
        firstMountedBeforeStaleTouchMove,
      );

      const firstMountedBeforeMultiTouch = getFirstMountedMessageIndex(container);

      await act(async () => {
        // If the first finger lifts while another finger remains down,
        // touchend must keep tracking the remaining touch. The following
        // touchmove should still prewarm before native scroll writes.
        dispatchTouch("touchstart", [100, 300], [100, 300]);
        dispatchTouch("touchend", [300], [100]);
        dispatchTouch("touchmove", [1900], [1900]);
        await Promise.resolve();
      });

      expect(getFirstMountedMessageIndex(container)).toBeLessThan(
        firstMountedBeforeMultiTouch,
      );
      expect(
        container
          .querySelector<HTMLElement>(".virtualized-message-page")
          ?.getBoundingClientRect().top,
      ).toBeLessThanOrEqual(0);

      await act(async () => {
        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });
      await act(async () => {
        scrollTop = 3600;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(getFirstMountedMessageIndex(container)).toBeGreaterThan(1);
      });
      const firstMountedBeforeTouch = getFirstMountedMessageIndex(container);

      await act(async () => {
        // Finger moving down by 1800 px scrolls content upward by the same
        // magnitude, so the touch path should prewarm the pages above before
        // the browser's native scroll write paints.
        dispatchTouchGesture(100, 1900);
        scrollTop = 1800;
        notifyMessageStackScrollWrite(scrollNode);
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(getFirstMountedMessageIndex(container)).toBeLessThan(
          firstMountedBeforeTouch,
        );
        const firstMountedPage = container.querySelector<HTMLElement>(
          ".virtualized-message-page",
        );
        expect(firstMountedPage?.getBoundingClientRect().top).toBeLessThanOrEqual(0);
      });

      await act(async () => {
        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });
      await act(async () => {
        scrollTop = 3600;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(getFirstMountedMessageIndex(container)).toBeGreaterThan(1);
      });
      const firstMountedBeforeLargeWrite = getFirstMountedMessageIndex(container);

      await act(async () => {
        fireEvent.wheel(scrollNode, { deltaY: -1 });
        scrollTop = 1800;
        notifyMessageStackScrollWrite(scrollNode, {
          scrollKind: "incremental",
          scrollSource: "user",
        });
        await Promise.resolve();
      });

      expect(getFirstMountedMessageIndex(container)).toBeLessThan(
        firstMountedBeforeLargeWrite,
      );
      const firstMountedPage = container.querySelector<HTMLElement>(
        ".virtualized-message-page",
      );
      expect(firstMountedPage?.getBoundingClientRect().top).toBeLessThanOrEqual(0);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.TouchEvent = OriginalTouchEvent;
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

  it("ignores upward wheel prewarm when a nested scrollable consumes the wheel", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const messages = makeTextMessages(80);
    let scrollTop = 0;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const getFirstMountedMessageIndex = (container: HTMLElement) => {
      const firstSlot = container.querySelector<HTMLElement>(".virtualized-message-slot");
      const messageId = firstSlot?.dataset.messageId ?? "";
      const index = Number.parseInt(messageId.replace("message-", ""), 10);
      return Number.isFinite(index) ? index : 0;
    };

    const scrollNode = document.createElement("div");
    document.body.appendChild(scrollNode);
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
      get: () => 20000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">
              <div className="nested-scrollable" style={{ overflowY: "auto" }}>
                {message.id}
              </div>
            </article>
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
        { container: scrollNode },
      );

      await waitFor(() => {
        expect(container.querySelectorAll(".virtualized-message-slot").length).toBeGreaterThan(0);
      });

      await act(async () => {
        scrollTop = 3600;
        notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" });
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(getFirstMountedMessageIndex(container)).toBeGreaterThan(1);
      });
      const firstMountedBeforeWheel = getFirstMountedMessageIndex(container);
      const nestedScrollable = container.querySelector<HTMLElement>(".nested-scrollable");
      expect(nestedScrollable).not.toBeNull();
      let nestedScrollTop = 40;
      Object.defineProperty(nestedScrollable!, "clientHeight", {
        configurable: true,
        get: () => 100,
      });
      Object.defineProperty(nestedScrollable!, "scrollHeight", {
        configurable: true,
        get: () => 400,
      });
      Object.defineProperty(nestedScrollable!, "scrollTop", {
        configurable: true,
        get: () => nestedScrollTop,
        set: (nextValue: number) => {
          nestedScrollTop = nextValue;
        },
      });

      act(() => {
        fireEvent.wheel(nestedScrollable!, { deltaY: -1800 });
      });

      expect(getFirstMountedMessageIndex(container)).toBe(firstMountedBeforeWheel);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      scrollNode.remove();
    }
  });

  it("does not hit nested update depth when a scrollbar drag jumps to a distant region", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(3000);
    const measuredSlotHeight = 24;
    let scrollTop = 0;
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

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
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () =>
        buildVirtualizedMessageLayout(messages.map(() => measuredSlotHeight)).totalHeight,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
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
        expect(container.querySelectorAll(".virtualized-message-slot").length).toBeGreaterThan(0);
      });

      await act(async () => {
        scrollTop = 32000;
        fireEvent.scroll(scrollNode);
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(container.querySelectorAll(".virtualized-message-slot").length).toBeGreaterThan(0);
      });

      const loggedErrors = consoleErrorSpy.mock.calls
        .flat()
        .map((entry) => String(entry))
        .join("\n");
      expect(loggedErrors).not.toContain("Maximum update depth exceeded");
    } finally {
      consoleErrorSpy.mockRestore();
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("reclassifies a programmatic jump as seek even after an incremental scroll gesture", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(1000);
    const measuredSlotHeight = 24;
    let scrollTop = 0;

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
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 100000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
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
        expect(container.querySelectorAll(".virtualized-message-page").length).toBeGreaterThan(0);
      });

      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: 80 });
      });

      await act(async () => {
        scrollTop = 32000;
        notifyMessageStackScrollWrite(scrollNode);
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(container.querySelectorAll(".virtualized-message-page").length).toBeGreaterThan(0);
      });

      const mountedPagesAfterJump = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      );
      expect(mountedPagesAfterJump.length).toBeLessThan(20);
      expect(mountedPagesAfterJump[0]?.dataset.pageKey?.startsWith("0:")).toBe(false);

      await act(async () => {
        scrollTop = 32040;
        fireEvent.scroll(scrollNode);
        await Promise.resolve();
      });

      const mountedPagesAfterNativeScroll = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      );
      expect(mountedPagesAfterNativeScroll.length).toBeLessThan(20);
      expect(mountedPagesAfterNativeScroll[0]?.dataset.pageKey?.startsWith("0:")).toBe(false);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("uses explicit scrollKind detail for a sub-threshold programmatic seek jump", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(1000);
    const measuredSlotHeight = 10;
    let scrollTop = 0;

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
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 100000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
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
      if (this === scrollNode) {
        return {
          bottom: 100,
          height: 100,
          left: 0,
          right: 1000,
          top: 0,
          width: 1000,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      }
      const pageNode = (this as HTMLElement).closest(".virtualized-message-page");
      if (pageNode instanceof HTMLElement) {
        const pageKey = pageNode.dataset.pageKey ?? "";
        const pageIndex = Number.parseInt(pageKey.split(":")[0] ?? "0", 10) || 0;
        const pageTop = pageIndex * measuredSlotHeight * 8;
        const pageHeight = measuredSlotHeight * 8;
        return {
          bottom: pageTop + pageHeight - scrollTop,
          height: pageHeight,
          left: 0,
          right: 100,
          top: pageTop - scrollTop,
          width: 100,
          x: 0,
          y: pageTop - scrollTop,
          toJSON: () => ({}),
        } as DOMRect;
      }
      const slotNode = (this as HTMLElement).closest(".virtualized-message-slot");
      if (!(slotNode instanceof HTMLElement)) {
        return originalGetBoundingClientRect.call(this);
      }
      const pageNodeForSlot = slotNode.closest(".virtualized-message-page");
      const pageKey = pageNodeForSlot instanceof HTMLElement ? pageNodeForSlot.dataset.pageKey ?? "" : "";
      const pageIndex = Number.parseInt(pageKey.split(":")[0] ?? "0", 10) || 0;
      const slotIndexWithinPage = Array.from(
        pageNodeForSlot?.querySelectorAll(".virtualized-message-slot") ?? [],
      ).indexOf(slotNode);
      const pageTop = pageIndex * measuredSlotHeight * 8;
      const slotTop = pageTop + Math.max(slotIndexWithinPage, 0) * measuredSlotHeight;
      return {
        bottom: slotTop + measuredSlotHeight - scrollTop,
        height: measuredSlotHeight,
        left: 0,
        right: 100,
        top: slotTop - scrollTop,
        width: 100,
        x: 0,
        y: slotTop - scrollTop,
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
        expect(container.querySelectorAll(".virtualized-message-page").length).toBeGreaterThan(0);
      });

      await act(async () => {
        scrollTop = 2000;
        notifyMessageStackScrollWrite(scrollNode);
        await Promise.resolve();
      });

      const mountedPagesAfterLargeJump = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      );
      expect(mountedPagesAfterLargeJump.length).toBeGreaterThan(0);
      const firstMountedPageKeyAfterLargeJump =
        mountedPagesAfterLargeJump[0]?.dataset.pageKey ?? null;
      expect(firstMountedPageKeyAfterLargeJump?.startsWith("0:")).toBe(false);

      await act(async () => {
        scrollTop = 2400;
        notifyMessageStackScrollWrite(scrollNode, {
          scrollKind: "seek",
        });
        await Promise.resolve();
      });

      const mountedPagesAfterExplicitSeek = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      );
      expect(mountedPagesAfterExplicitSeek.length).toBeGreaterThan(0);
      expect(mountedPagesAfterExplicitSeek[0]?.dataset.pageKey).not.toBe(
        firstMountedPageKeyAfterLargeJump,
      );
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("compacts the mounted band again after a programmatic jump settles", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(1000);
    const measuredSlotHeight = 24;
    let scrollTop = 0;

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
    Object.defineProperty(scrollNode, "clientWidth", {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 100000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
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
        expect(container.querySelectorAll(".virtualized-message-page").length).toBeGreaterThan(0);
      });

      vi.useFakeTimers();

      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: 80 });
      });

      await act(async () => {
        scrollTop = 32000;
        notifyMessageStackScrollWrite(scrollNode);
        await Promise.resolve();
      });

      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: 40 });
      });

      await act(async () => {
        scrollTop = 32040;
        fireEvent.scroll(scrollNode);
        await Promise.resolve();
      });

      await act(async () => {
        scrollTop = 32120;
        fireEvent.scroll(scrollNode);
        await Promise.resolve();
      });

      const mountedPageKeysBeforeIdle = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      ).map((page) => page.dataset.pageKey);

      await act(async () => {
        vi.advanceTimersByTime(250);
        await Promise.resolve();
        await Promise.resolve();
      });

      const mountedPagesAfterIdle = Array.from(
        container.querySelectorAll<HTMLElement>(".virtualized-message-page"),
      );
      const mountedPageKeysAfterIdle = mountedPagesAfterIdle.map(
        (page) => page.dataset.pageKey,
      );
      expect(mountedPageKeysAfterIdle).not.toEqual(mountedPageKeysBeforeIdle);
      expect(mountedPagesAfterIdle[0]?.dataset.pageKey?.startsWith("0:")).toBe(false);
    } finally {
      vi.useRealTimers();
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("preserves upward wheel progress when an overestimated row above the viewport shrinks", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    const messages = makeTextMessages(4);
    const measuredHeights = new Map<string, number>([
      ["message-1", 180],
      ["message-2", 180],
      ["message-3", 180],
      ["message-4", 180],
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
        messages.map((message) => measuredHeights.get(message.id) ?? 180),
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
          ? (measuredHeights.get(messageId) ?? 180)
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

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      scrollWrites.length = 0;
      act(() => {
        scrollTop = 360;
        fireEvent.wheel(scrollNode, { deltaY: -120 });
        fireEvent.scroll(scrollNode);
      });

      measuredHeights.set("message-1", 100);
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toHaveLength(0);
      expect(scrollTop).toBe(360);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("keeps the auto-load scroll listener bound across many scroll events", async () => {
    // Regression guard for the re-subscribe churn in the
    // auto-load-older-messages effect. Before the fix, the effect
    // depended on `viewportVisibleRange.startIndex`, which changes on
    // every scroll event (via `syncViewport` -> `setViewport` ->
    // `viewportVisibleRange` memo re-run). Each dep change ran the
    // cleanup (`removeEventListener`) and re-ran the body
    // (`addEventListener`). Between those two steps a wheel event
    // could land with no listener bound. After the fix the effect
    // reads `viewportVisibleRange` through a ref, so the listener is
    // attached once per `isActive` / `hasOlderMessages` / `sessionId`
    // transition — and scroll events no longer churn it.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let scrollTop = 50;
    const addScrollCalls: Array<{ options?: AddEventListenerOptions | boolean }> = [];
    const removeScrollCalls: Array<Record<string, never>> = [];

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
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 2000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });
    // Wrap addEventListener / removeEventListener to count scroll
    // registrations specifically. Other listeners (wheel, touchmove,
    // keydown) pass through unchanged.
    const originalAdd = scrollNode.addEventListener.bind(scrollNode);
    const originalRemove = scrollNode.removeEventListener.bind(scrollNode);
    scrollNode.addEventListener = ((
      event: string,
      handler: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (event === "scroll") {
        addScrollCalls.push({ options });
      }
      return originalAdd(event, handler, options);
    }) as typeof scrollNode.addEventListener;
    scrollNode.removeEventListener = ((
      event: string,
      handler: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (event === "scroll") {
        removeScrollCalls.push({});
      }
      return originalRemove(event, handler, options);
    }) as typeof scrollNode.removeEventListener;

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
        ? 180
        : 500;
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
      // 210 messages -> windowStartIndex = 210 - 200 = 10 -> hasOlderMessages = true
      // triggers the auto-load scroll effect and attaches a listener.
      render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(210)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // Let post-activation measurements settle. In the buffered-window model
      // the scroll listener attaches once and stays bound across the later
      // rewindowing / auto-load work.
      await waitFor(() => {
        expect(addScrollCalls.length).toBeGreaterThanOrEqual(1);
      });
      const baselineAddCount = addScrollCalls.length;
      const baselineRemoveCount = removeScrollCalls.length;

      // Fire a burst of scroll events that each drive a
      // `setViewport` -> `viewportVisibleRange` recompute. Before
      // the fix, each event would churn the auto-load listener
      // (add + remove). After the fix, the counts stay flat.
      for (let i = 0; i < 10; i += 1) {
        scrollTop = 1000 - i * 25;
        await act(async () => {
          scrollNode.dispatchEvent(new Event("scroll"));
          await Promise.resolve();
        });
      }

      expect(addScrollCalls.length).toBe(baselineAddCount);
      expect(removeScrollCalls.length).toBe(baselineRemoveCount);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("completes post-activation measuring when the first real height matches the estimate", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();

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
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 102,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => 0,
      set: () => {},
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? 102
        : 500;
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
          messages={makeTextMessages(1)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const list = container.querySelector(".virtualized-message-list");
      expect(list).not.toBeNull();
      expect(list).not.toHaveClass("is-measuring-post-activation");
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("arms post-activation measuring synchronously on inactive -> active transitions (no visible pre-measure paint)", async () => {
    // Regression guard for the "shows messages, then empty" flicker on
    // tab switch. The previous `useLayoutEffect`-based transition left
    // a render frame where `isActive=true` was committed with
    // `measuring=false`, allowing the browser to paint message cards
    // at mis-estimated positions before the effect re-hid them for
    // re-measurement. After the fix, the transition is detected during
    // render so `measuring=true` commits in the SAME render as
    // `isActive=true`.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => 0,
      set: () => {},
    });

    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;
    // Intentionally never resolve measurements: that keeps the list in
    // the measuring phase across the whole test, so the assertion below
    // pins "class present immediately after the transition" rather than
    // "class present at some later settled state".
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        return {
          bottom: 0,
          height: 0,
          left: 0,
          right: 0,
          top: 0,
          width: 0,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;

      // Mount inactive with messages already present, matching the
      // cached-but-not-focused tab shape.
      const { container, rerender } = render(
        <VirtualizedConversationMessageList
          isActive={false}
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(120)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // While inactive, no measuring.
      const preList = container.querySelector(".virtualized-message-list");
      expect(preList).not.toHaveClass("is-measuring-post-activation");

      // Flip to active — the "tab switch" scenario.
      rerender(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(120)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // The list must carry the measuring class after the synchronous
      // transition render settles. This guards the steady-state class, while
      // docs/bugs.md tracks a deeper timing harness for pre-paint behavior.
      const postList = container.querySelector(".virtualized-message-list");
      expect(postList).not.toBeNull();
      expect(postList).toHaveClass("is-measuring-post-activation");
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });
});
