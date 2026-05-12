import { fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";

import {
  ClaudeApprovalsPreferencesPanel,
  CodexPromptPreferencesPanel,
} from "./preferences-panels";
import { MAX_DEFAULT_MODEL_PREFERENCE_CHARS } from "./session-model-utils";

function renderCodexPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof CodexPromptPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultApprovalPolicy: "never" as const,
    defaultModel,
    defaultReasoningEffort: "medium" as const,
    defaultSandboxMode: "workspace-write" as const,
    onSelectApprovalPolicy: vi.fn(),
    onSelectModel,
    onSelectReasoningEffort: vi.fn(),
    onSelectSandboxMode: vi.fn(),
    sessions,
  };

  return {
    onSelectModel,
    ...render(<CodexPromptPreferencesPanel {...props} />),
  };
}

function renderClaudePanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof ClaudeApprovalsPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultClaudeApprovalMode: "ask" as const,
    defaultClaudeEffort: "default" as const,
    defaultClaudeModel: defaultModel,
    onSelectEffort: vi.fn(),
    onSelectModel,
    onSelectMode: vi.fn(),
    sessions,
  };

  return {
    onSelectModel,
    ...render(<ClaudeApprovalsPreferencesPanel {...props} />),
  };
}

describe("AgentDefaultModelControl", () => {
  it("preserves a dirty draft when the upstream preference changes", () => {
    const onSelectModel = vi.fn();
    const { rerender } = renderCodexPanel({ onSelectModel });
    const input = screen.getByLabelText("Codex custom default model");

    fireEvent.change(input, { target: { value: "gpt-5.5" } });
    expect(input).toHaveValue("gpt-5.5");

    rerender(
      <CodexPromptPreferencesPanel
        defaultApprovalPolicy="never"
        defaultModel="gpt-5.4"
        defaultReasoningEffort="medium"
        defaultSandboxMode="workspace-write"
        onSelectApprovalPolicy={vi.fn()}
        onSelectModel={onSelectModel}
        onSelectReasoningEffort={vi.fn()}
        onSelectSandboxMode={vi.fn()}
      />,
    );

    expect(input).toHaveValue("gpt-5.5");
  });

  it("canonicalizes default values before applying", () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ defaultModel: "gpt-5.4", onSelectModel });
    const input = screen.getByLabelText("Codex custom default model");
    const hint = screen.getByText(/or leave blank/u);

    fireEvent.change(input, { target: { value: " DEFAULT " } });
    fireEvent.click(screen.getByRole("button", { name: "Apply Codex default model" }));

    expect(input).toHaveAttribute("aria-describedby", hint.id);
    expect(onSelectModel).toHaveBeenCalledWith("default");
  });

  it("applies a custom model with Enter", () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ onSelectModel });
    const input = screen.getByLabelText("Codex custom default model");

    fireEvent.change(input, { target: { value: "gpt-5.5" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.5");
  });

  it("resets to the canonical default sentinel", () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ defaultModel: "gpt-5.5", onSelectModel });

    const reset = screen.getByRole("button", {
      name: "Reset Codex default model",
    });
    expect(reset).toHaveClass("session-model-custom-reset");

    fireEvent.click(reset);

    expect(onSelectModel).toHaveBeenCalledWith("default");
  });

  it("renders default-like upstream values as the canonical sentinel", () => {
    renderCodexPanel({ defaultModel: " DEFAULT " });
    const input = screen.getByLabelText("Codex custom default model");

    expect(input).toHaveValue("default");
    expect(
      screen.getByRole("button", { name: "Reset Codex default model" }),
    ).toBeDisabled();
  });

  it("caps custom model drafts by Unicode scalar count", () => {
    renderCodexPanel();
    const input = screen.getByLabelText("Codex custom default model");

    fireEvent.change(input, {
      target: { value: "😀".repeat(MAX_DEFAULT_MODEL_PREFERENCE_CHARS + 1) },
    });

    expect(Array.from((input as HTMLInputElement).value)).toHaveLength(
      MAX_DEFAULT_MODEL_PREFERENCE_CHARS,
    );
  });

  it("selects a Codex default model from live session options", async () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({
      onSelectModel,
      sessions: [
        {
          id: "codex-1",
          name: "Codex",
          emoji: "",
          agent: "Codex",
          workdir: "/tmp",
          model: "default",
          modelOptions: [
            {
              label: "GPT-5.5",
              value: "gpt-5.5",
              description: "Latest Codex model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Codex default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /GPT-5\.5/u }));

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.5");
  });

  it("selects a Claude default model from live session options", async () => {
    const onSelectModel = vi.fn();
    renderClaudePanel({
      onSelectModel,
      sessions: [
        {
          id: "claude-1",
          name: "Claude",
          emoji: "",
          agent: "Claude",
          workdir: "/tmp",
          model: "default",
          modelOptions: [
            {
              label: "Claude Sonnet 4.5",
              value: "claude-sonnet-4-5",
              description: "Balanced Claude model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Claude default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Claude Sonnet 4\.5/u }));

    expect(onSelectModel).toHaveBeenCalledWith("claude-sonnet-4-5");
  });
});
