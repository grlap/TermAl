// Owns the message-stack DOM event seam, browser keyboard-scroll semantics, and
// the short-lived DOM authority metadata shared by the pane and virtualizer.
// Does not decide pane tail-follow policy, virtualizer reconciliation, or
// history loading; those consumers interpret the normalized events and leases.

import { detectBrowserPlatform, isApplePlatform } from "./browser-platform";
import {
  canNestedScrollableConsumeWheel,
  normalizeWheelDelta,
} from "./app-utils";

export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";
export const MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT =
  "termal:message-stack-bottom-repin-request";
export const MESSAGE_STACK_USER_SCROLL_INTENT_EVENT =
  "termal:message-stack-user-scroll-intent";

export const MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS = 1200;
export const MESSAGE_STACK_FOCUS_OWNERSHIP_MS = 400;
export const MESSAGE_STACK_KEYBOARD_OWNERSHIP_MS =
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS;
export const MESSAGE_STACK_POINTER_OWNERSHIP_MS = 5_000;
export const MESSAGE_STACK_WHEEL_OWNERSHIP_MS = 120;

// Some browsers make later WheelEvents in one gesture non-cancelable. The
// pane capture arbiter still needs to revoke those stale events before any
// bubble-phase pane, React, or virtualizer consumer mutates scroll authority.
// A WeakSet carries that same-event decision without retaining the event.
const suppressedMessageStackWheelEvents = new WeakSet<Event>();

type MessageStackWheelRouting = {
  container: HTMLElement;
  deltaY: number;
  nestedScrollableConsumes: boolean;
};

const messageStackWheelRoutingByEvent = new WeakMap<
  WheelEvent,
  MessageStackWheelRouting
>();

// Capture, bubble, React, and virtualizer listeners all inspect the same native
// WheelEvent. Cache the layout-sensitive nested-scroller walk on that event so
// every consumer reaches one classification without repeating style reads.
export function resolveMessageStackWheelRouting(
  event: WheelEvent,
  container: HTMLElement,
) {
  const cached = messageStackWheelRoutingByEvent.get(event);
  if (cached?.container === container) {
    return cached;
  }
  const deltaY = normalizeWheelDelta(event, container);
  const routing = {
    container,
    deltaY,
    nestedScrollableConsumes:
      Math.abs(deltaY) >= 0.5 &&
      canNestedScrollableConsumeWheel(event.target, container, deltaY),
  };
  messageStackWheelRoutingByEvent.set(event, routing);
  return routing;
}

export function markMessageStackWheelEventSuppressed(event: WheelEvent) {
  suppressedMessageStackWheelEvents.add(event);
}

export function isMessageStackWheelEventSuppressed(event: Event) {
  return suppressedMessageStackWheelEvents.has(event);
}

export type MessageStackNativeScrollOwner =
  | "focus"
  | "keyboard"
  | "pointer"
  | "touch"
  | "wheel";

export type MessageStackNativeScrollOwnership = {
  direction: "down" | "up" | null;
  owner: MessageStackNativeScrollOwner;
};

type MessageStackNativeScrollOwnershipLease =
  MessageStackNativeScrollOwnership & {
    expiresAt: number;
  };

const messageStackNativeScrollOwnership = new WeakMap<
  HTMLElement,
  MessageStackNativeScrollOwnershipLease
>();

type MessageStackPointerReleaseObserver = {
  cleanup: () => void;
  subscribers: number;
};

const messageStackPointerReleaseObservers = new WeakMap<
  HTMLElement,
  MessageStackPointerReleaseObserver
>();

function messageStackScrollNow() {
  return globalThis.performance?.now() ?? Date.now();
}

export function claimMessageStackNativeScrollOwnership(
  node: HTMLElement,
  ownership: MessageStackNativeScrollOwnership,
  durationMs: number,
) {
  messageStackNativeScrollOwnership.set(node, {
    ...ownership,
    expiresAt: messageStackScrollNow() + Math.max(durationMs, 0),
  });
}

export function clearMessageStackNativeScrollOwnership(
  node: HTMLElement,
  owner?: MessageStackNativeScrollOwner,
) {
  const current = messageStackNativeScrollOwnership.get(node);
  if (!current || (owner !== undefined && current.owner !== owner)) {
    return;
  }
  messageStackNativeScrollOwnership.delete(node);
}

