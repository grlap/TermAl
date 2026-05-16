// Owns AgentSessionPanel footer and composer behavior tests.
// Does not own transcript virtualization or message-list rendering tests.
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

function renderFooter({
  isPaneActive = true,
  session,
  committedDraft = "",
  isSending = false,
  isStopping = false,
  isSessionBusy = false,
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
  canSpawnDelegation = false,
  onSpawnDelegation,
  onSessionSettingsChange = vi.fn(),
  onScrollToLatest = vi.fn(),
  onDraftAttachmentRemove = vi.fn(),
  onStopSession = vi.fn(),
  onPaste = vi.fn(),
}: {
  isPaneActive?: boolean;
  session: Session | null;
  committedDraft?: string;
  isSending?: boolean;
  isStopping?: boolean;
  isSessionBusy?: boolean;
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
  canSpawnDelegation?: boolean;
  onSpawnDelegation?: (sessionId: string, prompt: string) => Promise<boolean>;
  onSessionSettingsChange?: (sessionId: string, field: string, value: string) => void;
  onScrollToLatest?: () => void;
  onDraftAttachmentRemove?: (sessionId: string, attachmentId: string) => void;
  onStopSession?: (sessionId: string) => void;
  onPaste?: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
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
      isSending={isSending}
      isStopping={isStopping}
      isSessionBusy={isSessionBusy}
      isUpdating={isUpdating}
      showNewResponseIndicator={false}
      newResponseIndicatorLabel="New response"
      footerModeLabel="Session"
      onScrollToLatest={onScrollToLatest}
      onDraftCommit={onDraftCommit}
      onDraftAttachmentRemove={onDraftAttachmentRemove}
      isRefreshingModelOptions={false}
      modelOptionsError={modelOptionsError}
      agentCommands={agentCommands}
      hasLoadedAgentCommands={hasLoadedAgentCommands}
      isRefreshingAgentCommands={isRefreshingAgentCommands}
      agentCommandsError={agentCommandsError}
      onRefreshSessionModelOptions={onRefreshSessionModelOptions}
      onRefreshAgentCommands={onRefreshAgentCommands}
      onSend={onSend}
      canSpawnDelegation={canSpawnDelegation}
      onSpawnDelegation={onSpawnDelegation}
      onSessionSettingsChange={onSessionSettingsChange}
      onStopSession={onStopSession}
      onPaste={onPaste}
    />
  );
}

