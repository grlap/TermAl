// Owns the message-stack DOM event seam and browser keyboard-scroll semantics.
// Does not own pane tail-follow policy, virtualizer reconciliation, or history
// loading; those consumers subscribe to the normalized events defined here.

import { detectBrowserPlatform, isApplePlatform } from "./browser-platform";

export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";
export const MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT =
  "termal:message-stack-bottom-repin-request";
export const MESSAGE_STACK_USER_SCROLL_INTENT_EVENT =
  "termal:message-stack-user-scroll-intent";

export const MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS = 1200;

export type MessageStackScrollWriteKind =
  | "incremental"
  | "page_jump"
  | "seek"
  | "position_restore"
  | "bottom_pin"
  | "bottom_boundary"
  | "bottom_follow";

export type MessageStackScrollWriteSource = "programmatic" | "user";

export type MessageStackScrollWriteDetail = {
  scrollKind?: MessageStackScrollWriteKind;
  scrollSource?: MessageStackScrollWriteSource;
};

export type MessageStackBottomRepinRequestDetail = {
  authorityPresent: boolean;
  beforePaint: boolean;
};

export type MessageStackBottomRepinRequestOptions = {
  beforePaint?: boolean;
};

export type MessageStackUserScrollIntentDetail = {
  detachFromBottomAtBoundary?: boolean;
  direction: "up" | "down";
  scrollKind: Extract<
    MessageStackScrollWriteKind,
    "incremental" | "page_jump"
  >;
  sourceKeyboardEvent?: KeyboardEvent;
  viewportCanMove: boolean;
};

export type MessageStackKeyboardScrollIntent = {
  direction: "up" | "down";
  // Seek remains a classifier result so capture-phase consumers can identify
  // boundary gestures. SessionPaneView consumes it before publication.
  scrollKind: Extract<
    MessageStackScrollWriteKind,
    "incremental" | "page_jump" | "seek"
  >;
};

type MessageStackKeyboardEventLike = {
  altKey: boolean;
  ctrlKey: boolean;
  defaultPrevented: boolean;
  key: string;
  metaKey: boolean;
  shiftKey: boolean;
  target: EventTarget | null;
};

const MESSAGE_STACK_INTERACTIVE_TARGET_SELECTOR = [
  "button",
  "a[href]",
  "input",
  "textarea",
  "select",
  "summary",
  '[contenteditable]:not([contenteditable="false"])',
  '[role="button"]',
  '[role="checkbox"]',
  '[role="combobox"]',
  '[role="link"]',
  '[role="listbox"]',
  '[role="menuitem"]',
  '[role="menuitemcheckbox"]',
  '[role="menuitemradio"]',
  '[role="option"]',
  '[role="radio"]',
  '[role="slider"]',
  '[role="spinbutton"]',
  '[role="switch"]',
  '[role="tab"]',
  '[role="textbox"]',
  '[role="treeitem"]',
  '[role="grid"]',
  '[role="menu"]',
  '[role="menubar"]',
  '[role="radiogroup"]',
  '[role="toolbar"]',
  '[role="tree"]',
  "audio[controls]",
  "video[controls]",
].join(", ");

const MESSAGE_STACK_TEXT_ENTRY_INPUT_TYPES = new Set([
  "",
  "email",
  "password",
  "search",
  "tel",
  "text",
  "url",
]);

function resolveScopedInteractiveMessageStackTarget(
  target: EventTarget | null,
  scopeElement: Element | null,
) {
  if (
    !(target instanceof Element) ||
    !scopeElement ||
    target === scopeElement
  ) {
    return null;
  }

  const interactiveTarget = target.closest(
    MESSAGE_STACK_INTERACTIVE_TARGET_SELECTOR,
  );
  return interactiveTarget && scopeElement.contains(interactiveTarget)
    ? interactiveTarget
    : null;
}

function isInteractiveMessageStackScrollTarget(
  target: EventTarget | null,
  scopeElement: Element | null,
) {
  return resolveScopedInteractiveMessageStackTarget(target, scopeElement) !== null;
}

function isScopedContentEditableMessageStackTarget(
  target: EventTarget | null,
  scopeElement: Element | null,
) {
  if (!(target instanceof Node) || !scopeElement) {
    return false;
  }

  let current: Element | null =
    target instanceof Element ? target : target.parentElement;
  while (current && current !== scopeElement && scopeElement.contains(current)) {
    if (
      current instanceof HTMLElement &&
      (current.isContentEditable || current.contentEditable === "true")
    ) {
      return true;
    }
    if (current.hasAttribute("contenteditable")) {
      const contentEditable = current.getAttribute("contenteditable") ?? "";
      return contentEditable === "" || contentEditable.toLowerCase() === "true";
    }
    current = current.parentElement;
  }
  return false;
}

