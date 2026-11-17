// Owns the workspace confirmation's portal and viewport placement.
// The panel owns deletion, pending guards and focus restoration.
// Uses the session-delete popup chrome so confirmation never moves the list.
import { useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";

import { clamp } from "../app-utils";
import { isIgnorableEscape } from "./WorkspaceRowOverflowMenu";

export function WorkspaceDeletePopover({
  getAnchor,
  dialogRef,
  initialFocusRef,
  descriptionId,
  workspaceId,
  onCancel,
  children,
}: {
  getAnchor: () => HTMLElement | null;
  dialogRef: RefObject<HTMLDivElement>;
  initialFocusRef: RefObject<HTMLButtonElement>;
  descriptionId: string;
  workspaceId: string;
  onCancel: () => boolean;
  children: ReactNode;
}) {
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const lastAnchorRect = useRef<DOMRect | null>(null);
  const initialFocusDone = useRef(false);

  useLayoutEffect(() => {
    function updatePosition() {
      const popup = dialogRef.current;
      if (!popup) return;
      const anchor = getAnchor();
      if (anchor?.isConnected) {
        lastAnchorRect.current = anchor.getBoundingClientRect();
      }
      // A pending deletion can remove the row before its request settles.
      // Keep that confirmation at its last anchor until the panel closes it.
      const rect = lastAnchorRect.current;
      if (!rect) return;
      const popupRect = popup.getBoundingClientRect();
      const padding = 12;
      const gap = 8;
      const maxLeft = Math.max(padding, window.innerWidth - popupRect.width - padding);
      const maxTop = Math.max(padding, window.innerHeight - popupRect.height - padding);
      const preferredTop = rect.bottom + gap + popupRect.height <= window.innerHeight - padding
        ? rect.bottom + gap
        : rect.top - popupRect.height - gap;
      const next = {
        left: clamp(rect.right - popupRect.width, padding, maxLeft),
        top: clamp(preferredTop, padding, maxTop),
      };
      if (position?.left === next.left && position.top === next.top) return;
      setPosition((current) => current?.left === next.left && current.top === next.top ? current : next);
    }

    updatePosition();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(updatePosition);
    if (dialogRef.current) observer?.observe(dialogRef.current);
    const anchor = getAnchor();
    if (anchor) observer?.observe(anchor);
    function handleOutsidePointer(event: PointerEvent) {
      if (event.target instanceof Node && !dialogRef.current?.contains(event.target)) {
        // Dismiss an idle confirmation without swallowing the outside action.
        // Pending guards may refuse dismissal, but never lock the rest of the app.
        onCancel();
      }
    }
    function handleEscape(event: KeyboardEvent) {
      if (!isIgnorableEscape(event) && onCancel()) event.preventDefault();
    }
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    document.addEventListener("pointerdown", handleOutsidePointer);
    window.addEventListener("keydown", handleEscape);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
      document.removeEventListener("pointerdown", handleOutsidePointer);
      window.removeEventListener("keydown", handleEscape);
    };
    // Anchor refs attach during commit. Remeasure after every render because
    // rename/list changes can move or remount it without any size notification.
    // The coordinate equality check keeps positioning renders bounded.
  });

  useLayoutEffect(() => {
    if (!position || initialFocusDone.current) return;
    initialFocusDone.current = true;
    // The position render has removed visibility:hidden before this layout
    // effect. A parent passive effect can run before that render and lose focus.
    initialFocusRef.current?.focus({ preventScroll: true });
  }, [initialFocusRef, position]);

  return createPortal(
    <div
      ref={dialogRef}
      className="workspaces-panel-confirm session-kill-popover panel"
      style={position ?? { left: 0, top: 0, visibility: "hidden" }}
      role="dialog"
      tabIndex={-1}
      aria-label={`Delete workspace ${workspaceId}`}
      aria-describedby={descriptionId}
    >
      {children}
    </div>,
    document.body,
  );
}
