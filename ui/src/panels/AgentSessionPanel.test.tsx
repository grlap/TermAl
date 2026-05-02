import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useLayoutEffect, type RefObject } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import * as slashPalette from "./session-slash-palette";
import {
  AgentSessionPanel,
  AgentSessionPanelFooter,
  includeUndeferredMessageTail,
} from "./AgentSessionPanel";
import { buildConversationOverviewTailItems } from "./conversation-overview-controller";
import { VirtualizedConversationMessageList } from "./VirtualizedConversationMessageList";
import { RunningIndicator } from "./session-activity-cards";
import { notifyMessageStackScrollWrite } from "../message-stack-scroll-sync";
import {
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
} from "../session-store";
import {
  VIRTUALIZED_MESSAGE_GAP_PX,
  buildVirtualizedMessageLayout,
  clampVirtualizedViewportScrollTop,
  estimateConversationMessageHeight,
  getAdjustedVirtualizedScrollTopForHeightChange,
  getScrollContainerBottomGap,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import type { CommandMessage, DiffMessage, Message, Session } from "../types";

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

const EMPTY_AGENT_COMMANDS: {
  kind?: "promptTemplate" | "nativeSlash";
  name: string;
  description: string;
  content: string;
  source: string;
  argumentHint?: string | null;
}[] = [];
const formatFooterByteSize = (byteSize: number) => `${byteSize} B`;

function renderSessionPanelWithDefaults(
  props: Partial<Parameters<typeof AgentSessionPanel>[0]> & {
    activeSession?: Session | null;
  },
) {
  const { activeSession = null, ...panelProps } = props;
  syncComposerSessionsStore({
    sessions: activeSession ? [activeSession] : [],
    draftsBySessionId: {},
    draftAttachmentsBySessionId: {},
  });
  return render(
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
      {...panelProps}
    />,
  );
}

afterEach(() => {
  act(() => {
    resetSessionStoreForTesting();
  });
});

describe("AgentSessionPanel conversation caching", () => {
  it("renders current same-id message updates at the active transcript tail", () => {
    const stableMessage: Message = {
      author: "you",
      id: "message-1",
      text: "Prompt",
      timestamp: "10:00",
      type: "text",
    };
    const deferredAssistant: Message = {
      author: "assistant",
      id: "message-2",
      text: "Old streamed answer",
      timestamp: "10:01",
      type: "text",
    };
    const currentAssistant: Message = {
      ...deferredAssistant,
      text: "Old streamed answer plus the latest chunk",
    };

    expect(
      includeUndeferredMessageTail(
        [stableMessage, deferredAssistant],
        [stableMessage, currentAssistant],
      ),
    ).toEqual([stableMessage, currentAssistant]);
  });

  it("drops deferred transcript objects when the active session changes", () => {
    const deferredMessage: Message = {
      author: "assistant",
      id: "message-a",
      text: "Session A",
      timestamp: "10:00",
      type: "text",
    };
    const currentMessage: Message = {
      author: "assistant",
      id: "message-b",
      text: "Session B",
      timestamp: "10:00",
      type: "text",
    };

    const currentMessages = [currentMessage];

    expect(includeUndeferredMessageTail([deferredMessage], currentMessages)).toBe(
      currentMessages,
    );
  });

  it("drops deferred transcript objects when the current session is empty", () => {
    const deferredMessage: Message = {
      author: "assistant",
      id: "message-a",
      text: "Previous session",
      timestamp: "10:00",
      type: "text",
    };
    const currentMessages: Message[] = [];

    expect(includeUndeferredMessageTail([deferredMessage], currentMessages)).toBe(
      currentMessages,
    );
  });

  it("keeps the deferred array when all current messages are unchanged", () => {
    const message = makeTextMessages(1)[0];
    const deferredMessages = [message];

    expect(
      includeUndeferredMessageTail(deferredMessages, deferredMessages),
    ).toBe(deferredMessages);
  });

  it("returns the current array when deferred messages include pruned tail items", () => {
    const currentMessages = makeTextMessages(1);
    const deferredMessages = [...currentMessages, makeTextMessages(2)[1]];

    expect(includeUndeferredMessageTail(deferredMessages, currentMessages)).toBe(
      currentMessages,
    );
  });

  it("splices from the first same-id message object change and preserves stable deferred prefix", () => {
    const [firstMessage, deferredChangedMessage, trailingMessage] =
      makeTextMessages(3);
    const currentChangedMessage: Message = {
      author: "assistant",
      id: deferredChangedMessage.id,
      text: "Updated middle message",
      timestamp: deferredChangedMessage.timestamp,
      type: "text",
    };
    const result = includeUndeferredMessageTail(
      [firstMessage, deferredChangedMessage, trailingMessage],
      [firstMessage, currentChangedMessage, trailingMessage],
    );

    expect(result).toEqual([
      firstMessage,
      currentChangedMessage,
      trailingMessage,
    ]);
    expect(result[0]).toBe(firstMessage);
    expect(result[1]).toBe(currentChangedMessage);
    expect(result[2]).toBe(trailingMessage);
  });

  it("appends current tail messages after an unchanged deferred prefix", () => {
    const [firstMessage, secondMessage] = makeTextMessages(2);
    const deferredMessages = [firstMessage];
    const result = includeUndeferredMessageTail(deferredMessages, [
      firstMessage,
      secondMessage,
    ]);

    expect(result).toEqual([firstMessage, secondMessage]);
    expect(result[0]).toBe(firstMessage);
    expect(result[1]).toBe(secondMessage);
  });

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
    const renderCommandCard = () => null;
    const renderDiffCard = () => null;
    const renderMessageCard = (message: Message) => (
      <article className="message-card">{message.id}</article>
    );

    const renderPanel = (
      promptLabel: string,
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
        renderCommandCard={renderCommandCard}
        renderDiffCard={renderDiffCard}
        renderMessageCard={renderMessageCard}
        renderPromptSettings={() => <p>{promptLabel}</p>}
      />
    );

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
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const commandMessages = makeCommandMessages(1);
    const scrollContainerRef = { current: document.createElement("section") };
    const matchedItemKeys = new Set<string>();
    const noopApproval = () => {};
    const noopUserInput = () => {};
    const noopElicitation = () => {};
    const noopAppRequest = () => {};
    const noopCancel = () => {};
    const noopSettingsChange = () => {};
    const noopSearchMount = () => {};
    const renderDiffCard = () => null;
    const renderMessageCard = (message: Message) => (
      <article className="message-card">{message.id}</article>
    );
    const renderPromptSettings = () => null;

    const renderPanel = (label: string) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="commands"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={commandMessages}
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
        renderCommandCard={(message) => (
          <article>{`${label}: ${message.id}`}</article>
        )}
        renderDiffCard={renderDiffCard}
        renderMessageCard={renderMessageCard}
        renderPromptSettings={renderPromptSettings}
      />
    );

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
    act(() => {
      syncComposerSessionsStore({
        sessions: [activeSession],
        draftsBySessionId: {},
        draftAttachmentsBySessionId: {},
      });
    });

    const diffMessages = makeDiffMessages(1);
    const scrollContainerRef = { current: document.createElement("section") };
    const matchedItemKeys = new Set<string>();
    const noopApproval = () => {};
    const noopUserInput = () => {};
    const noopElicitation = () => {};
    const noopAppRequest = () => {};
    const noopCancel = () => {};
    const noopSettingsChange = () => {};
    const noopSearchMount = () => {};
    const renderCommandCard = () => null;
    const renderMessageCard = (message: Message) => (
      <article className="message-card">{message.id}</article>
    );
    const renderPromptSettings = () => null;

    const renderPanel = (label: string) => (
      <AgentSessionPanel
        paneId="pane-1"
        viewMode="diffs"
        activeSessionId={activeSession.id}
        isLoading={false}
        isUpdating={false}
        showWaitingIndicator={false}
        waitingIndicatorPrompt={null}
        commandMessages={[]}
        diffMessages={diffMessages}
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
        renderCommandCard={renderCommandCard}
        renderDiffCard={(message) => (
          <article>{`${label}: ${message.id}`}</article>
        )}
        renderMessageCard={renderMessageCard}
        renderPromptSettings={renderPromptSettings}
      />
    );

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

  it("renders a conversation overview rail for long active sessions", () => {
    const OriginalResizeObserver = window.ResizeObserver;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    try {
      renderSessionPanelWithDefaults({
        activeSession: makeSession("active-session", {
          status: "active",
          messages: makeTextMessages(90),
        }),
        showWaitingIndicator: true,
        waitingIndicatorPrompt: "run the build",
      });

      expect(screen.getAllByLabelText("Conversation overview")).toHaveLength(1);
      const rail = screen.getByLabelText("Conversation overview");
      expect(rail).toBeInTheDocument();
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

  it("renders the new window immediately after a parent-owned scroll write", () => {
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

      act(() => {
        scrollTop = 0;
        notifyMessageStackScrollWrite(scrollNode);
      });

      expect(screen.queryByText("message-1")).toBeInTheDocument();
      expect(screen.queryByText("message-120")).not.toBeInTheDocument();

      act(() => {
        scrollTop = estimatedLayout.totalHeight - clientHeight;
        notifyMessageStackScrollWrite(scrollNode, {
          scrollKind: "bottom_boundary",
        });
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

        act(() => {
          fireEvent.wheel(scrollNode, wheelInput);
          scrollTop = Math.max(3600 + resolveWheelDelta(wheelInput), 0);
          notifyMessageStackScrollWrite(scrollNode);
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

      act(() => {
        dispatchTouch("touchstart", [100], [100]);
        dispatchTouch("touchend", [], [100]);
        dispatchTouch("touchmove", [1900], [1900]);
      });

      expect(getFirstMountedMessageIndex(container)).toBe(
        firstMountedBeforeStaleTouchMove,
      );

      const firstMountedBeforeMultiTouch = getFirstMountedMessageIndex(container);

      act(() => {
        // If the first finger lifts while another finger remains down,
        // touchend must keep tracking the remaining touch. The following
        // touchmove should still prewarm before native scroll writes.
        dispatchTouch("touchstart", [100, 300], [100, 300]);
        dispatchTouch("touchend", [300], [100]);
        dispatchTouch("touchmove", [1900], [1900]);
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

      act(() => {
        // Finger moving down by 1800 px scrolls content upward by the same
        // magnitude, so the touch path should prewarm the pages above before
        // the browser's native scroll write paints.
        dispatchTouchGesture(100, 1900);
        scrollTop = 1800;
        notifyMessageStackScrollWrite(scrollNode);
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

      act(() => {
        fireEvent.wheel(scrollNode, { deltaY: -1 });
        scrollTop = 1800;
        notifyMessageStackScrollWrite(scrollNode, {
          scrollKind: "incremental",
          scrollSource: "user",
        });

        expect(getFirstMountedMessageIndex(container)).toBeLessThan(
          firstMountedBeforeLargeWrite,
        );
        const firstMountedPage = container.querySelector<HTMLElement>(
          ".virtualized-message-page",
        );
        expect(firstMountedPage?.getBoundingClientRect().top).toBeLessThanOrEqual(0);
      });
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

      // The list must carry the measuring class IMMEDIATELY after the
      // rerender — no intermediate frame with isActive=true but
      // measuring=false. testing-library's `rerender` is synchronous;
      // if the transition were detected in a useLayoutEffect instead
      // of during render, React would still commit one DOM state
      // without the class before the effect set it. The querySelector
      // here reads the DOM after the transition commit completes.
      const postList = container.querySelector(".virtualized-message-list");
      expect(postList).not.toBeNull();
      expect(postList).toHaveClass("is-measuring-post-activation");
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });
});

function renderFooter({
  isPaneActive = true,
  session,
  committedDraft = "",
  isUpdating = false,
  onDraftCommit = vi.fn(),
  modelOptionsError = null,
  agentCommands = EMPTY_AGENT_COMMANDS,
  hasLoadedAgentCommands = true,
  isRefreshingAgentCommands = false,
  agentCommandsError = null,
  onRefreshSessionModelOptions = vi.fn(),
  onRefreshAgentCommands = vi.fn(),
  onSend = vi.fn(() => true),
  onSessionSettingsChange = vi.fn(),
}: {
  isPaneActive?: boolean;
  session: Session | null;
  committedDraft?: string;
  isUpdating?: boolean;
  onDraftCommit?: (sessionId: string, nextValue: string) => void;
  modelOptionsError?: string | null;
  agentCommands?: {
    kind?: "promptTemplate" | "nativeSlash";
    name: string;
    description: string;
    content: string;
    source: string;
    argumentHint?: string | null;
  }[];
  hasLoadedAgentCommands?: boolean;
  isRefreshingAgentCommands?: boolean;
  agentCommandsError?: string | null;
  onRefreshSessionModelOptions?: (sessionId: string) => void;
  onRefreshAgentCommands?: (sessionId: string) => void;
  onSend?: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onSessionSettingsChange?: (sessionId: string, field: string, value: string) => void;
}) {
  syncComposerSessionsStore({
    sessions: session ? [session] : [],
    draftsBySessionId: session ? { [session.id]: committedDraft } : {},
    draftAttachmentsBySessionId: {},
  });
  return (
    <AgentSessionPanelFooter
      paneId="pane-1"
      viewMode="session"
      isPaneActive={isPaneActive}
      activeSessionId={session?.id ?? null}
      formatByteSize={formatFooterByteSize}
      isSending={false}
      isStopping={false}
      isSessionBusy={false}
      isUpdating={isUpdating}
      showNewResponseIndicator={false}
      footerModeLabel="Session"
      onScrollToLatest={() => {}}
      onDraftCommit={onDraftCommit}
      onDraftAttachmentRemove={() => {}}
      isRefreshingModelOptions={false}
      modelOptionsError={modelOptionsError}
      agentCommands={agentCommands}
      hasLoadedAgentCommands={hasLoadedAgentCommands}
      isRefreshingAgentCommands={isRefreshingAgentCommands}
      agentCommandsError={agentCommandsError}
      onRefreshSessionModelOptions={onRefreshSessionModelOptions}
      onRefreshAgentCommands={onRefreshAgentCommands}
      onSend={onSend}
      onSessionSettingsChange={onSessionSettingsChange}
      onStopSession={() => {}}
      onPaste={() => {}}
    />
  );
}

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

describe("AgentSessionPanelFooter", () => {
  it("shows a command badge in the live turn card for slash commands", () => {
    render(<RunningIndicator agent="Codex" lastPrompt="/review-local" />);

    expect(screen.getAllByText("Command")).toHaveLength(2);
    expect(screen.getByText("Executing a command...")).toBeInTheDocument();
  });

  it("does not show a command badge in the live turn card for regular prompts", () => {
    render(<RunningIndicator agent="Codex" lastPrompt="Review the staged diff" />);

    expect(screen.queryByText("Command")).not.toBeInTheDocument();
    expect(screen.getByText("Waiting for the next chunk of output...")).toBeInTheDocument();
  });

  it("does not commit a draft during unrelated session rerenders", () => {
    const initialCommit = vi.fn();
    const nextCommit = vi.fn();
    const sessionId = "session-a";
    const { rerender } = render(
      renderFooter({
        onDraftCommit: initialCommit,
        session: makeSession(sessionId, { preview: "first preview" }),
      }),
    );

    const textarea = screen.getByLabelText(`Message ${sessionId}`);
    fireEvent.change(textarea, { target: { value: "draft in progress" } });

    rerender(
      renderFooter({
        onDraftCommit: nextCommit,
        session: makeSession(sessionId, { preview: "streamed preview", status: "active" }),
      }),
    );

    expect(initialCommit).not.toHaveBeenCalled();
    expect(nextCommit).not.toHaveBeenCalled();
    expect(screen.getByLabelText(`Message ${sessionId}`)).toHaveValue("draft in progress");
  });

  it("dispatches composer sends through the latest parent callback", async () => {
    const initialSend = vi.fn(() => true);
    const latestSend = vi.fn(() => true);
    const session = makeSession("session-a");
    const { rerender } = render(
      renderFooter({
        onSend: initialSend,
        session,
      }),
    );
    await act(async () => {
      await Promise.resolve();
    });

    act(() => {
      fireEvent.change(screen.getByLabelText("Message session-a"), {
        target: { value: "use the newest sender" },
      });
    });

    await act(async () => {
      rerender(
        renderFooter({
          onSend: latestSend,
          session,
        }),
      );
      await Promise.resolve();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      await Promise.resolve();
    });

    expect(initialSend).not.toHaveBeenCalled();
    expect(latestSend).toHaveBeenCalledWith(
      "session-a",
      "use the newest sender",
    );
  });

  it("does not recompute the composer slash palette during assistant-only session churn", () => {
    const sessionId = "session-a";
    const buildSlashPaletteStateSpy = vi.spyOn(slashPalette, "buildSlashPaletteState");
    const { rerender } = render(
      renderFooter({
        committedDraft: "/model",
        session: makeSession(sessionId, {
          status: "active",
          preview: "first preview",
          messages: [
            {
              author: "you",
              id: "user-1",
              text: "Review the staged diff",
              timestamp: "10:00",
              type: "text",
            },
            {
              author: "assistant",
              id: "assistant-1",
              text: "Working...",
              timestamp: "10:01",
              type: "text",
            },
          ],
        }),
      }),
    );

    expect(buildSlashPaletteStateSpy).toHaveBeenCalledTimes(1);

    rerender(
      renderFooter({
        committedDraft: "/model",
        session: makeSession(sessionId, {
          status: "active",
          preview: "still working",
          messages: [
            {
              author: "you",
              id: "user-1",
              text: "Review the staged diff",
              timestamp: "10:00",
              type: "text",
            },
            {
              author: "assistant",
              id: "assistant-1",
              text: "Still working...",
              timestamp: "10:01",
              type: "text",
            },
          ],
        }),
      }),
    );

    expect(buildSlashPaletteStateSpy).toHaveBeenCalledTimes(1);
    buildSlashPaletteStateSpy.mockRestore();
  });

  it("commits the in-progress draft when switching sessions", () => {
    const onDraftCommit = vi.fn();
    const { rerender } = render(
      renderFooter({
        onDraftCommit,
        session: makeSession("session-a"),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "carry this draft" },
    });

    rerender(
      renderFooter({
        onDraftCommit,
        session: makeSession("session-b"),
      }),
    );

    expect(onDraftCommit).toHaveBeenCalledWith("session-a", "carry this draft");
  });

  it("coalesces composer autosize across rapid draft changes", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    let nextFrameId = 0;
    const queuedFrames = new Map<number, FrameRequestCallback>();
    const requestAnimationFrameMock = vi.fn((callback: FrameRequestCallback) => {
      const id = ++nextFrameId;
      queuedFrames.set(id, callback);
      return id;
    });
    const cancelAnimationFrameMock = vi.fn((id: number) => {
      queuedFrames.delete(id);
    });
    const drainAnimationFrames = () => {
      while (queuedFrames.size > 0) {
        const callbacks = [...queuedFrames.values()];
        queuedFrames.clear();
        act(() => {
          callbacks.forEach((callback) => callback(0));
        });
      }
    };
    const getComputedStyleSpy = vi.spyOn(window, "getComputedStyle");

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;

    try {
      render(
        renderFooter({
          session: makeSession("session-a"),
        }),
      );

      drainAnimationFrames();
      requestAnimationFrameMock.mockClear();
      getComputedStyleSpy.mockClear();

      const textarea = screen.getByLabelText("Message session-a");
      fireEvent.change(textarea, { target: { value: "a" } });
      fireEvent.change(textarea, { target: { value: "aa" } });

      expect(requestAnimationFrameMock).toHaveBeenCalledTimes(1);
      expect(queuedFrames.size).toBe(1);

      drainAnimationFrames();
      expect(getComputedStyleSpy).not.toHaveBeenCalled();
    } finally {
      getComputedStyleSpy.mockRestore();
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("keeps multiline composer height after blur commits the draft", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalScrollHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "scrollHeight",
    );
    let nextFrameId = 0;
    const queuedFrames = new Map<number, FrameRequestCallback>();
    const requestAnimationFrameMock = vi.fn((callback: FrameRequestCallback) => {
      const id = ++nextFrameId;
      queuedFrames.set(id, callback);
      return id;
    });
    const cancelAnimationFrameMock = vi.fn((id: number) => {
      queuedFrames.delete(id);
    });
    const drainAnimationFrames = () => {
      while (queuedFrames.size > 0) {
        const callbacks = [...queuedFrames.values()];
        queuedFrames.clear();
        act(() => {
          callbacks.forEach((callback) => callback(0));
        });
      }
    };
    const session = makeSession("session-a");
    let committedDraft = "";
    let rerender: ReturnType<typeof render>["rerender"];
    let unmount: ReturnType<typeof render>["unmount"] | null = null;
    const onDraftCommit = vi.fn((sessionId: string, nextValue: string) => {
      committedDraft = nextValue;
      expect(sessionId).toBe(session.id);
    });

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        if (document.activeElement !== textarea) {
          return 40;
        }
        return 40 + (textarea.value.split("\n").length - 1) * 28;
      },
    });

    try {
      ({ rerender, unmount } = render(
        renderFooter({
          committedDraft,
          onDraftCommit,
          session,
        }),
      ));
      drainAnimationFrames();

      const textarea = screen.getByLabelText("Message session-a");
      if (!(textarea instanceof HTMLTextAreaElement)) {
        throw new Error("Composer textarea not found");
      }

      act(() => {
        textarea.focus();
      });
      act(() => {
        fireEvent.change(textarea, {
          target: { value: "line one\nline two\nline three" },
        });
      });
      drainAnimationFrames();
      expect(textarea.style.height).toBe("96px");

      act(() => {
        textarea.blur();
        fireEvent.blur(textarea);
      });
      act(() => {
        rerender(
          renderFooter({
            committedDraft,
            onDraftCommit,
            session,
          }),
        );
      });
      drainAnimationFrames();

      expect(onDraftCommit).toHaveBeenCalledWith(
        session.id,
        "line one\nline two\nline three",
      );
      expect(textarea.style.height).toBe("96px");
    } finally {
      act(() => {
        unmount?.();
      });
      if (originalScrollHeightDescriptor) {
        Object.defineProperty(
          HTMLTextAreaElement.prototype,
          "scrollHeight",
          originalScrollHeightDescriptor,
        );
      } else {
        delete (
          HTMLTextAreaElement.prototype as unknown as {
            scrollHeight?: number;
          }
        ).scrollHeight;
      }
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("keeps remaining multiline composer height when a line is deleted", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalScrollHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "scrollHeight",
    );
    let nextFrameId = 0;
    const queuedFrames = new Map<number, FrameRequestCallback>();
    const requestAnimationFrameMock = vi.fn((callback: FrameRequestCallback) => {
      const id = ++nextFrameId;
      queuedFrames.set(id, callback);
      return id;
    });
    const cancelAnimationFrameMock = vi.fn((id: number) => {
      queuedFrames.delete(id);
    });
    let unmount: ReturnType<typeof render>["unmount"] | null = null;
    const drainAnimationFrames = () => {
      while (queuedFrames.size > 0) {
        const callbacks = [...queuedFrames.values()];
        queuedFrames.clear();
        act(() => {
          callbacks.forEach((callback) => callback(0));
        });
      }
    };

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        if (
          textarea.style.height !== "0px" &&
          !textarea.value.includes("line two") &&
          textarea.value.length > 0
        ) {
          return 40;
        }
        return 40 + (textarea.value.split("\n").length - 1) * 28;
      },
    });

    try {
      ({ unmount } = render(
        renderFooter({
          session: makeSession("session-a"),
        }),
      ));
      drainAnimationFrames();

      const textarea = screen.getByLabelText("Message session-a");
      if (!(textarea instanceof HTMLTextAreaElement)) {
        throw new Error("Composer textarea not found");
      }

      act(() => {
        textarea.focus();
      });
      act(() => {
        fireEvent.change(textarea, {
          target: { value: "line one\nline two\nline three\nline four" },
        });
      });
      drainAnimationFrames();
      expect(textarea.style.height).toBe("124px");

      act(() => {
        fireEvent.change(textarea, {
          target: { value: "line one\nline three\nline four" },
        });
      });
      drainAnimationFrames();

      expect(textarea.style.height).toBe("96px");
    } finally {
      act(() => {
        unmount?.();
      });
      if (originalScrollHeightDescriptor) {
        Object.defineProperty(
          HTMLTextAreaElement.prototype,
          "scrollHeight",
          originalScrollHeightDescriptor,
        );
      } else {
        delete (
          HTMLTextAreaElement.prototype as unknown as {
            scrollHeight?: number;
          }
        ).scrollHeight;
      }
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("shrinks multiline composer height after a width-only pane resize", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalResizeObserver = window.ResizeObserver;
    const originalScrollHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "scrollHeight",
    );
    let nextFrameId = 0;
    const queuedFrames = new Map<number, FrameRequestCallback>();
    const requestAnimationFrameMock = vi.fn((callback: FrameRequestCallback) => {
      const id = ++nextFrameId;
      queuedFrames.set(id, callback);
      return id;
    });
    const cancelAnimationFrameMock = vi.fn((id: number) => {
      queuedFrames.delete(id);
    });
    const drainAnimationFrames = () => {
      while (queuedFrames.size > 0) {
        const callbacks = [...queuedFrames.values()];
        queuedFrames.clear();
        act(() => {
          callbacks.forEach((callback) => callback(0));
        });
      }
    };
    let resizeCallback: ResizeObserverCallback | null = null;
    let isWide = false;
    let unmount: ReturnType<typeof render>["unmount"] | null = null;

    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
      }
      observe() {}
      disconnect() {}
    }

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        if (isWide && textarea.style.height === "0px") {
          return 40;
        }
        return 96;
      },
    });

    try {
      ({ unmount } = render(
        renderFooter({
          session: makeSession("session-a"),
        }),
      ));
      drainAnimationFrames();

      const textarea = screen.getByLabelText("Message session-a");
      if (!(textarea instanceof HTMLTextAreaElement)) {
        throw new Error("Composer textarea not found");
      }

      act(() => {
        fireEvent.change(textarea, {
          target: { value: "line one\nline two\nline three" },
        });
      });
      drainAnimationFrames();
      expect(textarea.style.height).toBe("96px");

      isWide = true;
      act(() => {
        resizeCallback?.(
          [
            {
              target: textarea,
              contentRect: { width: 600 },
            } as unknown as ResizeObserverEntry,
          ],
          {} as ResizeObserver,
        );
      });
      drainAnimationFrames();

      expect(textarea.style.height).toBe("40px");
    } finally {
      act(() => {
        unmount?.();
      });
      if (originalScrollHeightDescriptor) {
        Object.defineProperty(
          HTMLTextAreaElement.prototype,
          "scrollHeight",
          originalScrollHeightDescriptor,
        );
      } else {
        delete (
          HTMLTextAreaElement.prototype as unknown as {
            scrollHeight?: number;
          }
        ).scrollHeight;
      }
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      window.ResizeObserver = originalResizeObserver;
    }
  });

  it("focuses the prompt when a session opens in the active pane", async () => {
    const { rerender } = render(
      renderFooter({
        session: null,
      }),
    );

    rerender(
      renderFooter({
        session: makeSession("session-a"),
      }),
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Message session-a")).toHaveFocus();
    });
  });

  it("shows the paste-only image attachment hint for active sessions", () => {
    render(
      renderFooter({
        session: makeSession("session-a"),
      }),
    );

    expect(
      screen.getByText(
        "Paste PNG, JPEG, GIF, or WebP images into the prompt. Drag-and-drop is not supported yet.",
      ),
    ).toBeInTheDocument();
  });

  it("does not focus the prompt for an inactive pane", async () => {
    render(
      <>
        <button type="button">Outside focus</button>
        {renderFooter({
          isPaneActive: false,
          session: makeSession("session-a"),
        })}
      </>,
    );

    const outsideButton = screen.getByRole("button", { name: "Outside focus" });
    outsideButton.focus();
    expect(outsideButton).toHaveFocus();

    await waitFor(() => {
      expect(outsideButton).toHaveFocus();
    });
  });

  it("expands /model from the slash command menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/m" } });

    expect(screen.getByText("/model")).toBeInTheDocument();
    expect(screen.getByText("/mode")).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(screen.getByLabelText("Message session-a")).toHaveValue("/model ");
  });

  it("expands /effort from the Claude slash command menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/ef" } });

    expect(screen.getByText("/effort")).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(screen.getByLabelText("Message session-a")).toHaveValue("/effort ");
  });

  it("requests Claude agent commands when slash menu opens", async () => {
    const onRefreshAgentCommands = vi.fn();

    render(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: false,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });

    await waitFor(() => {
      expect(onRefreshAgentCommands).toHaveBeenCalledWith("session-a");
    });
  });

  it("re-requests Claude agent commands when the command revision changes", async () => {
    const onRefreshAgentCommands = vi.fn();
    const { rerender } = render(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: true,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
          agentCommandsRevision: 0,
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review local changes.",
            content: "Review local changes.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });
    expect(onRefreshAgentCommands).not.toHaveBeenCalled();

    rerender(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: true,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
          agentCommandsRevision: 1,
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review local changes.",
            content: "Review local changes.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    await waitFor(() => {
      expect(onRefreshAgentCommands).toHaveBeenCalledWith("session-a");
    });
  });

  it("requests project agent commands for Codex when slash menu opens", async () => {
    const onRefreshAgentCommands = vi.fn();

    render(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: false,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });

    await waitFor(() => {
      expect(onRefreshAgentCommands).toHaveBeenCalledWith("session-a");
    });
  });

  it("shows agent commands alongside session controls", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Review staged and unstaged changes.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });

    expect(screen.getByText("Agent Commands")).toBeInTheDocument();
    expect(screen.getByText("Session Controls")).toBeInTheDocument();
    expect(screen.getByText("/review-local")).toBeInTheDocument();
    expect(screen.getByText("/model")).toBeInTheDocument();
  });

  it("sends a no-argument agent command directly from the slash menu", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            kind: "nativeSlash",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "/review-local",
            source: "Claude project command",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith("session-a", "/review-local");
    expect(textarea).toHaveValue("");
  });

  it("expands an agent command with $ARGUMENTS and sends the substituted prompt", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "fix-bug",
            description: "Fix a bug from docs/bugs.md by number.",
            content: `Fix the requested bug:

$ARGUMENTS

Verify the fix.`,
            source: ".claude/commands/fix-bug.md",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/fix" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(textarea).toHaveValue("/fix-bug ");

    fireEvent.change(textarea, { target: { value: "/fix-bug 3" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith(
      "session-a",
      "/fix-bug 3",
      `Fix the requested bug:

3

Verify the fix.`,
    );
    expect(textarea).toHaveValue("");
  });

  it("expands a native Claude command with arguments and sends the slash prompt", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            kind: "nativeSlash",
            name: "review",
            description: "Review the current changes.",
            content: "/review",
            source: "Claude bundled command",
            argumentHint: "[scope]",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(textarea).toHaveValue("/review ");
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.change(textarea, { target: { value: "/review staged files" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith("session-a", "/review staged files");
    expect(textarea).toHaveValue("");
  });

  it("applies a model slash command with keyboard navigation instead of sending a prompt", () => {
    const onSend = vi.fn(() => true);
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [
            { label: "gpt-5.4", value: "gpt-5.4" },
            { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
          ],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith("session-a", "model", "gpt-5.3-codex");
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("");
  });

  it("applies a model slash choice on space without closing the slash menu", () => {
    const onSend = vi.fn(() => true);
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [
            { label: "gpt-5.4", value: "gpt-5.4" },
            { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
          ],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Space", code: "Space" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith("session-a", "model", "gpt-5.3-codex");
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("/model");
    expect(screen.getByRole("listbox", { name: "Codex models" })).toBeInTheDocument();
  });

  it("applies a manual /model value when the live list does not include it", () => {
    const onSend = vi.fn(() => true);
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [{ label: "gpt-5.4", value: "gpt-5.4" }],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model gpt-5.5-preview" } });
    expect(
      screen.getByText('Use "gpt-5.5-preview"'),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "gpt-5.5-preview is not in the current live model list. TermAl will still try it on the next prompt.",
      ),
    ).toBeInTheDocument();
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "model",
      "gpt-5.5-preview",
    );
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("");
  });

  it("shows rich model metadata in the /model slash menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [
            {
              label: "GPT-5.4",
              value: "gpt-5.4",
              description: "Latest frontier agentic coding model.",
              badges: ["Recommended"],
              defaultReasoningEffort: "medium",
              supportedReasoningEfforts: ["low", "medium", "high"],
            },
          ],
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    expect(
      screen.getByText((content) =>
        content.includes("Latest frontier agentic coding model.") &&
        content.includes("Recommended") &&
        content.includes("Reasoning low, medium, high | Default medium"),
      ),
    ).toBeInTheDocument();
  });

  it("requests live Claude model options from /model when they have not loaded yet", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",

        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });

  it("keeps the current /model option selected until the pointer moves", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
          modelOptions: [
            { label: "Sonnet", value: "sonnet" },
            { label: "Opus", value: "opus" },
          ],
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    const currentOption = screen.getByRole("option", { name: /Sonnet/i });
    const otherOption = screen.getByRole("option", { name: /Opus/i });
    expect(currentOption).toHaveAttribute("aria-selected", "true");
    expect(otherOption).toHaveAttribute("aria-selected", "false");

    fireEvent.mouseEnter(otherOption);
    expect(currentOption).toHaveAttribute("aria-selected", "true");
    expect(otherOption).toHaveAttribute("aria-selected", "false");

    fireEvent.mouseMove(otherOption);
    expect(currentOption).toHaveAttribute("aria-selected", "false");
    expect(otherOption).toHaveAttribute("aria-selected", "true");
  });

  it("applies Claude mode changes from /mode", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Claude",
          claudeApprovalMode: "ask",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/mode" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "claudeApprovalMode",
      "plan",
    );
  });

  it("applies Claude effort changes from /effort", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Claude",
          claudeEffort: "default",
          model: "sonnet",
          modelOptions: [
            {
              label: "Sonnet",
              value: "sonnet",
              badges: ["Effort"],
              supportedClaudeEffortLevels: ["low", "medium", "high"],
            },
          ],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/effort" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "claudeEffort",
      "low",
    );
  });

  it("applies Codex approval and sandbox slash commands", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          sandboxMode: "workspace-write",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/approvals" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "approvalPolicy",
      "on-request",
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/sandbox" },
    });
    fireEvent.keyDown(screen.getByLabelText("Message session-a"), { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByLabelText("Message session-a"), { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "sandboxMode",
      "read-only",
    );
  });

  it("applies Codex reasoning effort changes from /effort", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/effort" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "reasoningEffort",
      "high",
    );
  });

  it("applies /effort on space without closing the slash menu", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/effort" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Space", code: "Space" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "reasoningEffort",
      "high",
    );
    expect(screen.getByLabelText("Message session-a")).toHaveValue("/effort");
    expect(screen.getByRole("listbox", { name: "Codex reasoning effort" })).toBeInTheDocument();
  });

  it("shows a pending slash state while session settings are applying", () => {
    render(
      renderFooter({
        isUpdating: true,
        committedDraft: "/effort",
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "high",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
        }),
      }),
    );

    expect(screen.getByText("Applying setting...")).toBeInTheDocument();
    expect(screen.getByText("Applying")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("resets the slash selection to the current model after the session model changes", async () => {
    const sessionA = makeSession("session-a", {
      agent: "Codex",
      model: "gpt-5.4",
      modelOptions: [
        { label: "gpt-5.4", value: "gpt-5.4" },
        { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
      ],
    });
    const { rerender } = render(
      renderFooter({
        session: sessionA,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });

    expect(screen.getByRole("option", { name: /gpt-5\.3-codex/i })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    rerender(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.3-codex",
          modelOptions: [
            { label: "gpt-5.4", value: "gpt-5.4" },
            { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
          ],
        }),
      }),
    );

    await waitFor(() => {
      expect(screen.getByRole("option", { name: /gpt-5\.3-codex/i })).toHaveAttribute(
        "aria-selected",
        "true",
      );
      expect(screen.getByRole("option", { name: /gpt-5\.4/i })).toHaveAttribute(
        "aria-selected",
        "false",
      );
    });
  });

  it("limits /effort choices to the selected Codex model capabilities", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5-codex-mini",
          modelOptions: [
            {
              label: "GPT-5 Codex Mini",
              value: "gpt-5-codex-mini",
              description: "Optimized for codex. Cheaper, faster, but less capable.",
              defaultReasoningEffort: "medium",
              supportedReasoningEfforts: ["medium", "high"],
            },
          ],
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/effort" },
    });

    expect(screen.getByText(/GPT-5 Codex Mini supports medium, high\./)).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /medium/i })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /high/i })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /minimal/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /^low/i })).not.toBeInTheDocument();
  });

  it("applies Gemini mode changes from /mode", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Gemini",
          geminiApprovalMode: "default",
          model: "auto",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/mode" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "geminiApprovalMode",
      "yolo",
    );
  });

  it("requests live Codex model options when /model opens", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: undefined,
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });

  it("requests live Cursor model options when /model opens", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Cursor",
          cursorMode: "agent",
          model: "auto",
          modelOptions: undefined,
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });

  it("shows inline model refresh errors and retries from the slash menu", () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        modelOptionsError: "Cursor auth is not configured.",
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Cursor",
          cursorMode: "agent",
          model: "auto",
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    expect(screen.getByRole("alert")).toHaveTextContent("Cursor auth is not configured.");

    fireEvent.click(screen.getByRole("button", { name: "Retry live models" }));

    expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
  });
});
