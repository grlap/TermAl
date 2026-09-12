// Owns one workspace row's overflow trigger, action menu, keyboard
// movement, and the React state/style that applies placement + maxHeight.
// Geometry helpers only return measurements. Does not own
// list/search/rename/delete operations or cross-row focus restoration
// after those operations. Split from WorkspacesPanel.tsx. The panel
// keeps the single menu-open identity and the trigger-ref map.

import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FocusEvent as ReactFocusEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";

import { getWorkspaceViewHref } from "../workspace-storage";
import { useCommittedRef } from "./use-committed-ref";
import {
  isMenuItemDisabled,
  measureWorkspaceOverflowMenuPlacement,
  resolveWorkspaceOverflowScroller,
  workspaceOverflowMenuNeedsReveal,
  workspaceOverflowScrollRoom,
} from "./workspace-overflow-menu-geometry";

export function workspaceOverflowTriggerLabel(displayName: string, isDeleting: boolean) {
  return isDeleting
    ? `Deleting. Actions for workspace ${displayName}`
    : `Actions for workspace ${displayName}`;
}

export function isIgnorableEscape(event: {
  key: string;
  defaultPrevented: boolean;
  nativeEvent?: { isComposing?: boolean; repeat?: boolean };
  isComposing?: boolean;
  repeat?: boolean;
}) {
  return event.key !== "Escape"
    || event.defaultPrevented
    || Boolean(event.isComposing ?? event.nativeEvent?.isComposing)
    || Boolean(event.repeat ?? event.nativeEvent?.repeat);
}

