import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AgentSessionPanelFooter, RunningIndicator } from "./AgentSessionPanel";
import type { Session } from "../types";

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

function renderFooter({
  isPaneActive = true,
  session,
  committedDraft = "",
  isUpdating = false,
  onDraftCommit = vi.fn(),
  modelOptionsError = null,
  agentCommands = [],
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
  return (
    <AgentSessionPanelFooter
      paneId="pane-1"
      viewMode="session"
      isPaneActive={isPaneActive}
      activeSession={session}
      committedDraft={committedDraft}
      draftAttachments={[]}
      formatByteSize={(byteSize) => `${byteSize} B`}
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