// Keyboard input is different from pointer ownership: buttons and links are
// interactive pointer targets, but browsers still route unhandled scroll keys
// from them to the nearest scrollable ancestor. Only native text-entry and
// editable targets consume those keys without moving the transcript.
export function messageStackTargetConsumesKey(
  target: EventTarget | null,
  scopeElement: Element | null,
  key: string,
) {
  if (isScopedContentEditableMessageStackTarget(target, scopeElement)) {
    return true;
  }
  const control = resolveScopedInteractiveMessageStackTarget(
    target,
    scopeElement,
  );
  if (!control) {
    return false;
  }
  // Native element default behavior wins over an author-supplied ARIA role:
  // role attributes change accessibility semantics, not browser key handling.
  if (
    control instanceof HTMLTextAreaElement ||
    control instanceof HTMLSelectElement
  ) {
    return true;
  }
  if (
    control instanceof HTMLButtonElement ||
    (control instanceof HTMLElement && control.tagName === "SUMMARY")
  ) {
    return key === " ";
  }
  if (control.localName.toLowerCase() === "a" && control.hasAttribute("href")) {
    // Native links activate with Enter. Scroll keys, including Space, continue
    // to the nearest scrollable ancestor and therefore belong to the transcript.
    return false;
  }
  if (
    (control.localName.toLowerCase() === "audio" ||
      control.localName.toLowerCase() === "video") &&
    control.hasAttribute("controls")
  ) {
    return true;
  }

  if (!(control instanceof HTMLInputElement)) {
    const role = control.getAttribute("role");
    if (role === "textbox") {
      return true;
    }
    if (role === "button" || role === "menuitem") {
      return key === " ";
    }
    if (role === "link") {
      return false;
    }
    if (
      role === "checkbox" ||
      role === "switch" ||
      role === "menuitemcheckbox" ||
      role === "menuitemradio"
    ) {
      return key === " ";
    }
    if (role === "radio") {
      return key === " " || key === "ArrowUp" || key === "ArrowDown";
    }
    if (role === "slider" || role === "spinbutton") {
      return (
        key === "ArrowUp" ||
        key === "ArrowDown" ||
        key === "Home" ||
        key === "End" ||
        key === "PageUp" ||
        key === "PageDown"
      );
    }
    if (
      role === "combobox" ||
      role === "listbox" ||
      role === "option" ||
      role === "tab" ||
      role === "treeitem"
    ) {
      return (
        key === " " ||
        key === "ArrowUp" ||
        key === "ArrowDown" ||
        key === "Home" ||
        key === "End" ||
        key === "PageUp" ||
        key === "PageDown"
      );
    }
    if (
      role === "grid" ||
      role === "menu" ||
      role === "menubar" ||
      role === "radiogroup" ||
      role === "toolbar" ||
      role === "tree"
    ) {
      return (
        key === "ArrowUp" ||
        key === "ArrowDown" ||
        key === "Home" ||
        key === "End"
      );
    }
    // Unknown/custom roles fail open to transcript scrolling.
    return false;
  }

  const inputType = control.type;
  if (inputType === "checkbox") {
    return key === " ";
  }
  if (inputType === "radio") {
    return key === " " || key === "ArrowUp" || key === "ArrowDown";
  }
  if (inputType === "range") {
    return (
      key === "ArrowUp" ||
      key === "ArrowDown" ||
      key === "Home" ||
      key === "End" ||
      key === "PageUp" ||
      key === "PageDown"
    );
  }
  if (
    ["button", "color", "file", "image", "reset", "submit"].includes(
      inputType,
    )
  ) {
    return key === " ";
  }
  if (MESSAGE_STACK_TEXT_ENTRY_INPUT_TYPES.has(inputType)) {
    // Single-line editors keep caret and text-entry keys, but browsers route
    // PageUp/PageDown to the nearest scrollable ancestor.
    return key !== "PageUp" && key !== "PageDown";
  }
  // Number/date/time-like controls own their arrow and boundary keys. Page
  // keys and Space have no native control action, so let the transcript's
  // deterministic scroll pipeline take pre-frame authority.
  return (
    key === "ArrowUp" ||
    key === "ArrowDown" ||
    key === "Home" ||
    key === "End"
  );
}

