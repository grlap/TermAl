// Owns overflow menu measurement: nearest overflow ancestor,
// box-model min-height, and up/down + maxHeight numbers.
// Does not mutate menu class or style, reveal, or own trigger/menu
// DOM, keyboard, or list/search/rename/delete. WorkspaceRowOverflowMenu
// owns applying those numbers through React state. Split from
// WorkspacesPanel.tsx. styles.css owns the overflow:auto rule on
// `.control-panel-body`. ControlPanelSurface owns that overflow
// ancestor DOM.

export type WorkspaceOverflowScrollRoom = {
  canScrollUp: boolean;
  canScrollDown: boolean;
};

export function isMenuItemDisabled(item: HTMLElement) {
  return item.getAttribute("aria-disabled") === "true"
    || (item instanceof HTMLButtonElement && item.disabled)
    || item.hasAttribute("disabled");
}

function elementOverflowY(node: HTMLElement): string {
  const computed = getComputedStyle(node);
  const value = computed.overflowY || computed.overflow;
  if (value && value !== "visible") {
    return value;
  }
  return node.style.overflowY || node.style.overflow;
}

export function resolveWorkspaceOverflowScroller(start: Element | null): HTMLElement | null {
  let node = start?.parentElement ?? null;
  while (node instanceof HTMLElement) {
    const overflowY = elementOverflowY(node);
    if (overflowY === "auto" || overflowY === "scroll") {
      return node;
    }
    node = node.parentElement;
  }
  return null;
}

export function workspaceOverflowScrollRoom(scroller: HTMLElement): WorkspaceOverflowScrollRoom {
  const maxScrollTop = Math.max(scroller.scrollHeight - scroller.clientHeight, 0);
  return {
    canScrollUp: scroller.scrollTop > 0,
    canScrollDown: scroller.scrollTop < maxScrollTop - 0.5,
  };
}

function workspaceOverflowMenuFallbackItemHeight(style: CSSStyleDeclaration) {
  const fontSize = Number.parseFloat(style.fontSize);
  return (Number.isFinite(fontSize) && fontSize > 0 ? fontSize : 16) * 2.25;
}

export function workspaceOverflowMenuMinHeight(menu: HTMLElement): number {
  const style = getComputedStyle(menu);
  const items = [...menu.querySelectorAll<HTMLElement>("[role='menuitem']")];
  const item = items.find((entry) => !isMenuItemDisabled(entry)) ?? items[0] ?? null;
  const itemHeight = item?.getBoundingClientRect().height ?? 0;
  const leadingHeight = item && items[0]
    ? Math.max(0, item.getBoundingClientRect().top - items[0].getBoundingClientRect().top)
    : 0;
  const itemRegion = (itemHeight > 0 ? itemHeight : workspaceOverflowMenuFallbackItemHeight(style))
    + leadingHeight;
  const paddingTop = Number.parseFloat(style.paddingTop) || 0;
  const paddingBottom = Number.parseFloat(style.paddingBottom) || 0;
  const borderTop = Number.parseFloat(style.borderTopWidth) || 0;
  const borderBottom = Number.parseFloat(style.borderBottomWidth) || 0;
  const chrome = paddingTop + paddingBottom + borderTop + borderBottom;
  const raw = style.boxSizing === "border-box" ? itemRegion + chrome : itemRegion;
  return Math.ceil(raw);
}

export function resolveWorkspaceOverflowMenuPlacement(
  triggerRect: Pick<DOMRect, "top" | "bottom">,
  scrollerRect: Pick<DOMRect, "top" | "bottom">,
  menuHeight: number,
  gap = 8,
  scrollRoom?: WorkspaceOverflowScrollRoom,
): "up" | "down" {
  const spaceBelow = scrollerRect.bottom - triggerRect.bottom - gap;
  const spaceAbove = triggerRect.top - scrollerRect.top - gap;
  if (spaceBelow >= menuHeight) {
    return "down";
  }
  if (spaceAbove >= menuHeight) {
    return "up";
  }
  const preferUp = spaceAbove > spaceBelow;
  if (preferUp && scrollRoom && !scrollRoom.canScrollUp && spaceBelow > 0) {
    return "down";
  }
  if (!preferUp && scrollRoom && !scrollRoom.canScrollDown && spaceAbove > 0) {
    return "up";
  }
  return preferUp ? "up" : "down";
}

export function workspaceOverflowMenuNeedsReveal(
  menuRect: Pick<DOMRect, "top" | "bottom">,
  scrollerRect: Pick<DOMRect, "top" | "bottom">,
  scrollRoom?: WorkspaceOverflowScrollRoom,
): boolean {
  const clippedTop = menuRect.top < scrollerRect.top;
  const clippedBottom = menuRect.bottom > scrollerRect.bottom;
  if (!clippedTop && !clippedBottom) {
    return false;
  }
  if (!scrollRoom) {
    return true;
  }
  return (clippedTop && scrollRoom.canScrollUp) || (clippedBottom && scrollRoom.canScrollDown);
}

export function availableWorkspaceOverflowMenuHeight(
  triggerRect: Pick<DOMRect, "top" | "bottom">,
  scrollerRect: Pick<DOMRect, "top" | "bottom">,
  placement: "up" | "down",
  gap = 8,
  minHeight = 0,
): number {
  const raw = placement === "up"
    ? triggerRect.top - scrollerRect.top - gap
    : scrollerRect.bottom - triggerRect.bottom - gap;
  return Math.max(Math.floor(raw), Math.max(0, Math.ceil(minHeight)));
}

export type WorkspaceOverflowMenuMeasurement = {
  placement: "up" | "down";
  maxHeight: number;
};

export function measureWorkspaceOverflowMenuPlacement(
  menu: HTMLElement,
  trigger: HTMLElement,
): WorkspaceOverflowMenuMeasurement {
  const scroller = resolveWorkspaceOverflowScroller(menu);
  if (!scroller) {
    const minHeight = workspaceOverflowMenuMinHeight(menu);
    const naturalHeight = menu.getBoundingClientRect().height;
    return {
      placement: "down",
      maxHeight: Math.max(Math.ceil(minHeight), Math.ceil(naturalHeight), 1),
    };
  }
  const scrollerRect = scroller.getBoundingClientRect();
  const triggerRect = trigger.getBoundingClientRect();
  const scrollRoom = workspaceOverflowScrollRoom(scroller);
  const minHeight = workspaceOverflowMenuMinHeight(menu);
  const placement = resolveWorkspaceOverflowMenuPlacement(
    triggerRect,
    scrollerRect,
    menu.getBoundingClientRect().height,
    8,
    scrollRoom,
  );
  return {
    placement,
    maxHeight: availableWorkspaceOverflowMenuHeight(
      triggerRect,
      scrollerRect,
      placement,
      8,
      minHeight,
    ),
  };
}
