// Tests for the Settings dialog backdrop dismissal guard.
//
// The backdrop historically attached `onMouseDown -> onClose`
// without a button-code guard, which swallowed middle-click (paste
// on Linux, auto-scroll anchor on Windows) and right-click
// (context menu on every platform) by dismissing the dialog before
// the browser could handle the gesture. macOS Ctrl-click fires
// with `button === 0` but the OS treats it as a secondary-click
// gesture, so the fix also guards `(event.ctrlKey && isMac)`.
// These tests pin the primary-button-only behaviour against all
// four gestures.
//
// The backdrop is located via `screen.getByRole("dialog")
// .parentElement` rather than a raw `querySelector(".dialog-
// backdrop")` so a future classname rename does not silently pass
// the non-primary-button cases by returning `null` that would
// later throw opaquely at `fireEvent.mouseDown`.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { SettingsDialogShell } from "./SettingsDialogShell";

type NavigatorWithUserAgentData = Navigator & {
  userAgentData?: { platform?: string };
};

describe("SettingsDialogShell backdrop dismissal", () => {
  let originalPlatform: PropertyDescriptor | undefined;
  let originalUserAgentData: PropertyDescriptor | undefined;

  beforeEach(() => {
    originalPlatform = Object.getOwnPropertyDescriptor(navigator, "platform");
    originalUserAgentData = Object.getOwnPropertyDescriptor(
      navigator as NavigatorWithUserAgentData,
      "userAgentData",
    );
    stubPlatform("Win32");
  });

  afterEach(() => {
    if (originalPlatform) {
      Object.defineProperty(navigator, "platform", originalPlatform);
    } else {
      // In jsdom, `navigator.platform` is commonly inherited from
      // the prototype rather than existing as an own property. The
      // `stubPlatform` call above creates an OWN property on each
      // test; leaving it behind shadows the prototype value for
      // any later test that runs in the same worker and doesn't
      // stub explicitly. Mirror the `userAgentData` cleanup below.
      Reflect.deleteProperty(navigator, "platform");
    }
    if (originalUserAgentData) {
      Object.defineProperty(navigator, "userAgentData", originalUserAgentData);
    } else {
      Reflect.deleteProperty(
        navigator as NavigatorWithUserAgentData,
        "userAgentData",
      );
    }
  });

  function stubPlatform(platform: string) {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: platform,
    });
    Object.defineProperty(navigator, "userAgentData", {
      configurable: true,
      value: { platform },
    });
  }

  function renderShell(onClose: () => void) {
    return render(
      <SettingsDialogShell onClose={onClose}>
        <div data-testid="body">body</div>
      </SettingsDialogShell>,
    );
  }

  function getBackdrop(): HTMLElement {
    const dialog = screen.getByRole("dialog");
    const backdrop = dialog.parentElement;
    if (!(backdrop instanceof HTMLElement)) {
      throw new Error("Dialog backdrop not found — classname drift?");
    }
    return backdrop;
  }

  it("closes on primary-button mousedown on the backdrop", () => {
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(getBackdrop(), { button: 0 });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not close on middle-button mousedown on the backdrop", () => {
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(getBackdrop(), { button: 1 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("does not close on right-button mousedown on the backdrop", () => {
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(getBackdrop(), { button: 2 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("does not close on macOS Ctrl-click on the backdrop", () => {
    // macOS Ctrl-click reports as primary-button (`button === 0`)
    // with `ctrlKey === true`, but the OS escalates it to a
    // secondary-click context-menu gesture. Dismissing the dialog
    // here would swallow the menu before the browser could open it.
    stubPlatform("MacIntel");
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(getBackdrop(), { button: 0, ctrlKey: true });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("does close on plain primary-button mousedown on macOS (no ctrlKey)", () => {
    stubPlatform("MacIntel");
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(getBackdrop(), { button: 0 });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does close on primary-button mousedown with ctrlKey on non-Apple platforms", () => {
    // Linux / Windows Ctrl+click is NOT a secondary-click gesture;
    // only macOS escalates it. The backdrop should still dismiss on
    // Ctrl+primary elsewhere so the Linux/Windows user's normal
    // click isn't accidentally preserved.
    stubPlatform("Linux x86_64");
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(getBackdrop(), { button: 0, ctrlKey: true });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not close when mousedown originates inside the dialog body", () => {
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.mouseDown(screen.getByTestId("body"), { button: 0 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes when the header close button is clicked", () => {
    const onClose = vi.fn();
    renderShell(onClose);

    fireEvent.click(screen.getByRole("button", { name: "Close dialog" }));

    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
