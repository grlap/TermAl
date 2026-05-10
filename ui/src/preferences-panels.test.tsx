import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { CodexPromptPreferencesPanel } from "./preferences-panels";
import { MAX_DEFAULT_MODEL_PREFERENCE_CHARS } from "./session-model-utils";

function renderCodexPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
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
  };

  return {
    onSelectModel,
    ...render(<CodexPromptPreferencesPanel {...props} />),
  };
}

describe("AgentDefaultModelControl", () => {
  it("preserves a dirty draft when the upstream preference changes", () => {
    const onSelectModel = vi.fn();
    const { rerender } = renderCodexPanel({ onSelectModel });
    const input = screen.getByLabelText("Codex default model");

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
    const input = screen.getByLabelText("Codex default model");
    const hint = screen.getByText(/or leave blank/u);

    fireEvent.change(input, { target: { value: " DEFAULT " } });
    fireEvent.click(screen.getByRole("button", { name: "Apply Codex default model" }));

    expect(input).toHaveAttribute("aria-describedby", hint.id);
    expect(onSelectModel).toHaveBeenCalledWith("default");
  });

  it("applies a custom model with Enter", () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ onSelectModel });
    const input = screen.getByLabelText("Codex default model");

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
    const input = screen.getByLabelText("Codex default model");

    expect(input).toHaveValue("default");
    expect(
      screen.getByRole("button", { name: "Reset Codex default model" }),
    ).toBeDisabled();
  });

  it("caps custom model drafts by Unicode scalar count", () => {
    renderCodexPanel();
    const input = screen.getByLabelText("Codex default model");

    fireEvent.change(input, {
      target: { value: "😀".repeat(MAX_DEFAULT_MODEL_PREFERENCE_CHARS + 1) },
    });

    expect(Array.from((input as HTMLInputElement).value)).toHaveLength(
      MAX_DEFAULT_MODEL_PREFERENCE_CHARS,
    );
  });
});
