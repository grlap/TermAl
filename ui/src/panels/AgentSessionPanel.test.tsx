import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AgentSessionPanelFooter } from "./AgentSessionPanel";
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
  session,
  committedDraft = "",
  onDraftCommit = vi.fn(),
  modelOptionsError = null,
  onRefreshSessionModelOptions = vi.fn(),
  onSend = vi.fn(() => true),
  onSessionSettingsChange = vi.fn(),
}: {
  session: Session | null;
  committedDraft?: string;
  onDraftCommit?: (sessionId: string, nextValue: string) => void;
  modelOptionsError?: string | null;
  onRefreshSessionModelOptions?: (sessionId: string) => void;
  onSend?: (sessionId: string, draftText?: string) => boolean;
  onSessionSettingsChange?: (sessionId: string, field: string, value: string) => void;
}) {
  return (
    <AgentSessionPanelFooter
      paneId="pane-1"
      viewMode="session"
      activeSession={session}
      committedDraft={committedDraft}
      draftAttachments={[]}
      formatByteSize={(byteSize) => `${byteSize} B`}
      isSending={false}
      isStopping={false}
      isSessionBusy={false}
      showNewResponseIndicator={false}
      footerModeLabel="Session"
      onScrollToLatest={() => {}}
      onDraftCommit={onDraftCommit}
      onDraftAttachmentRemove={() => {}}
      isRefreshingModelOptions={false}
      modelOptionsError={modelOptionsError}
      onRefreshSessionModelOptions={onRefreshSessionModelOptions}
      onSend={onSend}
      onSessionSettingsChange={onSessionSettingsChange}
      onStopSession={() => {}}
      onPaste={() => {}}
    />
  );
}

describe("AgentSessionPanelFooter", () => {
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
