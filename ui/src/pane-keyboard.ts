const TEXT_ENTRY_INPUT_TYPES = new Set([
  "",
  "email",
  "password",
  "search",
  "tel",
  "text",
  "url",
]);

export type PaneScrollCommand =
  | { kind: "page"; direction: "up" | "down" }
  | { kind: "boundary"; direction: "up" | "down" };

export function shouldHandlePanePageKey(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return true;
  }

  if (target instanceof HTMLTextAreaElement) {
    return hasCaretAtStart(target);
  }

  if (target instanceof HTMLInputElement) {
    return TEXT_ENTRY_INPUT_TYPES.has(target.type) && hasCaretAtStart(target);
  }

  if (target instanceof HTMLSelectElement || isEditableContainer(target)) {
    return false;
  }

  return true;
}

export function resolvePaneScrollCommand(
  event: {
    altKey: boolean;
    ctrlKey: boolean;
    key: string;
    metaKey: boolean;
    shiftKey: boolean;
  },
  target: EventTarget | null,
  platform = detectPlatform(),
): PaneScrollCommand | null {
  if (event.key === "Home" || event.key === "End") {
    if (!event.ctrlKey && !event.metaKey && shouldKeepPlainHomeEndInTarget(target)) {
      return null;
    }

    if (event.ctrlKey || event.metaKey) {
      return { kind: "boundary", direction: event.key === "Home" ? "up" : "down" };
    }

    // Plain Home/End when not in text entry still jumps to the pane boundary.
    if (!event.altKey && !event.shiftKey) {
      return { kind: "boundary", direction: event.key === "Home" ? "up" : "down" };
    }

    return null;
  }

  if (event.key === "PageUp" || event.key === "PageDown") {
    if (event.altKey || event.metaKey) {
      return null;
    }

    if (event.ctrlKey) {
      // Ctrl+PageUp/PageDown is a Windows/Linux boundary-jump
      // shortcut. On macOS the convention is Cmd-arrow (already
      // rejected above by the `metaKey` gate) and the pane has
      // no reason to intercept `Ctrl+PageUp/Down` there — falling
      // through to `makePaneScrollCommand` would turn it into a
      // one-page scroll, a platform-specific capture the user
      // didn't ask for. Mirrors the `Ctrl+ArrowUp/Down` branch
      // below which also bails on Apple.
      if (isApplePlatform(platform)) {
        return null;
      }
      return { kind: "boundary", direction: event.key === "PageUp" ? "up" : "down" };
    }

    if (!shouldHandlePanePageKey(target)) {
      return null;
    }

    return makePaneScrollCommand(event.key === "PageUp" ? "up" : "down", event.shiftKey);
  }

  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") {
    return null;
  }

  if (event.altKey || event.metaKey || !event.ctrlKey || isApplePlatform(platform)) {
    return null;
  }

  return { kind: "boundary", direction: event.key === "ArrowUp" ? "up" : "down" };
}

function hasCaretAtStart(element: HTMLInputElement | HTMLTextAreaElement): boolean {
  return element.selectionStart === 0 && element.selectionEnd === 0;
}

function shouldKeepPlainHomeEndInTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  if (target instanceof HTMLTextAreaElement) {
    return true;
  }

  if (target instanceof HTMLInputElement) {
    return TEXT_ENTRY_INPUT_TYPES.has(target.type);
  }

  return target instanceof HTMLSelectElement || isEditableContainer(target);
}

function isEditableContainer(target: HTMLElement): boolean {
  return (
    target.isContentEditable ||
    target.contentEditable === "true" ||
    target.getAttribute("contenteditable") === "" ||
    target.getAttribute("contenteditable") === "true"
  );
}

function makePaneScrollCommand(
  direction: "up" | "down",
  shiftKey: boolean,
): PaneScrollCommand {
  return shiftKey ? { kind: "boundary", direction } : { kind: "page", direction };
}

function detectPlatform(): string {
  if (typeof navigator === "undefined") {
    return "";
  }

  const navigatorWithUserAgentData = navigator as Navigator & {
    userAgentData?: { platform?: string };
  };

  return navigatorWithUserAgentData.userAgentData?.platform ?? navigator.platform ?? "";
}

function isApplePlatform(platform: string): boolean {
  return /mac|iphone|ipad|ipod/i.test(platform);
}