// Pointer ownership can survive React listener re-registration while a
// scrollbar drag is still in flight. Keep one node-scoped release observer and
// reference-count its consumers so rerenders cannot manufacture a lost mouseup.
export function observeMessageStackPointerOwnershipRelease(
  node: HTMLElement,
) {
  let observer = messageStackPointerReleaseObservers.get(node);
  if (!observer) {
    const ownerDocument = node.ownerDocument;
    const ownerWindow = ownerDocument.defaultView;
    const release = () => {
      clearMessageStackNativeScrollOwnership(node, "pointer");
    };
    node.addEventListener("lostpointercapture", release);
    ownerDocument.addEventListener("mouseup", release);
    ownerDocument.addEventListener("pointerup", release);
    ownerDocument.addEventListener("pointercancel", release);
    ownerWindow?.addEventListener("blur", release);
    observer = {
      cleanup: () => {
        node.removeEventListener("lostpointercapture", release);
        ownerDocument.removeEventListener("mouseup", release);
        ownerDocument.removeEventListener("pointerup", release);
        ownerDocument.removeEventListener("pointercancel", release);
        ownerWindow?.removeEventListener("blur", release);
      },
      subscribers: 0,
    };
    messageStackPointerReleaseObservers.set(node, observer);
  }
  observer.subscribers += 1;

  let active = true;
  return () => {
    if (!active) {
      return;
    }
    active = false;
    const current = messageStackPointerReleaseObservers.get(node);
    if (!current) {
      return;
    }
    current.subscribers -= 1;
    if (current.subscribers <= 0) {
      current.cleanup();
      messageStackPointerReleaseObservers.delete(node);
    }
  };
}

export function peekMessageStackNativeScrollOwnership(
  node: HTMLElement,
): MessageStackNativeScrollOwnership | null {
  const current = messageStackNativeScrollOwnership.get(node);
  if (!current) {
    return null;
  }
  if (current.expiresAt < messageStackScrollNow()) {
    messageStackNativeScrollOwnership.delete(node);
    return null;
  }
  return {
    direction: current.direction,
    owner: current.owner,
  };
}

// The virtualizer native listener owns the true per-tick delta. It is the only
// consumer allowed to revoke a lease for directional conflict; pane and input
// observers only peek, so listener order cannot make them delete shared state.
export function revokeMessageStackNativeScrollOwnershipOnConflict(
  node: HTMLElement,
  scrollDelta: number,
) {
  const current = peekMessageStackNativeScrollOwnership(node);
  if (!current) {
    return false;
  }
  const observedDirection =
    scrollDelta < -0.5 ? "up" : scrollDelta > 0.5 ? "down" : null;
  if (
    current.direction !== null &&
    observedDirection !== null &&
    current.direction !== observedDirection
  ) {
    messageStackNativeScrollOwnership.delete(node);
    return true;
  }
  return false;
}

export function messageStackNativeScrollOwnershipMovesTowardBottom(
  ownership: MessageStackNativeScrollOwnership | null,
) {
  return Boolean(
    ownership &&
      (ownership.direction === "down" ||
        ownership.owner === "pointer" ||
        ownership.owner === "touch"),
  );
}

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

const pendingVirtualizerPositionCorrections = new WeakMap<
  HTMLElement,
  number
>();

// Virtualized height/anchor reconciliation can legitimately change scrollTop
// while pane tail-follow remains detached. Mark that one pending native scroll
// so the pane does not confuse it with a late tick from a canceled smooth
// bottom-follow animation and rewind the corrected visible anchor.
export function markMessageStackVirtualizerPositionCorrection(
  node: HTMLElement,
  targetScrollTop: number,
) {
  if (!Number.isFinite(targetScrollTop)) {
    pendingVirtualizerPositionCorrections.delete(node);
    return;
  }
  pendingVirtualizerPositionCorrections.set(
    node,
    Math.max(targetScrollTop, 0),
  );
}

export function clearMessageStackVirtualizerPositionCorrection(
  node: HTMLElement,
) {
  pendingVirtualizerPositionCorrections.delete(node);
}

export function consumeMessageStackVirtualizerPositionCorrection(
  node: HTMLElement,
) {
  const targetScrollTop = pendingVirtualizerPositionCorrections.get(node);
  if (targetScrollTop === undefined) {
    return false;
  }
  // A marker owns at most the first native frame after its write. If the
  // browser coalesced or clamped that write, a later reader scroll must not be
  // reclassified merely because it happens to land on the stale target.
  pendingVirtualizerPositionCorrections.delete(node);
  return Math.abs(node.scrollTop - targetScrollTop) < 1;
}

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
  ctrlKey: boolean;
  key: string;
  metaKey: boolean;
  shiftKey: boolean;
}) {
  // Only PLAIN shifted navigation belongs to browser selection extension.
  // Ctrl/Meta-modified shifted keys are pane boundary shortcuts
  // (resolvePaneScrollCommand maps Ctrl+[Shift+]Arrow/Home/End to a
  // boundary jump); claiming them here would hand e.g. Ctrl+Shift+ArrowDown
  // to the browser as a paragraph-selection gesture instead of jumping to
  // the transcript bottom.
  return (
    event.shiftKey &&
    !event.ctrlKey &&
    !event.metaKey &&
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

// Keyboard ownership can remain with a transcript even when the key event
// itself targets document.body. Publish that intent on the real scroll node so
// pane persistence and virtualized layout arbitration observe it before the
// pane's immediate Arrow/Page write or any remaining browser-owned Space motion.
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
