import { describe, expect, it } from "vitest";

import { resolvePaneScrollCommand, shouldHandlePanePageKey } from "./pane-keyboard";

describe("shouldHandlePanePageKey", () => {
  it("allows pane paging for non-editable targets", () => {
    expect(shouldHandlePanePageKey(document.createElement("div"))).toBe(true);
  });

  it("allows textarea paging only when the caret is at the start", () => {
    const textarea = document.createElement("textarea");
    textarea.value = "hello";
    textarea.setSelectionRange(0, 0);

    expect(shouldHandlePanePageKey(textarea)).toBe(true);

    textarea.setSelectionRange(2, 2);
    expect(shouldHandlePanePageKey(textarea)).toBe(false);
  });

  it("blocks paging for text selections inside the textarea", () => {
    const textarea = document.createElement("textarea");
    textarea.value = "hello";
    textarea.setSelectionRange(0, 3);

    expect(shouldHandlePanePageKey(textarea)).toBe(false);
  });

  it("blocks paging for contenteditable targets", () => {
    const element = document.createElement("div");
    element.contentEditable = "true";

    expect(shouldHandlePanePageKey(element)).toBe(false);
  });
});

describe("resolvePaneScrollCommand", () => {
  it("maps Shift+PageUp to a boundary jump", () => {
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: false,
          key: "PageUp",
          metaKey: false,
          shiftKey: true,
        },
        document.createElement("div"),
        "MacIntel",
      ),
    ).toEqual({ kind: "boundary", direction: "up" });
  });

  it("maps Ctrl+ArrowDown to a boundary jump on Windows/Linux", () => {
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "ArrowDown",
          metaKey: false,
          shiftKey: false,
        },
        document.createElement("div"),
        "Win32",
      ),
    ).toEqual({ kind: "boundary", direction: "down" });
  });

  it("maps Ctrl+ArrowUp to a boundary jump on Windows/Linux", () => {
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "ArrowUp",
          metaKey: false,
          shiftKey: false,
        },
        document.createElement("div"),
        "Linux x86_64",
      ),
    ).toEqual({ kind: "boundary", direction: "up" });
  });

  it("maps Ctrl+PageDown to a boundary jump on Windows/Linux", () => {
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "PageDown",
          metaKey: false,
          shiftKey: false,
        },
        document.createElement("div"),
        "Win32",
      ),
    ).toEqual({ kind: "boundary", direction: "down" });
  });

  it("maps Ctrl+PageUp from a textarea at the start to a boundary jump", () => {
    const textarea = document.createElement("textarea");
    textarea.value = "prompt";
    textarea.setSelectionRange(0, 0);

    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "PageUp",
          metaKey: false,
          shiftKey: false,
        },
        textarea,
        "Win32",
      ),
    ).toEqual({ kind: "boundary", direction: "up" });
  });

  it("does not intercept Ctrl+PageUp on macOS", () => {
    // Ctrl+PageUp/PageDown is a Windows/Linux boundary-jump
    // shortcut. macOS's equivalent is Cmd-arrow (already rejected
    // by the `metaKey` gate on this function). The pane must not
    // fall through to a one-page scroll on Apple — that would be
    // a platform-specific shortcut capture the user didn't ask
    // for, and is inconsistent with the `Ctrl+ArrowUp/Down`
    // branch which already bails on Apple. Return `null` so the
    // key event propagates to whatever else might handle it.
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "PageUp",
          metaKey: false,
          shiftKey: false,
        },
        document.createElement("div"),
        "MacIntel",
      ),
    ).toBeNull();
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "PageDown",
          metaKey: false,
          shiftKey: false,
        },
        document.createElement("div"),
        "MacIntel",
      ),
    ).toBeNull();
  });

  it("keeps Home and End inside textareas even when the caret starts at the boundary", () => {
    const textarea = document.createElement("textarea");
    textarea.value = "first\nsecond";
    textarea.setSelectionRange(0, 0);

    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: false,
          key: "Home",
          metaKey: false,
          shiftKey: false,
        },
        textarea,
        "Win32",
      ),
    ).toBeNull();
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: false,
          key: "End",
          metaKey: false,
          shiftKey: false,
        },
        textarea,
        "Win32",
      ),
    ).toBeNull();
  });

  it("maps Ctrl+Home and Ctrl+End from textareas to conversation boundaries", () => {
    const textarea = document.createElement("textarea");
    textarea.value = "first\nsecond";
    textarea.setSelectionRange(0, 0);

    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "Home",
          metaKey: false,
          shiftKey: false,
        },
        textarea,
        "Win32",
      ),
    ).toEqual({ kind: "boundary", direction: "up" });
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "End",
          metaKey: false,
          shiftKey: false,
        },
        textarea,
        "Win32",
      ),
    ).toEqual({ kind: "boundary", direction: "down" });
  });

  it("does not map Ctrl+ArrowUp on Apple platforms", () => {
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "ArrowUp",
          metaKey: false,
          shiftKey: false,
        },
        document.createElement("div"),
        "MacIntel",
      ),
    ).toBeNull();
  });
});
