import { fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";

import {
  ClaudeApprovalsPreferencesPanel,
  CLAUDE_APPROVAL_OPTIONS,
  CodexPromptPreferencesPanel,
  INTERNAL_CLAUDE_APPROVAL_MODES,
} from "./preferences-panels";

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
  it("keeps a custom model entry path beside the known-model combobox", () => {
    renderCodexPanel({ defaultModel: "gpt-5.5" });

    expect(
      screen.getByRole("combobox", { name: "Codex default model" }),
    ).toHaveTextContent("gpt-5.5");
    expect(screen.getByLabelText("Codex custom default model")).toHaveValue("gpt-5.5");
    expect(
      screen.queryByRole("button", { name: "Reset Codex default model" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Apply Codex default model" }),
    ).toBeDisabled();
  });

  it("applies an arbitrary app-level default model id", () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ defaultModel: "default", onSelectModel });

    fireEvent.change(screen.getByLabelText("Codex custom default model"), {
      target: { value: "gpt-5.6-preview" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Apply Codex default model" }));

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.6-preview");
  });

  it("selects the canonical default sentinel from the combobox", async () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ defaultModel: "gpt-5.5", onSelectModel });

    fireEvent.click(screen.getByRole("combobox", { name: "Codex default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Default/u }));

    expect(onSelectModel).toHaveBeenCalledWith("default");
  });

  it("renders default-like upstream values as the canonical sentinel", () => {
    renderCodexPanel({ defaultModel: " DEFAULT " });

    expect(
      screen.getByRole("combobox", { name: "Codex default model" }),
    ).toHaveTextContent("Default");
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

  it("keeps read-only auto-approve internal to delegation flows", () => {
    expect(CLAUDE_APPROVAL_OPTIONS.map((option) => option.value)).not.toContain(
      "read-only-auto-approve",
    );
    expect(INTERNAL_CLAUDE_APPROVAL_MODES.has("read-only-auto-approve")).toBe(true);
  });
});
