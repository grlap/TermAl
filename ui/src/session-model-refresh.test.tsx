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
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
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
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));

    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(onRequestModelOptions).toHaveBeenCalledWith("gemini-session");
  });
});
