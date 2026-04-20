// Pure-function tests for the dialog-backdrop dismiss predicate.
//
// Scope: confirm the four accepted button shapes map to the
// intended dismiss decision, and that macOS Ctrl-click (primary
// button with `ctrlKey: true` on an Apple platform) is recognised
// as a secondary-click gesture and NOT dismissed. The platform
// check reads `navigator.userAgentData.platform` with a fallback
// to `navigator.platform`; both are stubbed per case via
// `Object.defineProperty` so the suite is deterministic regardless
// of the host running it.

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { isDialogBackdropDismissMouseDown } from "./dialog-backdrop-dismiss";

type NavigatorWithUserAgentData = Navigator & {
  userAgentData?: { platform?: string };
};

describe("isDialogBackdropDismissMouseDown", () => {
  let originalPlatform: PropertyDescriptor | undefined;
  let originalUserAgentData: PropertyDescriptor | undefined;

  beforeEach(() => {
    originalPlatform = Object.getOwnPropertyDescriptor(navigator, "platform");
    originalUserAgentData = Object.getOwnPropertyDescriptor(
      navigator as NavigatorWithUserAgentData,
      "userAgentData",
    );
  });

  afterEach(() => {
    if (originalPlatform) {
      Object.defineProperty(navigator, "platform", originalPlatform);
    } else {
      // In jsdom, `navigator.platform` is commonly inherited from
      // the prototype rather than existing as an own property. When
      // `stubPlatform` then calls `Object.defineProperty(navigator,
      // "platform", ...)` it creates an OWN property; leaving it
      // there shadows the prototype value for the rest of the
      // worker. Later tests in the same file (or, worse, other
      // files scheduled onto the same worker) would silently
      // inherit the previous platform stub and become order-
      // dependent. Mirror the `userAgentData` cleanup below:
      // when there was no original own descriptor, delete the
      // stubbed own property.
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

  it("returns true for a primary-button mousedown on a non-Apple platform", () => {
    stubPlatform("Win32");
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: false }),
    ).toBe(true);
  });

  it("returns false for a middle-button mousedown", () => {
    stubPlatform("Win32");
    expect(
      isDialogBackdropDismissMouseDown({ button: 1, ctrlKey: false }),
    ).toBe(false);
  });

  it("returns false for a right-button mousedown", () => {
    stubPlatform("Win32");
    expect(
      isDialogBackdropDismissMouseDown({ button: 2, ctrlKey: false }),
    ).toBe(false);
  });

  it("returns true for a primary-button mousedown with ctrlKey on a non-Apple platform", () => {
    // Linux / Windows Ctrl+click is NOT a secondary-click gesture;
    // it should still dismiss the dialog like a plain primary-button
    // click (the OS does not escalate it to a context menu).
    stubPlatform("Linux x86_64");
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: true }),
    ).toBe(true);
  });

  it("returns false for a primary-button mousedown with ctrlKey on macOS", () => {
    stubPlatform("MacIntel");
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: true }),
    ).toBe(false);
  });

  it("returns true for a plain primary-button mousedown on macOS (no ctrlKey)", () => {
    stubPlatform("MacIntel");
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: false }),
    ).toBe(true);
  });

  it("returns false for a middle-button mousedown on macOS", () => {
    stubPlatform("MacIntel");
    expect(
      isDialogBackdropDismissMouseDown({ button: 1, ctrlKey: false }),
    ).toBe(false);
  });

  it("honours userAgentData.platform over navigator.platform when both are set", () => {
    // Modern Chromium exposes `navigator.userAgentData.platform`;
    // older browsers still populate `navigator.platform`. The helper
    // should prefer the newer source so a user-agent-reduction UA
    // stub (which masks `navigator.platform`) still detects macOS.
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: "",
    });
    Object.defineProperty(navigator, "userAgentData", {
      configurable: true,
      value: { platform: "macOS" },
    });
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: true }),
    ).toBe(false);
  });

  // Safari / Firefox do not expose `navigator.userAgentData`; the
  // helper must fall back to `navigator.platform` on those. Every
  // other test in this file writes both values via `stubPlatform`,
  // so the fallback-only path went unexercised — a regression that
  // broke the `navigator.platform` branch would pass the whole
  // suite via the userAgentData path.
  it("falls back to navigator.platform for Apple detection when userAgentData is absent", () => {
    Reflect.deleteProperty(
      navigator as NavigatorWithUserAgentData,
      "userAgentData",
    );
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: "MacIntel",
    });
    // macOS via navigator.platform → Ctrl+primary is a context-menu
    // gesture, not a dismiss.
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: true }),
    ).toBe(false);
  });

  it("falls back to navigator.platform for non-Apple detection when userAgentData is absent", () => {
    Reflect.deleteProperty(
      navigator as NavigatorWithUserAgentData,
      "userAgentData",
    );
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: "Linux x86_64",
    });
    // Non-Apple via navigator.platform → Ctrl+primary is NOT a
    // context-menu gesture; backdrop dismisses normally.
    expect(
      isDialogBackdropDismissMouseDown({ button: 0, ctrlKey: true }),
    ).toBe(true);
  });
});
