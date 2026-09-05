// Owns: browser body-keyboard scroll ownership candidate for one session
// transcript. The consumer must still gate publication on active-pane state;
// inactive panes keep tracking pointer input so the activating mousedown is not
// lost before React commits the new active pane.
// Does not own: tail-follow detachment, normalized scroll-intent publication,
// history navigation, or pane scroll persistence.
// Split from: ui/src/SessionPaneView.scroll.ts.

import { useCallback, useEffect, useRef, type RefObject } from "react";

import { canNestedScrollableConsumeWheel } from "./app-utils";
import { messageStackOwnsBodyKeyboardScroll } from "./message-stack-scroll-sync";
import type { PaneViewMode } from "./workspace-types";

export function useSessionPaneBodyKeyboardOwnership({
  messageStackRef,
  paneViewMode,
  scrollStateKey,
}: {
  messageStackRef: RefObject<HTMLElement | null>;
  paneViewMode: PaneViewMode;
  scrollStateKey: string;
}) {
  const ownsBodyKeyboardScrollRef = useRef(false);
  const lastPointerWasInsideRef = useRef(false);
  const lastPointerTargetRef = useRef<EventTarget | null>(null);

  useEffect(() => {
    if (paneViewMode !== "session") {
      ownsBodyKeyboardScrollRef.current = false;
      lastPointerWasInsideRef.current = false;
      lastPointerTargetRef.current = null;
      return;
    }

    const updateFromPointer = (event: MouseEvent) => {
      const node = messageStackRef.current;
      // Pointer ownership follows containment, not whether the descendant is
      // focusable. Safari and Firefox on macOS can leave focus on body after a
      // button/link click while still routing scroll keys to this transcript.
      // A subsequent focusin refines ownership using control semantics below.
      ownsBodyKeyboardScrollRef.current = Boolean(
        node && event.target instanceof Node && node.contains(event.target),
      );
      lastPointerWasInsideRef.current = ownsBodyKeyboardScrollRef.current;
      lastPointerTargetRef.current = ownsBodyKeyboardScrollRef.current
        ? event.target
        : null;
    };
    let disposed = false;
    const updateFromFocus = (event: FocusEvent) => {
      const node = messageStackRef.current;
      if (messageStackOwnsBodyKeyboardScroll(event.target, node)) {
        ownsBodyKeyboardScrollRef.current = true;
        // Focus is now the browser's authoritative keyboard target. Do not
        // keep routing decisions through an older pointer target that may
        // belong to a nested scroller elsewhere in the transcript.
        lastPointerTargetRef.current = null;
        return;
      }
      // Programmatic helpers can focus a temporary off-screen control, remove
      // it, and leave focus on body while Chromium still routes scroll keys to
      // the previously clicked transcript. Defer outside-focus revocation so
      // that transient churn does not discard the browser's real scroll owner.
      queueMicrotask(() => {
        if (disposed) {
          return;
        }
        const activeElement = document.activeElement;
        if (
          activeElement === null ||
          activeElement === document.body ||
          activeElement === document.documentElement
        ) {
          return;
        }
        const ownsBodyKeyboardScroll = messageStackOwnsBodyKeyboardScroll(
          activeElement,
          node,
        );
        ownsBodyKeyboardScrollRef.current = ownsBodyKeyboardScroll;
        if (
          !ownsBodyKeyboardScroll &&
          !(node && activeElement instanceof Node && node.contains(activeElement))
        ) {
          // A persistent focus transfer outside the transcript is deliberate.
          // Clear the pointer fallback too, so a later blur to body cannot
          // silently redirect Home/PageUp/ArrowUp back into the transcript.
          lastPointerWasInsideRef.current = false;
          lastPointerTargetRef.current = null;
        }
      });
    };

    // Every mounted session pane tracks pointer/focus ownership, even before
    // it becomes the active workspace pane. The mousedown that activates a
    // background pane is already in flight before React can rerun an effect;
    // waiting for activation would lose that first ownership transfer.
    document.addEventListener("mousedown", updateFromPointer, true);
    document.addEventListener("focusin", updateFromFocus, true);
    return () => {
      disposed = true;
      document.removeEventListener("mousedown", updateFromPointer, true);
      document.removeEventListener("focusin", updateFromFocus, true);
      ownsBodyKeyboardScrollRef.current = false;
      lastPointerWasInsideRef.current = false;
      lastPointerTargetRef.current = null;
    };
  }, [messageStackRef, paneViewMode]);

  useEffect(() => {
    // Keyboard tab switches have no pointer event to transfer ownership. A
    // newly selected session must not inherit another tab's body-key scroller.
    ownsBodyKeyboardScrollRef.current = false;
    lastPointerWasInsideRef.current = false;
    lastPointerTargetRef.current = null;
  }, [scrollStateKey]);

  return useCallback(
    (direction: "up" | "down") => {
      if (hasOpenKeyboardBlockingDialog()) {
        return false;
      }

      const ownsBodyKeyboardScroll =
        ownsBodyKeyboardScrollRef.current ||
        Boolean(
          lastPointerWasInsideRef.current &&
          (document.activeElement === null ||
            document.activeElement === document.body ||
            document.activeElement === document.documentElement),
        );
      if (!ownsBodyKeyboardScroll) {
        return false;
      }

      const node = messageStackRef.current;
      if (!node) {
        return false;
      }
      return !canNestedScrollableConsumeWheel(
        lastPointerTargetRef.current,
        node,
        direction === "up" ? -1 : 1,
      );
    },
    [messageStackRef],
  );
}

function hasOpenKeyboardBlockingDialog() {
  const dialogs = document.querySelectorAll(
    '[aria-modal="true"], dialog[open]',
  );
  return Array.from(dialogs).some(isKeyboardBlockingDialogVisible);
}

function isKeyboardBlockingDialogVisible(dialog: Element) {
  if (dialog.closest('[aria-hidden="true"], [hidden], [inert]')) {
    return false;
  }

  for (let current: Element | null = dialog; current; current = current.parentElement) {
    const style = window.getComputedStyle(current);
    if (
      style.display === "none" ||
      style.visibility === "hidden" ||
      style.visibility === "collapse"
    ) {
      return false;
    }
  }
  return true;
}
