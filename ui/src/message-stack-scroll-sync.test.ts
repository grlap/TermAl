// Owns focused tests for the shared message-stack DOM scroll-write seam.
// Does not own pane intent, virtualizer reconciliation, or drag/drop policy.

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  armMessageStackFocusScrollOwnership,
  clearMessageStackNativeScrollOwnership,
  claimMessageStackPointerScrollOwnership,
  claimMessageStackNativeScrollOwnership,
  consumeMessageStackVirtualizerPositionCorrection,
  isMessageStackSelectionExtensionKey,
  peekMessageStackNativeScrollOwnership,
  markMessageStackVirtualizerPositionCorrection,
  messageStackOwnsBodyKeyboardScroll,
  nativeScrollPreservesFollowAuthority,
  notifyMessageStackScrollWrite,
  observeMessageStackPointerOwnershipRelease,
  readMessageStackNativeScrollOwnershipForEvent,
  resolveMessageStackKeyboardScrollIntent,
  resolveMessageStackWheelRouting,
  revokeMessageStackNativeScrollOwnershipOnConflict,
  writeMessageStackScrollTopImmediately,
} from "./message-stack-scroll-sync";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("native scroll follow authority", () => {
  it("does not treat an owned zero-movement frame as a new escape", () => {
    expect(nativeScrollPreservesFollowAuthority({
      tailFollowIntent: true, isDetachedFromBottom: false,
      ownership: { owner: "pointer", direction: null }, scrollDelta: 0,
    })).toBe(true);
  });
  it.each(["focus", "keyboard", "pointer", "touch", "wheel"] as const)(
    "requires a matching %s owner to create an escape", (owner) => {
      const attached = { tailFollowIntent: true, isDetachedFromBottom: false, scrollDelta: -40 };
      expect(nativeScrollPreservesFollowAuthority({ ...attached, ownership: null })).toBe(true);
      expect(nativeScrollPreservesFollowAuthority({
        ...attached, ownership: { owner, direction: "up" },
      })).toBe(false);
      expect(nativeScrollPreservesFollowAuthority({
        ...attached, ownership: { owner, direction: "down" },
      })).toBe(true);
    },
  );

  it.each(["pointer", "touch"] as const)("accepts undirected %s ownership", (owner) => {
    expect(nativeScrollPreservesFollowAuthority({
      tailFollowIntent: true, isDetachedFromBottom: false,
      ownership: { owner, direction: null }, scrollDelta: -40,
    })).toBe(false);
  });

  it.each([[false, false], [false, true], [true, true]])(
    "never grants FOLLOW to a detached reader (follow=%s, detached=%s)", (tailFollowIntent, isDetachedFromBottom) => {
      expect(nativeScrollPreservesFollowAuthority({
        tailFollowIntent, isDetachedFromBottom, ownership: null, scrollDelta: -40,
      })).toBe(false);
    },
  );
});

