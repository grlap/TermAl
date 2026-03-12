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

  it("maps Ctrl+ArrowDown to page scrolling on Windows/Linux", () => {
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
    ).toEqual({ kind: "page", direction: "down" });
  });

  it("maps Shift+Ctrl+ArrowUp to a boundary jump on Windows/Linux", () => {
    expect(
      resolvePaneScrollCommand(
        {
          altKey: false,
          ctrlKey: true,
          key: "ArrowUp",
          metaKey: false,
          shiftKey: true,
        },
        document.createElement("div"),
        "Linux x86_64",
      ),
    ).toEqual({ kind: "boundary", direction: "up" });
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
