// Shared Escape-key dismissal for modal dialog shells.
//
// This module owns the topmost-dialog ordering and defers dismissal until the
// current key event has reached inner surfaces. A combobox or menu can consume
// Escape with preventDefault(), closing only itself; the next Escape reaches
// the dialog. Callers still own open state, busy guards, focus restoration,
// and all other close-time cleanup.

import { useEffect, useRef } from "react";

type EscapeDismissEntry = {
  canDismissRef: { current: boolean };
  onDismissRef: { current: () => void };
  pending: boolean;
};

const escapeDismissStack: EscapeDismissEntry[] = [];
let isListeningForEscape = false;

function topEscapeDismissEntry(): EscapeDismissEntry | undefined {
  return escapeDismissStack[escapeDismissStack.length - 1];
}

function handleEscapeKeyDown(event: KeyboardEvent): void {
  if (
    event.key !== "Escape" ||
    event.isComposing ||
    event.defaultPrevented ||
    event.repeat
  ) {
    return;
  }

  const entry = topEscapeDismissEntry();
  if (!entry || entry.pending) {
    return;
  }

  entry.pending = true;
  queueMicrotask(() => {
    entry.pending = false;

    if (
      event.defaultPrevented ||
      event.isComposing ||
      topEscapeDismissEntry() !== entry ||
      !entry.canDismissRef.current
    ) {
      return;
    }

    event.preventDefault();
    entry.onDismissRef.current();
  });
}

function startListeningForEscape(): void {
  if (isListeningForEscape || typeof window === "undefined") {
    return;
  }

  window.addEventListener("keydown", handleEscapeKeyDown);
  isListeningForEscape = true;
}

function stopListeningForEscapeIfIdle(): void {
  if (
    !isListeningForEscape ||
    escapeDismissStack.length > 0 ||
    typeof window === "undefined"
  ) {
    return;
  }

  window.removeEventListener("keydown", handleEscapeKeyDown);
  isListeningForEscape = false;
}

export function useDialogEscapeDismiss({
  isOpen,
  canDismiss = true,
  onDismiss,
}: {
  isOpen: boolean;
  canDismiss?: boolean;
  onDismiss: () => void;
}): void {
  const canDismissRef = useRef(canDismiss);
  const onDismissRef = useRef(onDismiss);
  canDismissRef.current = canDismiss;
  onDismissRef.current = onDismiss;

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    const entry: EscapeDismissEntry = {
      canDismissRef,
      onDismissRef,
      pending: false,
    };
    escapeDismissStack.push(entry);
    startListeningForEscape();

    return () => {
      const entryIndex = escapeDismissStack.lastIndexOf(entry);
      if (entryIndex >= 0) {
        escapeDismissStack.splice(entryIndex, 1);
      }
      stopListeningForEscapeIfIdle();
    };
  }, [isOpen]);
}