describe("focus movement evidence", () => {
  function focusFixture() {
    const node = document.createElement("section");
    const target = document.createElement("button");
    node.append(target);
    document.body.append(node);
    target.focus({ preventScroll: true });
    node.scrollTop = 800;
    const releaseObserver = observeMessageStackPointerOwnershipRelease(node);
    return { node, target, cleanup() { releaseObserver(); node.remove(); } };
  }

  it("consumes one proven landing across both listeners, not a second event", async () => {
    const view = focusFixture();
    try {
      armMessageStackFocusScrollOwnership(view.node, view.target);
      view.node.scrollTop = 600;
      await Promise.resolve();
      vi.spyOn(performance, "now").mockReturnValue(performance.now() + 10_000);
      const event = new Event("scroll");
      const expected = { owner: "focus", direction: "up" };
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, event)).toEqual(expected);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, event)).toEqual(expected);
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, new Event("scroll"))).toBeNull();
    } finally { view.cleanup(); }
  });

  it.each(["write", "window blur", "different landing", "target removed"] as const)("invalidates a proven landing after %s", async (change) => {
    const view = focusFixture();
    try {
      armMessageStackFocusScrollOwnership(view.node, view.target);
      view.node.scrollTop = 600;
      await Promise.resolve();
      if (change === "write") notifyMessageStackScrollWrite(view.node, { scrollKind: "bottom_pin" });
      if (change === "window blur") window.dispatchEvent(new Event("blur"));
      if (change === "different landing") view.node.scrollTop = 400;
      if (change === "target removed") view.target.remove();
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, new Event("scroll"))).toBeNull();
    } finally { view.cleanup(); }
  });

  it("does not let a stale focus microtask overwrite a newer input", async () => {
    const view = focusFixture();
    try {
      armMessageStackFocusScrollOwnership(view.node, view.target);
      view.node.scrollTop = 600;
      claimMessageStackNativeScrollOwnership(view.node, { owner: "wheel", direction: "down" }, 120);
      await Promise.resolve();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toEqual({ owner: "wheel", direction: "down" });
    } finally { view.cleanup(); }
  });

  it.each(["wheel", "keyboard", "touch"] as const)("preserves prior %s authority across no-movement focus", async (owner) => {
    const view = focusFixture();
    try {
      const ownership = { owner, direction: "down" as const };
      claimMessageStackNativeScrollOwnership(view.node, ownership, 1_200);
      armMessageStackFocusScrollOwnership(view.node, view.target);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toEqual(ownership);
      await Promise.resolve();
      expect(peekMessageStackNativeScrollOwnership(view.node)).toEqual(ownership);
      view.node.scrollTop = 820;
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, new Event("scroll"))).toEqual(ownership);
    } finally { view.cleanup(); }
  });

  it("does not extend the expiry of prior input when focus did not move", async () => {
    const view = focusFixture();
    let now = 0;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    try {
      claimMessageStackNativeScrollOwnership(view.node, { owner: "wheel", direction: "down" }, 120);
      now = 119;
      armMessageStackFocusScrollOwnership(view.node, view.target);
      await Promise.resolve();
      now = 121;
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
    } finally { view.cleanup(); }
  });

  it.each(["wheel", "keyboard", "touch"] as const)("replaces prior %s authority only after focus movement is proven", async (owner) => {
    const view = focusFixture();
    const onMovement = vi.fn();
    try {
      claimMessageStackNativeScrollOwnership(view.node, { owner, direction: "down" }, 1_200);
      armMessageStackFocusScrollOwnership(view.node, view.target, onMovement);
      view.node.scrollTop = 600;
      expect(onMovement).not.toHaveBeenCalled();
      await Promise.resolve();
      expect(onMovement).toHaveBeenCalledExactlyOnceWith("up");
      const event = new Event("scroll");
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, event)).toEqual({ owner: "focus", direction: "up" });
      readMessageStackNativeScrollOwnershipForEvent(view.node, event);
      expect(onMovement).toHaveBeenCalledTimes(1);
    } finally { view.cleanup(); }
  });

  it.each(["write", "window blur", "new input", "scope teardown"] as const)("does not publish pending focus after %s", async (change) => {
    const view = focusFixture();
    const onMovement = vi.fn();
    try {
      armMessageStackFocusScrollOwnership(view.node, view.target, onMovement);
      view.node.scrollTop = 600;
      if (change === "write") notifyMessageStackScrollWrite(view.node);
      if (change === "window blur") window.dispatchEvent(new Event("blur"));
      if (change === "new input") claimMessageStackNativeScrollOwnership(view.node, { owner: "wheel", direction: "down" }, 120);
      if (change === "scope teardown") clearMessageStackNativeScrollOwnership(view.node);
      await Promise.resolve();
      expect(onMovement).not.toHaveBeenCalled();
      expect(peekMessageStackNativeScrollOwnership(view.node)?.owner).not.toBe("focus");
    } finally { view.cleanup(); }
  });

  it("revokes the consumed focus snapshot for every later observer of the same event", async () => {
    const view = focusFixture();
    try {
      armMessageStackFocusScrollOwnership(view.node, view.target);
      view.node.scrollTop = 600;
      await Promise.resolve();
      const event = new Event("scroll");
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, event)).toEqual({ owner: "focus", direction: "up" });
      expect(revokeMessageStackNativeScrollOwnershipOnConflict(view.node, 40, event)).toBe(true);
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, event)).toBeNull();
      expect(readMessageStackNativeScrollOwnershipForEvent(view.node, new Event("scroll"))).toBeNull();
    } finally { view.cleanup(); }
  });

  it.each(["mouseup", "pointerup", "pointercancel", "lostpointercapture", "contextmenu", "dragend", "blur", "cleanup"] as const)("retains a focused descendant drag until %s", async (release) => {
    const view = focusFixture();
    try {
      claimMessageStackPointerScrollOwnership(view.node);
      armMessageStackFocusScrollOwnership(view.node, view.target);
      await Promise.resolve();
      vi.spyOn(performance, "now").mockReturnValue(performance.now() + 10_000);
      expect(peekMessageStackNativeScrollOwnership(view.node)).toEqual({ owner: "pointer", direction: null });
      if (release === "cleanup") view.cleanup();
      else if (release === "blur") window.dispatchEvent(new Event(release));
      else if (release === "lostpointercapture") view.node.dispatchEvent(new Event(release));
      else document.dispatchEvent(new Event(release));
      expect(peekMessageStackNativeScrollOwnership(view.node)).toBeNull();
    } finally { view.cleanup(); }
  });
});

