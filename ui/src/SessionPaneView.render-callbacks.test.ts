import { fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  cancelDelegationCommand,
  getDelegationResultCommand,
  getDelegationStatusCommand,
} from "./delegation-commands";
import {
  shouldPreferStreamingAssistantTextRender,
  streamingAssistantTextMessageIdForSession,
  useSessionRenderCallbacks,
} from "./SessionPaneView.render-callbacks";
import type { Message, Session } from "./types";

vi.mock("./delegation-commands", () => ({
  cancelDelegationCommand: vi.fn(),
  getDelegationResultCommand: vi.fn(),
  getDelegationStatusCommand: vi.fn(),
}));

function makeSession(
  status: Session["status"],
  messages: Message[],
): Session {
  return {
    id: "session-1",
    name: "Session",
    emoji: "",
    agent: "Codex",
    workdir: "/repo",
    model: "gpt-5.5",
    status,
    preview: "",
    messages,
  };
}

const assistantTable: Message = {
  id: "assistant-table",
  type: "text",
  author: "assistant",
  timestamp: "10:01",
  text: [
    "Tracked Project Total",
    "",
    "| Group | Files | Lines | Size |",
    "| --- | ---: | ---: | ---: |",
    "| Backend |",
  ].join("\n"),
};

const approvalCard: Message = {
  id: "approval-1",
  type: "approval",
  author: "assistant",
  timestamp: "10:02",
  title: "Codex wants approval",
  command: "git status",
  detail: "Inspect the repository.",
  decision: "pending",
};

function renderCallbacks(overrides: Partial<Parameters<typeof useSessionRenderCallbacks>[0]> = {}) {
  const params: Parameters<typeof useSessionRenderCallbacks>[0] = {
    activeSession: makeSession("idle", []),
    activeSessionSearchMatchItemKey: undefined,
    editorAppearance: "dark",
    getConnectionRetryDisplayState: () => undefined,
    isRefreshingModelOptions: false,
    latestAssistantMessageId: null,
    streamingAssistantTextMessageId: null,
    modelOptionsError: null,
    onArchiveCodexThread: vi.fn(),
    onCompactCodexThread: vi.fn(),
    onForkCodexThread: vi.fn(),
    onOpenDiffPreviewTab: vi.fn(),
    onOpenSourceTab: vi.fn(),
    onOpenConversationFromDiff: vi.fn(),
    onInsertReviewIntoPrompt: vi.fn(),
    onComposerError: vi.fn(),
    onRefreshSessionModelOptions: vi.fn(),
    onRollbackCodexThread: vi.fn(),
    onUnarchiveCodexThread: vi.fn(),
    paneId: "pane-1",
    sessionFindQuery: "",
    sessionSettingNotice: null,
    ...overrides,
  };
  const hook = renderHook(() => useSessionRenderCallbacks(params));
  return { hook, params };
}

function renderDelegationCard(params: Parameters<typeof useSessionRenderCallbacks>[0]) {
  const hook = renderHook(() => useSessionRenderCallbacks(params));
  const element = hook.result.current.renderSessionMessageCard(
    {
      id: "delegations",
      type: "parallelAgents",
      author: "assistant",
      timestamp: "10:10",
      agents: [
        {
          id: "delegation-1",
          source: "delegation",
          title: "Review",
          status: "running",
          detail: "Running",
        },
      ],
    },
    false,
    vi.fn(),
    vi.fn(),
    vi.fn(),
    vi.fn(),
  );
  render(element);
  return hook;
}

describe("SessionPaneView render callbacks", () => {
  it("streams only the active assistant text that is the last transcript item", () => {
    const session = makeSession("active", [assistantTable]);
    const streamingTextId = streamingAssistantTextMessageIdForSession(session);

    expect(streamingTextId).toBe("assistant-table");
    expect(
      shouldPreferStreamingAssistantTextRender(
        assistantTable,
        streamingTextId,
      ),
    ).toBe(true);
  });

  it("does not put prior assistant text back on the streaming path after a prompt or approval card follows it", () => {
    const userPrompt: Message = {
      id: "user-2",
      type: "text",
      author: "you",
      timestamp: "10:03",
      text: "Continue.",
    };

    for (const session of [
      makeSession("active", [assistantTable, userPrompt]),
      makeSession("approval", [assistantTable, approvalCard]),
      makeSession("idle", [assistantTable]),
    ]) {
      const streamingTextId = streamingAssistantTextMessageIdForSession(session);

      expect(streamingTextId).toBeNull();
      expect(
        shouldPreferStreamingAssistantTextRender(
          assistantTable,
          streamingTextId,
        ),
      ).toBe(false);
    }
  });

  it("opens delegation child sessions through the command wrapper", async () => {
    vi.mocked(getDelegationStatusCommand).mockResolvedValueOnce({
      delegationId: "delegation-1",
      childSessionId: "child-1",
      status: "running",
      delegation: {
        id: "delegation-1",
        parentSessionId: "session-1",
        childSessionId: "child-1",
        mode: "reviewer",
        status: "running",
        title: "Review",
        agent: "Codex",
        model: null,
        writePolicy: { kind: "readOnly" },
        createdAt: "now",
        startedAt: "now",
        completedAt: null,
        result: null,
      },
      revision: 2,
      serverInstanceId: "server-1",
    });
    const { params } = renderCallbacks();
    renderDelegationCard(params);

    fireEvent.click(screen.getByRole("button", { name: "Open session" }));

    await waitFor(() =>
      expect(params.onOpenConversationFromDiff).toHaveBeenCalledWith(
        "child-1",
        "pane-1",
      ),
    );
    expect(getDelegationStatusCommand).toHaveBeenCalledWith(
      "session-1",
      "delegation-1",
    );
  });

  it("inserts delegation results through the command wrapper", async () => {
    vi.mocked(getDelegationResultCommand).mockResolvedValueOnce({
      delegationId: "delegation-1",
      childSessionId: "child-1",
      status: "completed",
      summary: "Result summary",
      findings: [],
      changedFiles: [],
      commandsRun: [],
      notes: [],
      revision: 2,
      serverInstanceId: "server-1",
    });
    const { params, hook } = renderCallbacks();
    const element = hook.result.current.renderSessionMessageCard(
      {
        id: "delegations",
        type: "parallelAgents",
        author: "assistant",
        timestamp: "10:10",
        agents: [
          {
            id: "delegation-1",
            source: "delegation",
            title: "Review",
            status: "completed",
            detail: "Done",
          },
        ],
      },
      false,
      vi.fn(),
      vi.fn(),
      vi.fn(),
      vi.fn(),
    );
    render(element);

    fireEvent.click(screen.getByRole("button", { name: "Show tasks" }));
    fireEvent.click(screen.getByRole("button", { name: "Insert result" }));

    await waitFor(() =>
      expect(params.onInsertReviewIntoPrompt).toHaveBeenCalledWith(
        "session-1",
        "pane-1",
        "Delegation result (completed) from child-1:\n\nResult summary",
      ),
    );
    expect(getDelegationResultCommand).toHaveBeenCalledWith(
      "session-1",
      "delegation-1",
    );
  });

  it("cancels delegations through the command wrapper and reports errors", async () => {
    vi.mocked(cancelDelegationCommand).mockRejectedValueOnce(
      new Error("delegation not found"),
    );
    const { params } = renderCallbacks();
    renderDelegationCard(params);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() =>
      expect(params.onComposerError).toHaveBeenCalledWith(
        "delegation not found",
      ),
    );
    expect(cancelDelegationCommand).toHaveBeenCalledWith(
      "session-1",
      "delegation-1",
    );
  });
});
