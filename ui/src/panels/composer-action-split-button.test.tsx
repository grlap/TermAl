// Owns keyboard, focus, and accessibility tests for the composer action split
// button. Delegation request behavior remains covered by the footer tests.

import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  ComposerActionSplitButton,
  type ComposerActionMode,
  type ComposerActionOption,
} from "./composer-action-split-button";

const OPTIONS: readonly ComposerActionOption[] = [
  { mode: "send", label: "Send" },
  { mode: "reviewer", label: "Delegate · Reviewer" },
  { mode: "explorer", label: "Delegate · Explorer" },
];

function renderSplitButton({
  onAction = vi.fn(),
  onModeChange = vi.fn(),
  options = OPTIONS,
  selectedMode = "send" as ComposerActionMode,
} = {}) {
  return {
    onAction,
    onModeChange,
    ...render(
      <>
        <input aria-label="Draft" defaultValue="Keep this draft" />
        <ComposerActionSplitButton
          actionLabel={
            selectedMode === "send"
              ? "Send"
              : selectedMode === "reviewer"
                ? "Delegate · Reviewer"
                : "Delegate · Explorer"
          }
          disabled={false}
          onAction={onAction}
          onModeChange={onModeChange}
          options={options}
          selectedMode={selectedMode}
        />
      </>,
    ),
  };
}

describe("ComposerActionSplitButton", () => {
  it("exposes a radio menu and returns focus after selection", async () => {
    const user = userEvent.setup();
    const { onModeChange } = renderSplitButton();
    const trigger = screen.getByRole("button", {
      name: "Choose composer action, current: Send",
    });

    await user.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByRole("menuitemradio", { name: "Send" }),
    ).toHaveAttribute("aria-checked", "true");

    await user.click(
      screen.getByRole("menuitemradio", { name: "Delegate · Explorer" }),
    );

    expect(onModeChange).toHaveBeenCalledWith("explorer");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("skips disabled actions during keyboard navigation and closes on Escape", async () => {
    const user = userEvent.setup();
    renderSplitButton({
      options: [
        OPTIONS[0]!,
        { ...OPTIONS[1]!, disabled: true },
        OPTIONS[2]!,
      ],
    });
    const primary = screen.getByRole("button", { name: "Send" });
    primary.focus();

    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitemradio", { name: "Send" })).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(
      screen.getByRole("menuitemradio", { name: "Delegate · Explorer" }),
    ).toHaveFocus();
    expect(
      screen.getByRole("menuitemradio", { name: "Delegate · Reviewer" }),
    ).toBeDisabled();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Choose composer action, current: Send",
      }),
    ).toHaveFocus();
  });

  it("dismisses outside without changing or clearing the draft", async () => {
    const user = userEvent.setup();
    const { onModeChange } = renderSplitButton();
    await user.click(
      screen.getByRole("button", {
        name: "Choose composer action, current: Send",
      }),
    );

    fireEvent.pointerDown(screen.getByRole("textbox", { name: "Draft" }));

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Draft" })).toHaveValue(
      "Keep this draft",
    );
    expect(onModeChange).not.toHaveBeenCalled();
  });

  it("keeps the primary click separate from disclosure selection", async () => {
    const user = userEvent.setup();
    const { onAction, onModeChange } = renderSplitButton({
      selectedMode: "reviewer",
    });

    await user.click(
      screen.getByRole("button", { name: "Delegate · Reviewer" }),
    );

    expect(onAction).toHaveBeenCalledOnce();
    expect(onModeChange).not.toHaveBeenCalled();
  });
});