function deferredValue<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function recordTextareaHeightWrites(
  textarea: HTMLTextAreaElement,
  writes: { value: string; transition: string }[],
) {
  const style = textarea.style;
  const originalHeightDescriptor = Object.getOwnPropertyDescriptor(
    style,
    "height",
  );
  Object.defineProperty(style, "height", {
    configurable: true,
    get() {
      return style.getPropertyValue("height");
    },
    set(value: string) {
      writes.push({ value, transition: style.transition });
      style.setProperty("height", value);
    },
  });

  return () => {
    if (originalHeightDescriptor) {
      Object.defineProperty(style, "height", originalHeightDescriptor);
    } else {
      delete (style as { height?: string }).height;
    }
  };
}

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

  it("marks the real composer input for overview focus deferral", () => {
    render(renderFooter({ session: makeSession("session-a") }));

    expect(screen.getByLabelText("Message session-a")).toHaveAttribute(
      "data-conversation-composer-input",
      "true",
    );
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

  it("spawns a delegation from the current draft without sending it", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    render(
      renderFooter({
        session: makeSession("session-a"),
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, {
      target: { value: "  Review the staged frontend change.  " },
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith(
      "session-a",
      "Review the staged frontend change.",
    );
    expect(onSend).not.toHaveBeenCalled();
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("keeps the draft when delegation spawn is rejected", async () => {
    const onSpawnDelegation = vi.fn(async () => false);
    render(
      renderFooter({
        session: makeSession("session-a"),
        canSpawnDelegation: true,
        onSpawnDelegation,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, {
      target: { value: "Review this before I send it." },
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith(
      "session-a",
      "Review this before I send it.",
    );
    expect(textarea).toHaveValue("Review this before I send it.");
  });

  it("hides the delegation action when local delegation is unavailable", () => {
    render(
      renderFooter({
        session: makeSession("session-a"),
        canSpawnDelegation: false,
        onSpawnDelegation: vi.fn(async () => true),
      }),
    );

    expect(screen.queryByRole("button", { name: "Delegate" })).not.toBeInTheDocument();
  });

  it("refreshes the delegation action when availability changes", () => {
    const session = makeSession("session-a");
    const stableProps = {
      onDraftAttachmentRemove: vi.fn(),
      onDraftCommit: vi.fn(),
      onPaste: vi.fn(),
      onRefreshAgentCommands: vi.fn(),
      onRefreshSessionModelOptions: vi.fn(),
      onScrollToLatest: vi.fn(),
      onSend: vi.fn(() => true),
      onSessionSettingsChange: vi.fn(),
      onStopSession: vi.fn(),
      onSpawnDelegation: vi.fn(async () => true),
    };
    const { rerender } = render(
      renderFooter({
        session,
        canSpawnDelegation: false,
        ...stableProps,
      }),
    );

    expect(screen.queryByRole("button", { name: "Delegate" })).not.toBeInTheDocument();

    rerender(
      renderFooter({
        session,
        canSpawnDelegation: true,
        ...stableProps,
      }),
    );

    expect(screen.getByRole("button", { name: "Delegate" })).toBeInTheDocument();
  });

  it("uses the latest delegation handler after rerender", async () => {
    const session = makeSession("session-a");
    const initialSpawn = vi.fn(async () => true);
    const latestSpawn = vi.fn(async () => true);
    const stableProps = {
      canSpawnDelegation: true,
      onDraftAttachmentRemove: vi.fn(),
      onDraftCommit: vi.fn(),
      onPaste: vi.fn(),
      onRefreshAgentCommands: vi.fn(),
      onRefreshSessionModelOptions: vi.fn(),
      onScrollToLatest: vi.fn(),
      onSend: vi.fn(() => true),
      onSessionSettingsChange: vi.fn(),
      onStopSession: vi.fn(),
    };
    const { rerender } = render(
      renderFooter({
        session,
        onSpawnDelegation: initialSpawn,
        ...stableProps,
      }),
    );
    rerender(
      renderFooter({
        session,
        onSpawnDelegation: latestSpawn,
        ...stableProps,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, {
      target: { value: "Use the latest delegation handler." },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(initialSpawn).not.toHaveBeenCalled();
    expect(latestSpawn).toHaveBeenCalledWith(
      "session-a",
      "Use the latest delegation handler.",
    );
  });

  it("marks the delegation action busy while the spawn is in flight", async () => {
    const pendingSpawn = deferredValue<boolean>();
    const onSpawnDelegation = vi.fn(() => pendingSpawn.promise);
    render(
      renderFooter({
        session: makeSession("session-a"),
        canSpawnDelegation: true,
        onSpawnDelegation,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, {
      target: { value: "Review while I keep typing." },
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
    });

    const busyButton = await screen.findByRole("button", {
      name: "Delegating...",
    });
    expect(busyButton).toBeDisabled();
    expect(busyButton).not.toHaveAttribute("aria-busy");
    expect(busyButton).toHaveAttribute(
      "title",
      "Spawn read-only delegation from current draft",
    );

    await act(async () => {
      pendingSpawn.resolve(true);
      await pendingSpawn.promise;
    });
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("keeps the draft when delegation spawn throws", async () => {
    const onSpawnDelegation = vi.fn(async () => {
      throw new Error("spawn failed");
    });
    render(
      renderFooter({
        session: makeSession("session-a"),
        canSpawnDelegation: true,
        onSpawnDelegation,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, {
      target: { value: "Keep this if spawn rejects." },
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith(
      "session-a",
      "Keep this if spawn rejects.",
    );
    expect(textarea).toHaveValue("Keep this if spawn rejects.");
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Delegating..." }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "Delegate" })).toBeEnabled();
  });

  it("disables delegation for session-control slash palette choices", () => {
    const onSpawnDelegation = vi.fn(async () => true);
    render(
      renderFooter({
        session: makeSession("session-a", {
          modelOptions: [{ label: "gpt-5.4", value: "gpt-5.4" }],
        }),
        committedDraft: "/model",
        canSpawnDelegation: true,
        onSpawnDelegation,
      }),
    );

    const delegateButton = screen.getByRole("button", { name: "Delegate" });
    expect(
      screen.getByRole("listbox", { name: "Codex models" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /gpt-5\.4/ })).toBeInTheDocument();
    expect(delegateButton).toBeDisabled();
    fireEvent.click(delegateButton);
    expect(onSpawnDelegation).not.toHaveBeenCalled();
  });

  it("delegates a selected prompt-template agent command with expanded content", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "review-local",
      source: ".claude/commands/review-local.md",
      kind: "promptTemplate",
      visiblePrompt: "/review-local",
      expandedPrompt: "Expanded review command body.",
      title: "Review staged and unstaged changes.",
      delegation: {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    });
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    expect(
      screen.getByRole("option", { name: /\/review-local/ }),
    ).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith(
      "session-a",
      "Expanded review command body.",
      {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    );
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "",
      intent: "delegate",
    });
    expect(onSend).not.toHaveBeenCalled();
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("lets keyboard users tab to Delegate for active agent slash commands", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    stubResolvedAgentCommand({
      name: "review-local",
      source: ".claude/commands/review-local.md",
      kind: "promptTemplate",
      visiblePrompt: "/review-local",
      expandedPrompt: "Expanded review command body.",
      title: "Review staged and unstaged changes.",
      delegation: {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    });
    render(
      renderFooter({
        isSessionBusy: true,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    await user.click(textarea);
    await user.keyboard("/rev");
    expect(
      screen.getByRole("option", { name: /\/review-local/ }),
    ).toBeInTheDocument();
    const hint = screen.getByText(/Tab moves focus to Delegate\./);
    expect(textarea).toHaveAttribute("aria-describedby", hint.id);
    const stopButton = screen.getByRole("button", { name: "Stop" });

    await user.tab();

    const delegateButton = screen.getByRole("button", { name: "Delegate" });
    expect(delegateButton).toHaveFocus();
    expect(stopButton).not.toHaveFocus();
    expect(onSend).not.toHaveBeenCalled();
    expect(onSpawnDelegation).not.toHaveBeenCalled();

    await user.keyboard("{Enter}");

    await waitFor(() =>
      expect(onSpawnDelegation).toHaveBeenCalledWith(
        "session-a",
        "Expanded review command body.",
        {
          mode: "reviewer",
          title: "Review staged and unstaged changes.",
          writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
        },
      ),
    );
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not send or delegate when Shift+Tab is pressed on a slash command", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "review-local",
      source: ".claude/commands/review-local.md",
      kind: "promptTemplate",
      visiblePrompt: "/review-local",
      expandedPrompt: "Expanded review command body.",
      title: "Review staged and unstaged changes.",
      delegation: {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    });
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    expect(
      screen.getByRole("option", { name: /\/review-local/ }),
    ).toBeInTheDocument();

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
      await Promise.resolve();
    });

    expect(fetchMock).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    expect(onSpawnDelegation).not.toHaveBeenCalled();
  });

  it("keeps Tab as send when delegation is unavailable for active agent slash commands", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "review-local",
      source: "Claude project command",
      kind: "nativeSlash",
      visiblePrompt: "/review-local",
      title: "Review staged and unstaged changes.",
    });
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
        canSpawnDelegation: false,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    expect(
      screen.getByRole("option", { name: /\/review-local/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Tab moves focus to Delegate\./),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Delegate" }),
    ).not.toBeInTheDocument();

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Tab" });
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("session-a", "/review-local"),
    );
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "",
      intent: "send",
    });
  });

  it("does not tab to Delegate while a delegatable slash command is temporarily disabled", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "review-local",
      source: ".claude/commands/review-local.md",
      kind: "promptTemplate",
      visiblePrompt: "/review-local",
      expandedPrompt: "Expanded review command body.",
      title: "Review staged and unstaged changes.",
      delegation: {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    });
    render(
      renderFooter({
        isUpdating: true,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    expect(
      screen.getByRole("option", { name: /\/review-local/ }),
    ).toBeInTheDocument();
    const delegateButton = screen.getByRole("button", { name: "Delegate" });
    expect(delegateButton).toBeDisabled();
    expect(
      screen.queryByText(/Tab moves focus to Delegate\./),
    ).not.toBeInTheDocument();

    const tabEvent = createEvent.keyDown(textarea, { key: "Tab" });
    await act(async () => {
      fireEvent(textarea, tabEvent);
      await Promise.resolve();
    });

    expect(tabEvent.defaultPrevented).toBe(false);
    expect(delegateButton).not.toHaveFocus();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    expect(onSpawnDelegation).not.toHaveBeenCalled();
  });

  it("surfaces resolver validation errors without clearing the composer draft", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = vi.fn(async () => {
      return new Response(
        JSON.stringify({
          error: "Native slash command notes are not supported.",
        }),
        {
          status: 400,
          headers: { "Content-Type": "application/json" },
        },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
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
    fireEvent.change(textarea, {
      target: { value: "/review-local -- include staged and unstaged files" },
    });

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Native slash command notes are not supported.",
    );
    expect(screen.getByRole("alert").closest(".composer-slash-menu")).not.toBeNull();
    expect(textarea).toHaveValue(
      "/review-local -- include staged and unstaged files",
    );
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "",
      intent: "send",
      note: "include staged and unstaged files",
    });
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.change(textarea, { target: { value: "/review-local" } });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("redacts resolver validation errors that include local paths", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = vi.fn(async () => {
      return new Response(
        JSON.stringify({
          error:
            "failed to read config=/workspace/secrets/review-local.md token=secret",
        }),
        {
          status: 400,
          headers: { "Content-Type": "application/json" },
        },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/review-local" } });

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "Could not resolve the slash command. Check the command file and try again.",
    );
    expect(document.body).not.toHaveTextContent("/workspace/secrets");
    expect(document.body).not.toHaveTextContent("token=secret");
    expect(document.body).not.toHaveTextContent("review-local.md");
    expect(onSend).not.toHaveBeenCalled();
  });

  it("clears resolver errors after selecting another slash command", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = vi.fn(async () => {
      return new Response(
        JSON.stringify({
          error: "Native slash command notes are not supported.",
        }),
        {
          status: 400,
          headers: { "Content-Type": "application/json" },
        },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
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
          {
            kind: "nativeSlash",
            name: "review-fix",
            description: "Review a fix.",
            content: "/review-fix",
            source: "Claude project command",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, {
      target: { value: "/rev -- include staged and unstaged files" },
    });

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Native slash command notes are not supported.",
    );

    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: " ", code: "Space" });

    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(textarea).toHaveValue("/review-fix ");
    expect(onSend).not.toHaveBeenCalled();
  });

  it("redacts internal resolver failures before displaying them", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = vi.fn(async () => {
      return new Response(
        JSON.stringify({
          error:
            "failed to read agent command metadata C:\\secret\\project\\.claude\\commands\\review-local.md",
        }),
        {
          status: 500,
          headers: { "Content-Type": "application/json" },
        },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/review-local" } });

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "Could not resolve the slash command. Check the command file and try again.",
    );
    expect(document.body).not.toHaveTextContent("C:\\secret");
    expect(document.body).not.toHaveTextContent("review-local.md");
    expect(textarea).toHaveValue("/review-local");
    expect(onSend).not.toHaveBeenCalled();
  });

  it("surfaces backend-unavailable resolver failures without leaking details or spawning", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const fetchMock = vi.fn(async () => {
      throw new Error("token=secret C:\\internal\\backend.log");
    });
    vi.stubGlobal("fetch", fetchMock);
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Expanded review command body.",
            source: ".claude/commands/review-local.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "Could not resolve the slash command. Check the command file and try again.",
    );
    expect(alert).not.toHaveTextContent("token=secret");
    expect(alert).not.toHaveTextContent("backend.log");
    expect(document.body).not.toHaveTextContent("token=secret");
    expect(document.body).not.toHaveTextContent("backend.log");
    expect(textarea).toHaveValue("/rev");
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(onSpawnDelegation).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
  });

  it("delegates native slash agent commands as slash prompts", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    stubResolvedAgentCommand({
      name: "review",
      source: "Claude bundled command",
      kind: "nativeSlash",
      visiblePrompt: "/review",
      title: "Review the current changes.",
    });
    render(
      renderFooter({
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
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith("session-a", "/review", {
      title: "Review the current changes.",
    });
    expect(onSend).not.toHaveBeenCalled();
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("forwards resolver-provided delegation mode to composer delegation", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    stubResolvedAgentCommand({
      name: "explore",
      source: ".claude/commands/explore.md",
      kind: "promptTemplate",
      visiblePrompt: "/explore",
      expandedPrompt: "Explore the resolver path.",
      title: "Explore resolver path",
      delegation: {
        mode: "explorer",
        title: "Explore resolver path",
      },
    });
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "explore",
            description: "Explore the resolver path.",
            content: "Explore the resolver path.",
            source: ".claude/commands/explore.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/exp" } });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith(
      "session-a",
      "Explore the resolver path.",
      {
        mode: "explorer",
        title: "Explore resolver path",
      },
    );
    expect(onSend).not.toHaveBeenCalled();
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("ignores duplicate delegated agent commands while resolution is pending", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const pendingResolve = deferredValue<Response>();
    const fetchMock = vi.fn(() => pendingResolve.promise);
    vi.stubGlobal("fetch", fetchMock);
    render(
      renderFooter({
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
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    const delegateButton = screen.getByRole("button", { name: "Delegate" });
    await act(async () => {
      fireEvent.click(delegateButton);
      fireEvent.click(delegateButton);
      await Promise.resolve();
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(onSpawnDelegation).not.toHaveBeenCalled();
    expect(delegateButton).toBeDisabled();
    expect(textarea).toBeDisabled();

    await act(async () => {
      pendingResolve.resolve(
        new Response(
          JSON.stringify({
            name: "review-local",
            source: "Claude project command",
            kind: "nativeSlash",
            visiblePrompt: "/review-local",
            title: "Review staged and unstaged changes.",
            delegation: {
              mode: "reviewer",
              title: "Review staged and unstaged changes.",
              writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      );
      await pendingResolve.promise;
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(onSpawnDelegation).toHaveBeenCalledWith("session-a", "/review-local", {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      }),
    );
    expect(onSpawnDelegation).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("drops resolved delegated agent commands after a session switch", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const onDraftCommit = vi.fn();
    const pendingResolve = deferredValue<Response>();
    const fetchMock = vi.fn(() => pendingResolve.promise);
    vi.stubGlobal("fetch", fetchMock);
    const { rerender } = render(
      renderFooter({
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
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
        onDraftCommit,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(onSpawnDelegation).not.toHaveBeenCalled();

    await act(async () => {
      rerender(
        renderFooter({
          session: makeSession("session-b"),
          canSpawnDelegation: true,
          onSpawnDelegation,
          onSend,
          onDraftCommit,
        }),
      );
      await Promise.resolve();
    });

    await act(async () => {
      pendingResolve.resolve(
        new Response(
          JSON.stringify({
            name: "review-local",
            source: "Claude project command",
            kind: "nativeSlash",
            visiblePrompt: "/review-local",
            title: "Review staged and unstaged changes.",
            delegation: {
              mode: "reviewer",
              title: "Review staged and unstaged changes.",
              writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      );
      await pendingResolve.promise;
      await Promise.resolve();
    });

    expect(onSpawnDelegation).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    expect(onDraftCommit).toHaveBeenCalledWith("session-a", "/rev");
    expect(
      onDraftCommit.mock.calls.filter(([sessionId]) => sessionId === "session-b"),
    ).toEqual([]);
  });

  it("does not focus the composer when delegated command resolution rejects after a session switch", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    const pendingResolve = deferredValue<Response>();
    const fetchMock = vi.fn(() => pendingResolve.promise);
    vi.stubGlobal("fetch", fetchMock);
    const { rerender } = render(
      renderFooter({
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
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    await act(async () => {
      rerender(
        renderFooter({
          session: makeSession("session-b"),
          canSpawnDelegation: true,
          onSpawnDelegation,
          onSend,
        }),
      );
      await Promise.resolve();
    });
    const sessionBTextarea = screen.getByLabelText("Message session-b");
    const focusSpy = vi.spyOn(sessionBTextarea, "focus");

    await act(async () => {
      pendingResolve.reject(new Error("resolver unavailable"));
      await pendingResolve.promise.catch(() => undefined);
      await Promise.resolve();
    });

    expect(focusSpy).not.toHaveBeenCalled();
    expect(onSpawnDelegation).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
  });

  it("delegates review-local native slash commands in an isolated worktree", async () => {
    const onSend = vi.fn(() => true);
    const onSpawnDelegation = vi.fn(async () => true);
    stubResolvedAgentCommand({
      name: "review-local",
      source: ".claude/commands/review-local.md",
      kind: "nativeSlash",
      visiblePrompt: "/review-local",
      title: "Review staged and unstaged changes.",
      delegation: {
        mode: "reviewer",
        title: "Review staged and unstaged changes.",
        writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
      },
    });
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "nativeSlash",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "/review-local",
            source: ".claude/commands/review-local.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
        onSend,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith("session-a", "/review-local", {
      mode: "reviewer",
      title: "Review staged and unstaged changes.",
      writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
    });
    expect(onSend).not.toHaveBeenCalled();
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("expands argument-taking agent commands before delegation", async () => {
    const onSpawnDelegation = vi.fn(async () => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "fix-bug",
      source: ".claude/commands/fix-bug.md",
      kind: "promptTemplate",
      visiblePrompt: "/fix-bug 42",
      expandedPrompt: "Fix:\n42",
      title: "Fix bug 42",
    });
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "fix-bug",
            description: "Fix a bug.",
            content: "Fix:\n$ARGUMENTS",
            source: ".claude/commands/fix-bug.md",
          },
        ],
        canSpawnDelegation: true,
        onSpawnDelegation,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/fix" } });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).not.toHaveBeenCalled();
    expect(textarea).toHaveValue("/fix-bug ");

    fireEvent.change(textarea, { target: { value: "/fix-bug 42" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    expect(onSpawnDelegation).toHaveBeenCalledWith("session-a", "Fix:\n42", {
      title: "Fix bug 42",
    });
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "42",
      intent: "delegate",
    });
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("does not clear the original draft when a delegation resolves after a session switch", async () => {
    const pendingSpawn = deferredValue<boolean>();
    const onSpawnDelegation = vi.fn(() => pendingSpawn.promise);
    const onDraftCommit = vi.fn();
    const { rerender } = render(
      renderFooter({
        session: makeSession("session-a"),
        canSpawnDelegation: true,
        onSpawnDelegation,
        onDraftCommit,
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "Review after switch." },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
      await Promise.resolve();
    });

    await act(async () => {
      rerender(
        renderFooter({
          session: makeSession("session-b"),
          canSpawnDelegation: true,
          onSpawnDelegation,
          onDraftCommit,
        }),
      );
      await Promise.resolve();
    });
    await act(async () => {
      pendingSpawn.resolve(true);
      await pendingSpawn.promise;
    });

    expect(onDraftCommit).toHaveBeenCalledWith(
      "session-a",
      "Review after switch.",
    );
    expect(onDraftCommit).not.toHaveBeenCalledWith("session-a", "");
    expect(onDraftCommit).not.toHaveBeenCalledWith("session-b", "");
  });

  it("ignores delegation completion after the footer unmounts", async () => {
    const pendingSpawn = deferredValue<boolean>();
    const onDraftCommit = vi.fn();
    const focusSpy = vi
      .spyOn(HTMLTextAreaElement.prototype, "focus")
      .mockImplementation(() => {});
    const consoleErrorSpy = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    try {
      const { unmount } = render(
        renderFooter({
          session: makeSession("session-a"),
          canSpawnDelegation: true,
          onSpawnDelegation: vi.fn(() => pendingSpawn.promise),
          onDraftCommit,
        }),
      );

      fireEvent.change(screen.getByLabelText("Message session-a"), {
        target: { value: "Unmount before completion." },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Delegate" }));
        await Promise.resolve();
      });

      focusSpy.mockClear();
      unmount();
      await act(async () => {
        pendingSpawn.resolve(true);
        await pendingSpawn.promise;
      });

      expect(onDraftCommit).not.toHaveBeenCalledWith("session-a", "");
      expect(focusSpy).not.toHaveBeenCalled();
      expect(
        consoleErrorSpy.mock.calls
          .map((args) => args.map(String).join(" "))
          .filter((message) => /act|unmount/i.test(message)),
      ).toEqual([]);
    } finally {
      focusSpy.mockRestore();
      consoleErrorSpy.mockRestore();
    }
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

    act(() => {
      fireEvent.change(screen.getByLabelText("Message session-a"), {
        target: { value: "carry this draft" },
      });
    });

    act(() => {
      rerender(
        renderFooter({
          onDraftCommit,
          session: makeSession("session-b"),
        }),
      );
    });

    expect(onDraftCommit).toHaveBeenCalledWith("session-a", "carry this draft");
  });

  it("does not schedule a redundant composer autosize frame when switching sessions", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const requestAnimationFrameMock = vi.fn((callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    const cancelAnimationFrameMock = vi.fn();

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;

    try {
      const { rerender } = render(
        renderFooter({
          committedDraft: "first draft",
          isPaneActive: false,
          session: makeSession("session-a"),
        }),
      );

      requestAnimationFrameMock.mockClear();

      act(() => {
        rerender(
          renderFooter({
            committedDraft: "second draft",
            isPaneActive: false,
            session: makeSession("session-b"),
          }),
        );
      });

      expect(screen.getByLabelText("Message session-b")).toHaveValue(
        "second draft",
      );
      expect(requestAnimationFrameMock).toHaveBeenCalledTimes(1);
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
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
    let sawZeroHeightProbe = false;
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
        if (textarea.style.height === "0px") {
          sawZeroHeightProbe = true;
        }
        if (
          !textarea.value.includes("line two") &&
          textarea.value.length > 0 &&
          textarea.style.transition !== "none"
        ) {
          return 124;
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
      expect(sawZeroHeightProbe).toBe(false);
      expect(textarea.style.transition).not.toBe("none");
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

  it("keeps multiline composer height steady when deleting text inside a line", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalScrollHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "scrollHeight",
    );
    const originalOffsetHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetHeight",
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
    const heightWrites: { value: string; transition: string }[] = [];
    let offsetHeightReads = 0;
    const drainAnimationFrames = () => {
      while (queuedFrames.size > 0) {
        const callbacks = [...queuedFrames.values()];
        queuedFrames.clear();
        act(() => {
          callbacks.forEach((callback) => callback(0));
        });
      }
    };
    let unmount: ReturnType<typeof render>["unmount"] | null = null;
    let restoreHeightWrites: (() => void) | null = null;

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        return 40 + (textarea.value.split("\n").length - 1) * 28;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get() {
        offsetHeightReads += 1;
        return Number.parseFloat((this as HTMLElement).style.height) || 0;
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
      restoreHeightWrites = recordTextareaHeightWrites(textarea, heightWrites);

      act(() => {
        fireEvent.change(textarea, {
          target: { value: "line one\nline two\nline three" },
        });
      });
      drainAnimationFrames();
      expect(textarea.style.height).toBe("96px");

      heightWrites.length = 0;
      act(() => {
        fireEvent.change(textarea, {
          target: { value: "line one\nline tw\nline three" },
        });
      });
      drainAnimationFrames();

      expect(textarea.style.height).toBe("96px");
      expect(heightWrites).toEqual([
        {
          value: "1px",
          transition: "none",
        },
        {
          value: "96px",
          transition: "none",
        },
      ]);
      expect(offsetHeightReads).toBe(0);
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
      if (originalOffsetHeightDescriptor) {
        Object.defineProperty(
          HTMLElement.prototype,
          "offsetHeight",
          originalOffsetHeightDescriptor,
        );
      } else {
        delete (
          HTMLElement.prototype as unknown as {
            offsetHeight?: number;
          }
        ).offsetHeight;
      }
      restoreHeightWrites?.();
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("shrinks multiline composer height after an accepted send", () => {
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
    const onSend = vi.fn(() => true);
    let unmount: ReturnType<typeof render>["unmount"] | null = null;
    let sawZeroHeightProbe = false;

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        if (textarea.style.height === "0px") {
          sawZeroHeightProbe = true;
        }
        if (
          textarea.value === "" &&
          textarea.style.height === "96px" &&
          textarea.style.transition !== "none"
        ) {
          return 96;
        }
        return 40 + (textarea.value.split("\n").length - 1) * 28;
      },
    });

    try {
      ({ unmount } = render(
        renderFooter({
          onSend,
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

      act(() => {
        fireEvent.click(screen.getByRole("button", { name: "Send" }));
      });
      drainAnimationFrames();

      expect(onSend).toHaveBeenCalledWith(
        "session-a",
        "line one\nline two\nline three",
      );
      expect(textarea).toHaveValue("");
      expect(textarea.style.height).toBe("40px");
      expect(sawZeroHeightProbe).toBe(false);
      expect(textarea.style.transition).not.toBe("none");
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

  it("restores composer transition when typing before send-shrink restore fires", () => {
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
    const flushNextAnimationFrameBatch = () => {
      const callbacks = [...queuedFrames.values()];
      queuedFrames.clear();
      act(() => {
        callbacks.forEach((callback) => callback(0));
      });
    };
    const flushNewestAnimationFrame = () => {
      const newestFrameId = Math.max(...queuedFrames.keys());
      const callback = queuedFrames.get(newestFrameId);
      if (!callback) {
        throw new Error("Expected a queued animation frame");
      }
      queuedFrames.delete(newestFrameId);
      act(() => {
        callback(0);
      });
    };
    const onSend = vi.fn(() => true);
    let unmount: ReturnType<typeof render>["unmount"] | null = null;

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        return 40 + (textarea.value.split("\n").length - 1) * 28;
      },
    });

    try {
      ({ unmount } = render(
        renderFooter({
          onSend,
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
      textarea.style.transition = "height 150ms ease";

      act(() => {
        fireEvent.click(screen.getByRole("button", { name: "Send" }));
      });
      flushNextAnimationFrameBatch();

      expect(textarea).toHaveValue("");
      expect(textarea.style.transition).toBe("none");
      expect(queuedFrames.size).toBeGreaterThan(0);

      act(() => {
        fireEvent.change(textarea, {
          target: { value: "new draft" },
        });
      });
      flushNewestAnimationFrame();

      expect(textarea).toHaveValue("new draft");
      expect(textarea.style.transition).toBe("height 150ms ease");
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

  it("restores composer transition when switching sessions before send-shrink restore fires", () => {
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
    const flushNextAnimationFrameBatch = () => {
      const callbacks = [...queuedFrames.values()];
      queuedFrames.clear();
      act(() => {
        callbacks.forEach((callback) => callback(0));
      });
    };
    const onSend = vi.fn(() => true);
    let unmount: ReturnType<typeof render>["unmount"] | null = null;

    window.requestAnimationFrame =
      requestAnimationFrameMock as unknown as typeof requestAnimationFrame;
    window.cancelAnimationFrame =
      cancelAnimationFrameMock as unknown as typeof cancelAnimationFrame;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const textarea = this as HTMLTextAreaElement;
        return 40 + (textarea.value.split("\n").length - 1) * 28;
      },
    });

    try {
      const view = render(
        renderFooter({
          onSend,
          session: makeSession("session-a"),
        }),
      );
      unmount = view.unmount;
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
      textarea.style.transition = "height 150ms ease";

      act(() => {
        fireEvent.click(screen.getByRole("button", { name: "Send" }));
      });
      flushNextAnimationFrameBatch();

      expect(textarea.style.transition).toBe("none");
      expect(queuedFrames.size).toBeGreaterThan(0);

      act(() => {
        view.rerender(
          renderFooter({
            session: makeSession("session-b"),
          }),
        );
      });

      expect(textarea.style.transition).toBe("height 150ms ease");
      const nextTextarea = screen.getByLabelText("Message session-b");
      if (!(nextTextarea instanceof HTMLTextAreaElement)) {
        throw new Error("Composer textarea not found after session switch");
      }
      expect(nextTextarea.style.transition).toBe("height 150ms ease");
      drainAnimationFrames();
      expect(nextTextarea.style.transition).toBe("height 150ms ease");
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
    let sawZeroHeightProbe = false;

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
        if (textarea.style.height === "0px") {
          sawZeroHeightProbe = true;
        }
        if (!isWide) {
          return 96;
        }
        return textarea.style.transition === "none" ? 40 : 96;
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
      expect(sawZeroHeightProbe).toBe(false);
      expect(textarea.style.transition).not.toBe("none");
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

    act(() => {
      fireEvent.change(screen.getByLabelText("Message session-a"), {
        target: { value: "/" },
      });
    });
    expect(onRefreshAgentCommands).not.toHaveBeenCalled();

    act(() => {
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
    });

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

  it("sends a no-argument agent command directly from the slash menu", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "review-local",
      source: "Claude project command",
      kind: "nativeSlash",
      visiblePrompt: "/review-local",
      title: "Review staged and unstaged changes.",
    });

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
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("session-a", "/review-local"),
    );
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "",
      intent: "send",
    });
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("ignores duplicate agent command sends while resolution is pending", async () => {
    const onSend = vi.fn(() => true);
    const pendingResolve = deferredValue<Response>();
    const fetchMock = vi.fn(() => pendingResolve.promise);
    vi.stubGlobal("fetch", fetchMock);
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
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    await act(async () => {
      pendingResolve.resolve(
        new Response(
          JSON.stringify({
            name: "review-local",
            source: "Claude project command",
            kind: "nativeSlash",
            visiblePrompt: "/review-local",
            title: "Review staged and unstaged changes.",
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      );
      await pendingResolve.promise;
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("session-a", "/review-local"),
    );
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("does not focus the composer when send command resolution rejects after a session switch", async () => {
    const onSend = vi.fn(() => true);
    const pendingResolve = deferredValue<Response>();
    const fetchMock = vi.fn(() => pendingResolve.promise);
    vi.stubGlobal("fetch", fetchMock);
    const { rerender } = render(
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
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    await act(async () => {
      rerender(
        renderFooter({
          onSend,
          session: makeSession("session-b"),
        }),
      );
      await Promise.resolve();
    });
    const sessionBTextarea = screen.getByLabelText("Message session-b");
    const focusSpy = vi.spyOn(sessionBTextarea, "focus");

    await act(async () => {
      pendingResolve.reject(new Error("resolver unavailable"));
      await pendingResolve.promise.catch(() => undefined);
      await Promise.resolve();
    });

    expect(focusSpy).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
  });

  it("expands a selected skill command on Space instead of running it", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "imagegen",
            description: "Use the image generation skill.",
            content: "Use the image generation skill.",
            source: "Skill",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/ima" } });
    fireEvent.keyDown(textarea, { key: "Space", code: "Space" });

    expect(onSend).not.toHaveBeenCalled();
    expect(textarea).toHaveValue("/imagegen ");
  });

  it("leaves Space as parameter input after a skill command is expanded", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "imagegen",
            description: "Use the image generation skill.",
            content: "Use the image generation skill.",
            source: "Skill",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/imagegen detailed prompt" } });

    expect(fireEvent.keyDown(textarea, { key: "Space", code: "Space" })).toBe(
      true,
    );
    expect(onSend).not.toHaveBeenCalled();
    expect(textarea).toHaveValue("/imagegen detailed prompt");
  });

  it("expands an agent command with $ARGUMENTS and sends the substituted prompt", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "fix-bug",
      source: ".claude/commands/fix-bug.md",
      kind: "promptTemplate",
      visiblePrompt: "/fix-bug 3",
      expandedPrompt: `Fix the requested bug:

3

Verify the fix.

## Additional User Note

Please add tests.`,
      title: "Fix bug 3",
    });

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
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });
    expect(textarea).toHaveValue("/fix-bug ");

    fireEvent.change(textarea, {
      target: { value: "/fix-bug 3 -- Please add tests." },
    });
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith(
        "session-a",
        "/fix-bug 3",
        `Fix the requested bug:

3

Verify the fix.

## Additional User Note

Please add tests.`,
      ),
    );
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "3",
      note: "Please add tests.",
      intent: "send",
    });
    await waitFor(() => expect(textarea).toHaveValue(""));
  });

  it("expands a native Claude command with arguments and sends the slash prompt", async () => {
    const onSend = vi.fn(() => true);
    const fetchMock = stubResolvedAgentCommand({
      name: "review",
      source: "Claude bundled command",
      kind: "nativeSlash",
      visiblePrompt: "/review staged files",
      title: "Review the current changes.",
    });

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
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });
    expect(textarea).toHaveValue("/review ");
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.change(textarea, { target: { value: "/review staged files" } });
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter" });
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("session-a", "/review staged files"),
    );
    expect(lastJsonRequestBody(fetchMock)).toEqual({
      arguments: "staged files",
      intent: "send",
    });
    await waitFor(() => expect(textarea).toHaveValue(""));
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
    act(() => {
      fireEvent.change(textarea, { target: { value: "/model" } });
      fireEvent.keyDown(textarea, { key: "ArrowDown" });
    });
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

    act(() => {
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
    });

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