export function resolveMessageStackKeyboardScrollIntent(
  event: MessageStackKeyboardEventLike,
  scopeElement: Element | null,
  platform = detectBrowserPlatform(),
): MessageStackKeyboardScrollIntent | null {
  const isAppleMetaArrowBoundary =
    event.metaKey &&
    !event.altKey &&
    !event.ctrlKey &&
    !event.shiftKey &&
    isApplePlatform(platform) &&
    (event.key === "ArrowUp" || event.key === "ArrowDown");
  if (
    event.defaultPrevented ||
    event.altKey ||
    event.ctrlKey ||
    (event.metaKey && !isAppleMetaArrowBoundary) ||
    isMessageStackSelectionExtensionKey(event) ||
    messageStackTargetConsumesKey(event.target, scopeElement, event.key)
  ) {
    return null;
  }

  const direction =
    event.key === "ArrowUp" ||
    event.key === "PageUp" ||
    event.key === "Home" ||
    (event.key === " " && event.shiftKey)
      ? "up"
      : event.key === "ArrowDown" ||
          event.key === "PageDown" ||
          event.key === "End" ||
          event.key === " "
        ? "down"
        : null;
  if (!direction) {
    return null;
  }
  return {
    direction,
    scrollKind:
      event.key === "Home" || event.key === "End" || isAppleMetaArrowBoundary
        ? "seek"
        : event.key === "PageUp" ||
            event.key === "PageDown" ||
            event.key === " "
          ? "page_jump"
          : "incremental",
  };
}

export function isMessageStackSelectionExtensionKey(event: {
  key: string;
  shiftKey: boolean;
}) {
  return (
    event.shiftKey &&
    (event.key === "ArrowUp" ||
      event.key === "ArrowDown" ||
      event.key === "Home" ||
      event.key === "End" ||
      event.key === "PageUp" ||
      event.key === "PageDown")
  );
}

export function messageStackOwnsBodyKeyboardScroll(
  target: EventTarget | null,
  node: HTMLElement | null,
) {
  return Boolean(
    node &&
      target instanceof Node &&
      node.contains(target) &&
      !isInteractiveMessageStackScrollTarget(target, node),
  );
}

// Shared seam between pane-owned transcript scroll intent and the virtualizer's
// reconciliation path. Producers normally dispatch this immediately after any
// direct message-stack `scrollTop` / `scrollTo` write. `bottom_pin` tells the
// virtualizer an already-sticky programmatic restore should mount the bottom
// range without the boundary reveal loop. `bottom_boundary` asks the virtualizer
// to mount the bottom range first, then perform the scroll after the target
// pages exist. `bottom_follow` marks the pane's frame-by-frame programmatic
// follow; the pane and virtualizer keep bottom-stick state while those bounded
// writes pass through intermediate positions. `position_restore` preserves an
// explicitly detached saved viewport while virtualized geometry converges.
// `scrollSource: "user"` is reserved for direct
// pane writes that are synchronously caused by an input event; layout-effect
// restores and other programmatic writes should omit it so the virtualizer never
// calls `flushSync` from a React lifecycle.
export function notifyMessageStackScrollWrite(
  node: HTMLElement,
  detail?: MessageStackScrollWriteDetail,
) {
  node.dispatchEvent(
    new CustomEvent<MessageStackScrollWriteDetail>(
      MESSAGE_STACK_SCROLL_WRITE_EVENT,
      {
        detail,
      },
    ),
  );
}

// Keyboard scrolling can remain browser-owned by a transcript even when the
// key event itself targets document.body. Publish that pre-scroll intent on the
// real scroll node so pane persistence and virtualized layout arbitration drop
// bottom authority before Blink emits its first animated native-scroll frame.
// History demand defers any resulting page request until a microtask, after all
// synchronous listeners (including virtualized layout arbitration) have seen
// this event. Listener registration order is therefore not an authority rule.
export function notifyMessageStackUserScrollIntent(
  node: HTMLElement,
  detail: MessageStackUserScrollIntentDetail,
) {
  // Intentionally non-bubbling: both the virtualizer and history-demand hooks
  // subscribe to the exact scroll node whose geometry produced `viewportCanMove`.
  node.dispatchEvent(
    new CustomEvent<MessageStackUserScrollIntentDetail>(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      { detail },
    ),
  );
}

export function writeMessageStackScrollTopImmediately(
  node: HTMLElement,
  top: number,
) {
  // A plain scrollTop assignment does not reliably take ownership from an
  // in-flight native smooth-scroll animation in Blink. Abort that animation
  // with an explicit auto landing before publishing the synchronous value
  // that scroll persistence and virtualizer reconciliation read back.
  if (typeof node.scrollTo === "function") {
    node.scrollTo({ top, behavior: "auto" });
  }
  node.scrollTop = top;
}

// Synchronous request seam for layout owners that need to preserve an existing
// bottom pin without becoming another transcript scroll writer. Requests may
// start on the message stack or bubble from a descendant layout owner;
// SessionPaneView handles them through its scroll authority, including explicit
// tail-follow intent, saved pane position, and virtualizer notification.
export function requestMessageStackBottomRepin(
  node: HTMLElement,
  options: MessageStackBottomRepinRequestOptions = {},
) {
  const detail: MessageStackBottomRepinRequestDetail = {
    authorityPresent: false,
    beforePaint: options.beforePaint === true,
  };
  node.dispatchEvent(
    new CustomEvent<MessageStackBottomRepinRequestDetail>(
      MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
      { bubbles: true, detail },
    ),
  );
  return detail.authorityPresent;
}
