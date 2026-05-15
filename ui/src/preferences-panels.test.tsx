import { fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";

import {
  ALL_CLAUDE_APPROVAL_MODES,
  ClaudeApprovalsPreferencesPanel,
  CLAUDE_APPROVAL_OPTIONS,
  CodexPromptPreferencesPanel,
  CursorPreferencesPanel,
  GeminiPreferencesPanel,
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

function renderCursorPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
} = {}) {
  return {
    onSelectModel,
    ...render(
      <CursorPreferencesPanel
        defaultCursorMode="agent"
        defaultCursorModel={defaultModel}
        onSelectMode={vi.fn()}
        onSelectModel={onSelectModel}
      />,
    ),
  };
}

function renderGeminiPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
} = {}) {
  return {
    onSelectModel,
    ...render(
      <GeminiPreferencesPanel
        defaultGeminiApprovalMode="default"
        defaultGeminiModel={defaultModel}
        onSelectApprovalMode={vi.fn()}
        onSelectModel={onSelectModel}
      />,
    ),
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
    expect(screen.queryByText(/not in the current live model list/u)).not.toBeInTheDocument();
  });

  it("warns for unknown manual model ids only when a live model list exists", () => {
    renderCodexPanel({
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

    fireEvent.change(screen.getByLabelText("Codex custom default model"), {
      target: { value: "gpt-5.6-preview" },
    });

    expect(screen.getByText(/not in the current live model list/u)).toBeInTheDocument();
  });

  it("applies manual default model ids with Enter", () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ defaultModel: "default", onSelectModel });

    fireEvent.change(screen.getByLabelText("Codex custom default model"), {
      target: { value: "gpt-5.7-preview" },
    });
    fireEvent.keyDown(screen.getByLabelText("Codex custom default model"), {
      key: "Enter",
    });

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.7-preview");
  });

  it("applies arbitrary app-level default model ids for Claude, Cursor, and Gemini", () => {
    const panels = [
      {
        agent: "Claude",
        renderPanel: renderClaudePanel,
        value: "claude-opus-4-5",
      },
      {
        agent: "Cursor",
        renderPanel: renderCursorPanel,
        value: "cursor-custom-model",
      },
      {
        agent: "Gemini",
        renderPanel: renderGeminiPanel,
        value: "gemini-custom-model",
      },
    ] as const;

    for (const { agent, renderPanel, value } of panels) {
      const onSelectModel = vi.fn();
      const { unmount } = renderPanel({ onSelectModel });

      fireEvent.change(screen.getByLabelText(`${agent} custom default model`), {
        target: { value },
      });
      fireEvent.click(screen.getByRole("button", { name: `Apply ${agent} default model` }));

      expect(onSelectModel).toHaveBeenCalledWith(value);
      unmount();
    }
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
    const userFacingModes = CLAUDE_APPROVAL_OPTIONS.map((option) => option.value);
    const internalModes = Array.from(INTERNAL_CLAUDE_APPROVAL_MODES);
    const partitionedModes = [...userFacingModes, ...internalModes].sort();

    expect(userFacingModes).not.toContain("read-only-auto-approve");
    expect(partitionedModes).toEqual([...ALL_CLAUDE_APPROVAL_MODES].sort());
    expect(new Set(partitionedModes).size).toBe(partitionedModes.length);
  });
});
