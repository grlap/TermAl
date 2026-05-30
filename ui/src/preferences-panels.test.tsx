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
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof CursorPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultCursorMode: "agent" as const,
    defaultCursorModel: defaultModel,
    onSelectModel,
    onSelectMode: vi.fn(),
    sessions,
  };

  return {
    onSelectModel,
    ...render(<CursorPreferencesPanel {...props} />),
  };
}

function renderGeminiPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof GeminiPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultGeminiApprovalMode: "default" as const,
    defaultGeminiModel: defaultModel,
    onSelectApprovalMode: vi.fn(),
    onSelectModel,
    sessions,
  };

  return {
    onSelectModel,
    ...render(<GeminiPreferencesPanel {...props} />),
  };
}

describe("AgentDefaultModelControl", () => {
  it("keeps configured custom values in the dropdown without a text input", () => {
    renderCodexPanel({ defaultModel: "gpt-5.5" });

    expect(
      screen.getByRole("combobox", { name: "Codex default model" }),
    ).toHaveTextContent("gpt-5.5");
    expect(
      screen.queryByLabelText("Codex custom default model"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Apply Codex default model" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/Select a known model/u)).toHaveTextContent(
      "Select a known model or choose Default to let Codex use its built-in default.",
    );
  });

  it("renders Claude default model selection as dropdown-only", () => {
    renderClaudePanel();

    expect(
      screen.getByRole("combobox", { name: "Claude default model" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Claude custom default model"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Apply Claude default model" }),
    ).not.toBeInTheDocument();
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

  it("offers static Codex model choices before a live session model list loads", async () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ onSelectModel });

    fireEvent.click(screen.getByRole("combobox", { name: "Codex default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /GPT-5\.4/u }));

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.4");
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

  it("selects Cursor and Gemini defaults from live session options", async () => {
    const onSelectCursorModel = vi.fn();
    renderCursorPanel({
      onSelectModel: onSelectCursorModel,
      sessions: [
        {
          id: "cursor-1",
          name: "Cursor",
          emoji: "",
          agent: "Cursor",
          workdir: "/tmp",
          model: "auto",
          modelOptions: [
            {
              label: "Cursor Pro",
              value: "cursor-pro",
              description: "Cursor subscription model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Cursor default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Cursor Pro/u }));

    expect(onSelectCursorModel).toHaveBeenCalledWith("cursor-pro");

    const onSelectGeminiModel = vi.fn();
    renderGeminiPanel({
      onSelectModel: onSelectGeminiModel,
      sessions: [
        {
          id: "gemini-1",
          name: "Gemini",
          emoji: "",
          agent: "Gemini",
          workdir: "/tmp",
          model: "auto",
          modelOptions: [
            {
              label: "Gemini Pro",
              value: "gemini-pro",
              description: "Gemini model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Gemini default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Gemini Pro/u }));

    expect(onSelectGeminiModel).toHaveBeenCalledWith("gemini-pro");
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