describe("message-stack keyboard ownership", () => {
  it("distinguishes embedded controls from transcript reading surfaces", () => {
    const stack = document.createElement("section");
    const message = document.createElement("article");
    const input = document.createElement("textarea");
    message.append(input);
    stack.append(message);

    expect(messageStackOwnsBodyKeyboardScroll(input, stack)).toBe(false);
    expect(messageStackOwnsBodyKeyboardScroll(message, stack)).toBe(true);
    expect(messageStackOwnsBodyKeyboardScroll(stack, stack)).toBe(true);
  });
});

describe("resolveMessageStackKeyboardScrollIntent", () => {
  it("classifies scroll intent by both key and native control semantics", () => {
    const stack = document.createElement("section");
    const textarea = document.createElement("textarea");
    const button = document.createElement("button");
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    const radio = document.createElement("input");
    radio.type = "radio";
    const range = document.createElement("input");
    range.type = "range";
    const disabledEditing = document.createElement("div");
    disabledEditing.contentEditable = "false";
    const editableButton = document.createElement("div");
    editableButton.contentEditable = "true";
    editableButton.setAttribute("role", "button");
    const nestedEditableButton = document.createElement("span");
    nestedEditableButton.setAttribute("role", "button");
    editableButton.append(nestedEditableButton);
    const slider = document.createElement("div");
    slider.setAttribute("role", "slider");
    const customCheckbox = document.createElement("div");
    customCheckbox.setAttribute("role", "checkbox");
    const anchor = document.createElement("a");
    anchor.href = "#target";
    const svgAnchor = document.createElementNS(
      "http://www.w3.org/2000/svg",
      "a",
    );
    svgAnchor.setAttribute("href", "#diagram-target");
    const roleLink = document.createElement("span");
    roleLink.setAttribute("role", "link");
    const summary = document.createElement("summary");
    const select = document.createElement("select");
    const submit = document.createElement("input");
    submit.type = "submit";
    const number = document.createElement("input");
    number.type = "number";
    const textInput = document.createElement("input");
    textInput.type = "text";
    const roleTargets = [
      "textbox",
      "combobox",
      "listbox",
      "option",
      "tab",
      "treeitem",
      "spinbutton",
    ].map((role) => {
      const target = document.createElement("div");
      target.setAttribute("role", role);
      return target;
    });
    const activationRoleTargets = [
      "button",
      "menuitem",
      "switch",
      "menuitemcheckbox",
      "menuitemradio",
      "radio",
    ].map((role) => {
      const target = document.createElement("div");
      target.setAttribute("role", role);
      return target;
    });
    const compositeWidgets = [
      "grid",
      "menu",
      "menubar",
      "radiogroup",
      "toolbar",
      "tree",
    ].map((role) => {
      const target = document.createElement("div");
      target.setAttribute("role", role);
      return target;
    });
    const video = document.createElement("video");
    video.controls = true;
    const audio = document.createElement("audio");
    audio.controls = true;
    const activationInputs = ["button", "color", "file", "image", "reset"].map(
      (type) => {
        const target = document.createElement("input");
        target.type = type;
        return target;
      },
    );
    stack.append(
      textarea,
      button,
      checkbox,
      radio,
      range,
      disabledEditing,
      editableButton,
      slider,
      customCheckbox,
      anchor,
      svgAnchor,
      roleLink,
      summary,
      select,
      submit,
      number,
      textInput,
      ...activationRoleTargets,
      ...compositeWidgets,
      video,
      audio,
      ...activationInputs,
      ...roleTargets,
    );

    const resolve = (
      target: EventTarget,
      key: string,
      shiftKey = false,
    ) =>
      resolveMessageStackKeyboardScrollIntent({
        altKey: false,
        ctrlKey: false,
        defaultPrevented: false,
        key,
        metaKey: false,
        shiftKey,
        target,
      }, stack);

    expect(resolve(textarea, "ArrowUp")).toBeNull();
    expect(resolve(button, " ")).toBeNull();
    expect(resolve(button, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(checkbox, " ")).toBeNull();
    expect(resolve(checkbox, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(radio, "ArrowUp")).toBeNull();
    expect(resolve(range, "PageUp")).toBeNull();
    expect(resolve(editableButton, "Home")).toBeNull();
    expect(resolve(editableButton, "PageUp")).toBeNull();
    expect(resolve(nestedEditableButton, "Home")).toBeNull();
    expect(resolve(slider, "Home")).toBeNull();
    expect(resolve(customCheckbox, " ")).toBeNull();
    expect(resolve(anchor, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(anchor, " ")).toEqual({
      direction: "down",
      scrollKind: "page_jump",
    });
    expect(resolve(svgAnchor, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(roleLink, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(roleLink, " ")).toEqual({
      direction: "down",
      scrollKind: "page_jump",
    });
    expect(resolve(summary, " ")).toBeNull();
    expect(resolve(summary, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(select, "ArrowUp")).toBeNull();
    expect(resolve(submit, " ")).toBeNull();
    expect(resolve(submit, "ArrowUp")).toEqual({
      direction: "up",
      scrollKind: "incremental",
    });
    expect(resolve(number, "Home")).toBeNull();
    expect(resolve(number, "PageUp")).toEqual({
      direction: "up",
      scrollKind: "page_jump",
    });
    expect(resolve(number, " ")).toEqual({
      direction: "down",
      scrollKind: "page_jump",
    });
    expect(resolve(textInput, " ")).toBeNull();
    expect(resolve(textInput, "PageUp")).toEqual({
      direction: "up",
      scrollKind: "page_jump",
    });
    for (const roleTarget of roleTargets) {
      expect(resolve(roleTarget, "ArrowUp")).toBeNull();
    }
    for (const roleTarget of activationRoleTargets) {
      expect(resolve(roleTarget, " ")).toBeNull();
    }
    for (const roleTarget of activationRoleTargets.slice(0, 5)) {
      expect(resolve(roleTarget, "ArrowUp")).toEqual({
        direction: "up",
        scrollKind: "incremental",
      });
    }
    expect(resolve(activationRoleTargets[5], "ArrowUp")).toBeNull();
    expect(resolve(activationRoleTargets[5], "PageUp")).toEqual({
      direction: "up",
      scrollKind: "page_jump",
    });
    for (const compositeWidget of compositeWidgets) {
      expect(resolve(compositeWidget, "ArrowUp")).toBeNull();
      expect(resolve(compositeWidget, "PageUp")).toEqual({
        direction: "up",
        scrollKind: "page_jump",
      });
    }
    for (const activationInput of activationInputs) {
      expect(resolve(activationInput, " ")).toBeNull();
      expect(resolve(activationInput, "ArrowUp")).toEqual({
        direction: "up",
        scrollKind: "incremental",
      });
    }
    expect(resolve(disabledEditing, " ", true)).toEqual({
      direction: "up",
      scrollKind: "page_jump",
    });
    expect(resolve(stack, "Home")).toEqual({
      direction: "up",
      scrollKind: "seek",
    });
    expect(resolve(stack, "End")).toEqual({
      direction: "down",
      scrollKind: "seek",
    });
    expect(resolve(stack, "Home", true)).toBeNull();
    expect(resolve(stack, "ArrowUp", true)).toBeNull();
    expect(resolve(stack, "PageUp", true)).toBeNull();
    expect(
      resolveMessageStackKeyboardScrollIntent(
        {
          altKey: false,
          ctrlKey: false,
          defaultPrevented: false,
          key: "ArrowUp",
          metaKey: true,
          shiftKey: false,
          target: stack,
        },
        stack,
        "MacIntel",
      ),
    ).toEqual({ direction: "up", scrollKind: "seek" });
    expect(
      resolveMessageStackKeyboardScrollIntent(
        {
          altKey: false,
          ctrlKey: false,
          defaultPrevented: false,
          key: "ArrowDown",
          metaKey: true,
          shiftKey: false,
          target: stack,
        },
        stack,
        "MacIntel",
      ),
    ).toEqual({ direction: "down", scrollKind: "seek" });
    expect(
      resolveMessageStackKeyboardScrollIntent(
        {
          altKey: false,
          ctrlKey: false,
          defaultPrevented: false,
          key: "ArrowUp",
          metaKey: true,
          shiftKey: false,
          target: textarea,
        },
        stack,
        "MacIntel",
      ),
    ).toBeNull();
    expect(
      resolveMessageStackKeyboardScrollIntent(
        {
          altKey: false,
          ctrlKey: false,
          defaultPrevented: false,
          key: "ArrowUp",
          metaKey: true,
          shiftKey: false,
          target: stack,
        },
        stack,
        "Win32",
      ),
    ).toBeNull();
    expect(resolve(video, "ArrowUp")).toBeNull();
    expect(resolve(audio, "ArrowUp")).toBeNull();
    expect(resolve(stack, " ")).toEqual({
      direction: "down",
      scrollKind: "page_jump",
    });
    expect(messageStackOwnsBodyKeyboardScroll(button, stack)).toBe(false);
    expect(messageStackOwnsBodyKeyboardScroll(anchor, stack)).toBe(false);
    expect(messageStackOwnsBodyKeyboardScroll(roleLink, stack)).toBe(false);
    expect(messageStackOwnsBodyKeyboardScroll(slider, stack)).toBe(false);
    expect(messageStackOwnsBodyKeyboardScroll(disabledEditing, stack)).toBe(
      true,
    );
  });

  it("uses an explicit transcript scope for document-listener events", () => {
    const stack = document.createElement("section");
    const button = document.createElement("button");
    stack.append(button);
    const event = new KeyboardEvent("keydown", {
      bubbles: true,
      key: " ",
    });
    Object.defineProperty(event, "target", { value: button });

    expect(resolveMessageStackKeyboardScrollIntent(event, stack)).toBeNull();
  });
});

describe("writeMessageStackScrollTopImmediately", () => {
  it("aborts native smooth scrolling before publishing the synchronous landing", () => {
    const node = document.createElement("section");
    let currentTop = 900;
    const writes: string[] = [];
    Object.defineProperty(node, "scrollTop", {
      configurable: true,
      get: () => currentTop,
      set: (top: number) => {
        writes.push(`assign:${top}`);
        currentTop = top;
      },
    });
    node.scrollTo = vi.fn((optionsOrX?: ScrollToOptions | number, y?: number) => {
      const top =
        typeof optionsOrX === "object" && optionsOrX !== null
          ? optionsOrX.top
          : y;
      writes.push(`auto:${top}`);
    }) as typeof node.scrollTo;

    writeMessageStackScrollTopImmediately(node, 321);

    expect(node.scrollTo).toHaveBeenCalledWith({
      top: 321,
      behavior: "auto",
    });
    expect(writes).toEqual(["auto:321", "assign:321"]);
    expect(node.scrollTop).toBe(321);
  });
});

describe("message-stack native scroll ownership", () => {
  it("caches the nested-scrollable routing decision per native wheel event", () => {
    const node = document.createElement("section");
    const nested = document.createElement("div");
    nested.style.overflowY = "auto";
    Object.defineProperties(nested, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 300 },
      scrollTop: { configurable: true, writable: true, value: 40 },
    });
    node.append(nested);
    const getComputedStyleSpy = vi.spyOn(window, "getComputedStyle");
    const results: ReturnType<typeof resolveMessageStackWheelRouting>[] = [];
    node.addEventListener("wheel", (event) => {
      results.push(resolveMessageStackWheelRouting(event, node));
      results.push(resolveMessageStackWheelRouting(event, node));
    });

    nested.dispatchEvent(
      new WheelEvent("wheel", { bubbles: true, deltaY: 40 }),
    );

    expect(results).toHaveLength(2);
    expect(results[0]).toBe(results[1]);
    expect(results[0]).toMatchObject({
      deltaY: 40,
      nestedScrollableConsumes: true,
    });
    expect(getComputedStyleSpy).toHaveBeenCalledTimes(1);
  });

  it("shares a bounded owner lease and rejects an opposite-direction frame", () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const node = document.createElement("section");

    claimMessageStackNativeScrollOwnership(
      node,
      { direction: "down", owner: "wheel" },
      100,
    );
    expect(peekMessageStackNativeScrollOwnership(node)).toEqual({
      direction: "down",
      owner: "wheel",
    });
    expect(peekMessageStackNativeScrollOwnership(node)).toEqual({
      direction: "down",
      owner: "wheel",
    });
    expect(
      revokeMessageStackNativeScrollOwnershipOnConflict(node, -40),
    ).toBe(true);
    expect(peekMessageStackNativeScrollOwnership(node)).toBeNull();

    claimMessageStackNativeScrollOwnership(
      node,
      { direction: null, owner: "pointer" },
      100,
    );
    now += 101;
    expect(peekMessageStackNativeScrollOwnership(node)).toBeNull();
  });

  it("discards a virtualizer correction after its first nonmatching native frame", () => {
    const node = document.createElement("section");
    Object.defineProperty(node, "scrollTop", {
      configurable: true,
      writable: true,
      value: 50,
    });

    markMessageStackVirtualizerPositionCorrection(node, 100);
    expect(consumeMessageStackVirtualizerPositionCorrection(node)).toBe(false);
    node.scrollTop = 100;
    expect(consumeMessageStackVirtualizerPositionCorrection(node)).toBe(false);
  });

  it("consumes a matching virtualizer correction on its first native frame", () => {
    const node = document.createElement("section");
    Object.defineProperty(node, "scrollTop", {
      configurable: true,
      writable: true,
      value: 100,
    });

    markMessageStackVirtualizerPositionCorrection(node, 100);
    expect(consumeMessageStackVirtualizerPositionCorrection(node)).toBe(true);
    expect(consumeMessageStackVirtualizerPositionCorrection(node)).toBe(false);
  });
});

describe("isMessageStackSelectionExtensionKey", () => {
  const key = (overrides: {
    ctrlKey?: boolean;
    key: string;
    metaKey?: boolean;
    shiftKey?: boolean;
  }) => ({
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    ...overrides,
  });

  it("claims plain shifted navigation keys for browser selection extension", () => {
    for (const name of [
      "ArrowUp",
      "ArrowDown",
      "Home",
      "End",
      "PageUp",
      "PageDown",
    ]) {
      expect(
        isMessageStackSelectionExtensionKey(key({ key: name, shiftKey: true })),
      ).toBe(true);
      expect(isMessageStackSelectionExtensionKey(key({ key: name }))).toBe(
        false,
      );
    }
  });

  it("releases only Ctrl- and Cmd-modified shifted arrows to the pane boundary shortcuts", () => {
    // Ctrl+Shift+ArrowDown must reach resolvePaneScrollCommand and jump to
    // the transcript bottom instead of being handed to the browser as a
    // paragraph-selection gesture.
    for (const name of ["ArrowUp", "ArrowDown"]) {
      expect(
        isMessageStackSelectionExtensionKey(
          key({ ctrlKey: true, key: name, shiftKey: true }),
        ),
      ).toBe(false);
      expect(
        isMessageStackSelectionExtensionKey(
          key({ key: name, metaKey: true, shiftKey: true }),
        ),
      ).toBe(false);
    }
  });

  it("keeps Ctrl/Cmd+Shift+Home/End and shifted Page keys browser-owned for selection", () => {
    // Select-to-document-boundary and select-by-page are standard selection
    // gestures inside transcript content on every platform; the pane must
    // not turn them into boundary jumps.
    for (const name of ["Home", "End", "PageUp", "PageDown"]) {
      expect(
        isMessageStackSelectionExtensionKey(
          key({ ctrlKey: true, key: name, shiftKey: true }),
        ),
      ).toBe(true);
      expect(
        isMessageStackSelectionExtensionKey(
          key({ key: name, metaKey: true, shiftKey: true }),
        ),
      ).toBe(true);
    }
  });
});
