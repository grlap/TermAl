// Tests for the Settings dialog backdrop dismissal guard.
//
// The backdrop historically attached `onMouseDown -> onClose`
// without a button-code guard, which swallowed middle-click (paste
// on Linux, auto-scroll anchor on Windows) and right-click
// (context menu on every platform) by dismissing the dialog before
// the browser could handle the gesture. These tests pin the
// primary-button-only behaviour introduced in the fix.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { SettingsDialogShell } from "./SettingsDialogShell";

describe("SettingsDialogShell backdrop dismissal", () => {
  it("closes on primary-button mousedown on the backdrop", () => {
    const onClose = vi.fn();
    render(
      <SettingsDialogShell onClose={onClose}>
        <div data-testid="body">body</div>
      </SettingsDialogShell>,
    );

    const backdrop = document.querySelector(".dialog-backdrop");
    expect(backdrop).not.toBeNull();

    fireEvent.mouseDown(backdrop as Element, { button: 0 });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not close on middle-button mousedown on the backdrop", () => {
    const onClose = vi.fn();
    render(
      <SettingsDialogShell onClose={onClose}>
        <div data-testid="body">body</div>
      </SettingsDialogShell>,
    );

    const backdrop = document.querySelector(".dialog-backdrop");
    fireEvent.mouseDown(backdrop as Element, { button: 1 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("does not close on right-button mousedown on the backdrop", () => {
    const onClose = vi.fn();
    render(
      <SettingsDialogShell onClose={onClose}>
        <div data-testid="body">body</div>
      </SettingsDialogShell>,
    );

    const backdrop = document.querySelector(".dialog-backdrop");
    fireEvent.mouseDown(backdrop as Element, { button: 2 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("does not close when mousedown originates inside the dialog body", () => {
    const onClose = vi.fn();
    render(
      <SettingsDialogShell onClose={onClose}>
        <div data-testid="body">body</div>
      </SettingsDialogShell>,
    );

    fireEvent.mouseDown(screen.getByTestId("body"), { button: 0 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes when the header close button is clicked", () => {
    const onClose = vi.fn();
    render(
      <SettingsDialogShell onClose={onClose}>
        <div data-testid="body">body</div>
      </SettingsDialogShell>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Close dialog" }));

    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
