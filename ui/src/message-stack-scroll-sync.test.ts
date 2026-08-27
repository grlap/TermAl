// Owns focused tests for the shared message-stack DOM scroll-write seam.
// Does not own pane intent, virtualizer reconciliation, or drag/drop policy.

import { describe, expect, it, vi } from "vitest";

import {
  messageStackOwnsBodyKeyboardScroll,
  resolveMessageStackKeyboardScrollIntent,
  writeMessageStackScrollTopImmediately,
} from "./message-stack-scroll-sync";

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