export function WorkspaceRowOverflowMenu({
  workspaceId,
  displayName,
  descriptionId,
  isCurrent,
  isPersisted,
  isDeleting,
  isSavingLabel,
  open,
  layoutSignature,
  onToggle,
  onClose,
  onRename,
  onRequestDelete,
  onTriggerRef,
  onEscapeToTrigger,
}: {
  workspaceId: string;
  displayName: string;
  descriptionId: string;
  isCurrent: boolean;
  isPersisted: boolean;
  isDeleting: boolean;
  isSavingLabel: boolean;
  open: boolean;
  layoutSignature: string;
  onToggle: () => void;
  onClose: () => void;
  onRename: () => void;
  onRequestDelete: () => void;
  onTriggerRef: (node: HTMLButtonElement | null) => void;
  onEscapeToTrigger: () => void;
}) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const [menuPlacement, setMenuPlacement] = useState<"up" | "down">("down");
  const [menuMaxHeight, setMenuMaxHeight] = useState<number | null>(null);
  const initialFocusDoneRef = useRef(false);
  const ignoringRevealScrollRef = useRef(false);
  const allowRevealOnApplyRef = useRef(true);
  const appliedSignatureRef = useRef<string | null>(null);
  const newTabDismissGenerationRef = useRef(0);
  const newTabDismissRafRef = useRef<number | null>(null);
  const revealIgnoreRafRef = useRef<number | null>(null);
  const focusOutRafRef = useRef<number | null>(null);
  const tabOutRafRef = useRef<number | null>(null);
  const onCloseRef = useCommittedRef(onClose);

  function assignTrigger(node: HTMLButtonElement | null) {
    triggerRef.current = node;
    onTriggerRef(node);
  }

  function menuFocusableItems() {
    return [
      ...(menuRef.current?.querySelectorAll<HTMLElement>("[role='menuitem']") ?? []),
    ].filter((item) => !isMenuItemDisabled(item));
  }

  function isInsideOverflowUi(node: EventTarget | null) {
    if (!(node instanceof Node)) {
      return false;
    }
    return Boolean(
      menuRef.current?.contains(node)
      || triggerRef.current?.contains(node),
    );
  }

  function cancelOwnedRaf(rafRef: { current: number | null }) {
    if (rafRef.current == null) {
      return;
    }
    cancelAnimationFrame(rafRef.current);
    rafRef.current = null;
  }

  function cancelScheduledNewTabDismiss() {
    cancelOwnedRaf(newTabDismissRafRef);
  }

  function cancelOwnedMenuRafs() {
    cancelOwnedRaf(revealIgnoreRafRef);
    cancelOwnedRaf(focusOutRafRef);
    cancelOwnedRaf(tabOutRafRef);
    ignoringRevealScrollRef.current = false;
  }

  function requestUnclampedMeasure(allowReveal: boolean) {
    allowRevealOnApplyRef.current = allowReveal;
    setMenuPlacement("down");
    setMenuMaxHeight(null);
  }

  useLayoutEffect(() => {
    if (!open) {
      setMenuPlacement("down");
      setMenuMaxHeight(null);
      initialFocusDoneRef.current = false;
      appliedSignatureRef.current = null;
      allowRevealOnApplyRef.current = true;
      return;
    }
    const menu = menuRef.current;
    const trigger = triggerRef.current;
    if (!menu || !trigger) {
      return;
    }
    if (menuMaxHeight != null && appliedSignatureRef.current !== layoutSignature) {
      requestUnclampedMeasure(true);
      return;
    }
    if (menuMaxHeight == null) {
      const measured = measureWorkspaceOverflowMenuPlacement(menu, trigger);
      appliedSignatureRef.current = layoutSignature;
      setMenuPlacement(measured.placement);
      setMenuMaxHeight(measured.maxHeight);
      return;
    }
    const scroller = resolveWorkspaceOverflowScroller(menu);
    if (
      allowRevealOnApplyRef.current
      && scroller
      && workspaceOverflowMenuNeedsReveal(
        menu.getBoundingClientRect(),
        scroller.getBoundingClientRect(),
        workspaceOverflowScrollRoom(scroller),
      )
    ) {
      ignoringRevealScrollRef.current = true;
      menu.scrollIntoView({ block: "nearest", inline: "nearest" });
      cancelOwnedRaf(revealIgnoreRafRef);
      revealIgnoreRafRef.current = requestAnimationFrame(() => {
        revealIgnoreRafRef.current = null;
        ignoringRevealScrollRef.current = false;
      });
    }
    if (!initialFocusDoneRef.current) {
      initialFocusDoneRef.current = true;
      menuFocusableItems()[0]?.focus({ preventScroll: true });
    }
  }, [open, menuMaxHeight, layoutSignature]);

  useLayoutEffect(() => {
    if (!open) {
      return;
    }
    const menu = menuRef.current;
    const scroller = resolveWorkspaceOverflowScroller(menu);
    const panel = menu?.closest(".workspaces-panel");

    function recomputeFromResize() {
      requestUnclampedMeasure(true);
    }

    function recomputeFromScroll() {
      if (ignoringRevealScrollRef.current) {
        return;
      }
      requestUnclampedMeasure(false);
    }

    const observers: ResizeObserver[] = [];
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(recomputeFromResize);
      if (scroller) {
        observer.observe(scroller);
      }
      if (panel instanceof Element) {
        observer.observe(panel);
      }
      observers.push(observer);
    }
    scroller?.addEventListener("scroll", recomputeFromScroll, { passive: true });
    window.addEventListener("resize", recomputeFromResize);
    return () => {
      for (const observer of observers) {
        observer.disconnect();
      }
      scroller?.removeEventListener("scroll", recomputeFromScroll);
      window.removeEventListener("resize", recomputeFromResize);
    };
  }, [open, layoutSignature]);

  useEffect(() => {
    newTabDismissGenerationRef.current += 1;
    cancelScheduledNewTabDismiss();
    cancelOwnedMenuRafs();
    return () => {
      cancelScheduledNewTabDismiss();
      cancelOwnedMenuRafs();
    };
  }, [open]);

  useEffect(() => {
    if (!open) {
      return;
    }

    function isOutsideOverflow(node: EventTarget | null) {
      if (!(node instanceof Node)) {
        return true;
      }
      if (menuRef.current?.contains(node)) {
        return false;
      }
      return !(node instanceof HTMLElement && node.closest("[data-workspace-overflow]"));
    }

    function handlePointerDown(event: PointerEvent) {
      if (!isOutsideOverflow(event.target)) {
        return;
      }
      onCloseRef.current();
    }

    function handleFocusIn(event: FocusEvent) {
      if (!isOutsideOverflow(event.target)) {
        return;
      }
      onCloseRef.current();
    }

    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("focusin", handleFocusIn);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("focusin", handleFocusIn);
    };
  }, [open, onCloseRef]);

  function handleOverflowFocusOut(event: ReactFocusEvent<HTMLDivElement>) {
    if (!open) {
      return;
    }
    const next = event.relatedTarget;
    if (isInsideOverflowUi(next)) {
      return;
    }
    if (next instanceof Node) {
      onCloseRef.current();
      return;
    }
    // Null relatedTarget is not proof that focus left. Wait one frame to
    // see whether focus actually landed on an outside control (Tab-out in
    // some engines). Transient body focus, as in the unverified macOS
    // click hypothesis, does not close. Outside pointer is owned by the
    // document listener.
    cancelOwnedRaf(focusOutRafRef);
    focusOutRafRef.current = requestAnimationFrame(() => {
      focusOutRafRef.current = null;
      const active = document.activeElement;
      if (!active || active === document.body || !document.contains(active)) {
        return;
      }
      if (isInsideOverflowUi(active)) {
        return;
      }
      onCloseRef.current();
    });
  }

  function handleMenuKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    const items = menuFocusableItems();
    if (!items.length) {
      return;
    }
    const currentIndex = items.indexOf(event.target as HTMLElement);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      const nextIndex = currentIndex < 0
        ? 0
        : (currentIndex + delta + items.length) % items.length;
      items[nextIndex]?.focus();
      return;
    }
    if (event.key === "Home") {
      event.preventDefault();
      items[0]?.focus();
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      items[items.length - 1]?.focus();
      return;
    }
    if (event.key === "Tab") {
      const atStart = currentIndex <= 0;
      const atEnd = currentIndex === items.length - 1;
      if ((event.shiftKey && atStart) || (!event.shiftKey && atEnd)) {
        cancelOwnedRaf(tabOutRafRef);
        tabOutRafRef.current = requestAnimationFrame(() => {
          tabOutRafRef.current = null;
          onCloseRef.current();
        });
      }
      return;
    }
    if (event.key === " " || event.key === "Spacebar") {
      if (
        event.defaultPrevented
        || event.nativeEvent.isComposing
        || event.nativeEvent.repeat
      ) {
        return;
      }
      const target = event.target;
      if (!(target instanceof HTMLAnchorElement) || target.getAttribute("role") !== "menuitem") {
        return;
      }
      if (isMenuItemDisabled(target)) {
        return;
      }
      event.preventDefault();
      target.click();
      return;
    }
    if (isIgnorableEscape(event)) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    onEscapeToTrigger();
  }

  function isAcceptedNewTabActivation(event: { type: string; button: number; defaultPrevented: boolean }) {
    if (event.defaultPrevented) {
      return false;
    }
    if (event.type === "click") {
      return event.button === 0;
    }
    if (event.type === "auxclick") {
      return event.button === 1;
    }
    return false;
  }

  function scheduleNativeNewTabDismiss() {
    const generation = newTabDismissGenerationRef.current;
    cancelScheduledNewTabDismiss();
    newTabDismissRafRef.current = requestAnimationFrame(() => {
      newTabDismissRafRef.current = null;
      if (generation !== newTabDismissGenerationRef.current) {
        return;
      }
      const active = document.activeElement;
      const restoreTrigger = !active
        || active === document.body
        || !document.contains(active)
        || isInsideOverflowUi(active);
      onCloseRef.current();
      if (restoreTrigger && generation === newTabDismissGenerationRef.current) {
        triggerRef.current?.focus();
      }
    });
  }

  function handleNativeNewTabActivate(event: { type: string; button: number; defaultPrevented: boolean }) {
    if (!isAcceptedNewTabActivation(event)) {
      return;
    }
    scheduleNativeNewTabDismiss();
  }

  return (
    <div
      className="workspaces-panel-item-overflow"
      onBlur={handleOverflowFocusOut}
    >
      <button
        ref={assignTrigger}
        className="ghost-button workspaces-panel-overflow-trigger"
        type="button"
        data-workspace-overflow={workspaceId}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={workspaceOverflowTriggerLabel(displayName, isDeleting)}
        aria-describedby={descriptionId}
        disabled={isDeleting}
        onClick={onToggle}
      >
        {isDeleting ? "Deleting" : "⋯"}
      </button>
      {open ? (
        <div
          ref={menuRef}
          className={`workspaces-panel-menu panel${menuPlacement === "up" ? " workspaces-panel-menu-up" : ""}`}
          style={menuMaxHeight == null ? undefined : { maxHeight: `${menuMaxHeight}px` }}
          role="menu"
          aria-label={`Workspace actions ${displayName}`}
          aria-describedby={descriptionId}
          onKeyDown={handleMenuKeyDown}
        >
          {isPersisted ? (
            <button
              className="workspaces-panel-menu-item"
              type="button"
              role="menuitem"
              disabled={isDeleting || isSavingLabel}
              aria-describedby={descriptionId}
              onClick={onRename}
            >
              Rename
            </button>
          ) : null}
          {isCurrent ? null : (
            <>
              <a
                className="workspaces-panel-menu-item"
                role="menuitem"
                href={getWorkspaceViewHref(workspaceId)}
                target="_blank"
                rel="noopener"
                aria-label={`Open in new tab: workspace ${displayName}`}
                aria-describedby={descriptionId}
                onClick={handleNativeNewTabActivate}
                onAuxClick={handleNativeNewTabActivate}
              >
                {/* Native navigation: do not preventDefault or replace this with a button. */}
                Open in new tab
              </a>
              <button
                className="workspaces-panel-menu-item workspaces-panel-menu-item-danger"
                type="button"
                role="menuitem"
                disabled={isDeleting}
                aria-label={`Delete workspace ${displayName}`}
                aria-describedby={descriptionId}
                onClick={onRequestDelete}
              >
                Delete
              </button>
            </>
          )}
        </div>
      ) : null}
    </div>
  );
}
