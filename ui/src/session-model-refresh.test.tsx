import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  ClaudePromptSettingsCard,
  CodexPromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
} from "./App";
import type { Session } from "./types";

function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Cursor",
    workdir: "/tmp",
    model: "auto",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

const noopArchiveThread = () => {};
const noopCompactThread = () => {};
const noopForkThread = () => {};
const noopRollbackThread = () => {};
const noopUnarchiveThread = () => {};

describe("session model refresh controls", () => {
  it("auto-requests Codex model options when the session card opens without a live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={onRequestModelOptions}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("codex-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument();
  });

  it("limits Codex reasoning effort choices to the selected model capabilities", () => {
    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "high",
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
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice="GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium."
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    expect(
      screen.getByText((content) =>
        content.includes("This model supports medium, high reasoning. medium is the default."),
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
      ),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Reasoning effort"));

    expect(screen.getByRole("option", { name: /medium/i })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /high/i })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /minimal/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /^low/i })).not.toBeInTheDocument();
  });

  it("auto-requests Claude model options when the session card opens without a live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <ClaudePromptSettingsCard
        paneId="pane-claude"
        session={makeSession("claude-session", {
          agent: "Claude",
          claudeApprovalMode: "ask",
          model: "sonnet",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("claude-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument();
  });

  it("canonicalizes a known manual Claude model label from the session card", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      <ClaudePromptSettingsCard
        paneId="pane-claude"
        session={makeSession("claude-session", {
          agent: "Claude",
          claudeApprovalMode: "ask",
          model: "sonnet",
          modelOptions: [
            {
              label: "Default (recommended)",
              value: "default",
              description: "Opus 4.6 · Most capable for complex work",
              badges: ["Recommended", "Effort", "Adaptive", "Fast"],
            },
            {
              label: "Sonnet",
              value: "sonnet",
              description: "Sonnet 4.6 · Best for everyday tasks",
              badges: ["Effort"],
            },
          ],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={() => {}}
        onSessionSettingsChange={onSessionSettingsChange}
      />,
    );

    fireEvent.change(screen.getByLabelText("Manual model id"), {
      target: { value: "Default (recommended)" },
    });
    expect(
      screen.getByText(
        "Matches Default (recommended) from the current live list. TermAl will apply default.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "claude-session",
      "model",
      "default",
    );
    expect(screen.getByText("Sonnet 4.6 · Best for everyday tasks")).toBeInTheDocument();
    expect(screen.getByText("Effort")).toBeInTheDocument();
  });

  it("lets Codex apply a manual model id from the session card", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
          modelOptions: [{ label: "GPT-5.4", value: "gpt-5.4" }],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={onSessionSettingsChange}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    fireEvent.change(screen.getByLabelText("Manual model id"), {
      target: { value: "gpt-5.5-preview" },
    });
    expect(
      screen.getByText(
        "gpt-5.5-preview is not in the current live model list. TermAl will still try it on the next prompt.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "codex-session",
      "model",
      "gpt-5.5-preview",
    );
  });

  it("disables Codex thread actions until the session has a live thread id", () => {
    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    expect(screen.getByRole("button", { name: "Fork thread" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Compact" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Archive" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Unarchive" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Roll back" })).toBeDisabled();
  });

  it("fires Codex thread actions from the session card when a live thread exists", () => {
    const onArchiveThread = vi.fn();
    const onCompactThread = vi.fn();
    const onForkThread = vi.fn();
    const onRollbackThread = vi.fn();
    const onUnarchiveThread = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
          externalSessionId: "thread-live",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={onArchiveThread}
        onCompactThread={onCompactThread}
        onForkThread={onForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={onRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={onUnarchiveThread}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Fork thread" }));
    fireEvent.click(screen.getByRole("button", { name: "Compact" }));
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Unarchive" }));
    fireEvent.change(screen.getByLabelText("Roll back turns"), {
      target: { value: "3" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Roll back" }));

    expect(onForkThread).toHaveBeenCalledWith("codex-session", "pane-codex");
    expect(onCompactThread).toHaveBeenCalledWith("codex-session");
    expect(onArchiveThread).toHaveBeenCalledWith("codex-session");
    expect(onUnarchiveThread).toHaveBeenCalledWith("codex-session");
    expect(onRollbackThread).toHaveBeenCalledWith("codex-session", 3);
  });

  it("auto-requests Cursor model options when the session card opens without a live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <CursorPromptSettingsCard
        paneId="pane-cursor"
        session={makeSession("cursor-session", {
          agent: "Cursor",
          cursorMode: "agent",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("cursor-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument();
  });

  it("lets Gemini refresh model options manually from the session card", () => {
    const onRequestModelOptions = vi.fn();

    render(
      <GeminiPromptSettingsCard
        paneId="pane-gemini"
        session={makeSession("gemini-session", {
          agent: "Gemini",
          geminiApprovalMode: "default",
          modelOptions: [{ label: "Auto", value: "auto" }],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));

    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(onRequestModelOptions).toHaveBeenCalledWith("gemini-session");
  });

  it("shows inline refresh errors in the session card", () => {
    render(
      <GeminiPromptSettingsCard
        paneId="pane-gemini"
        session={makeSession("gemini-session", {
          agent: "Gemini",
          geminiApprovalMode: "default",
          model: "auto",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError="Gemini CLI is not authenticated."
        onRequestModelOptions={() => {}}
        onSessionSettingsChange={() => {}}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Could not refresh Gemini's live model list for this session. Gemini CLI is not authenticated.",
    );
  });
});
