// Owns composer textarea height measurement, rAF resize scheduling, and transition restoration.
// Does not own draft state, slash-palette behavior, send/delegate actions, or session selection.
// Split from AgentSessionPanel.tsx to keep SessionComposer focused on composer orchestration.
import { useEffect, useRef } from "react";

export function useComposerAutoResize(activeSessionId: string | null) {
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const composerResizeAnimationFrameRef = useRef<number | null>(null);
  const composerTransitionRestoreRef = useRef<{
    frameId: number;
    previousInlineTransition: string;
  } | null>(null);
  const composerResizeNeedsMetricRefreshRef = useRef(false);
  const composerResizeShouldAnimateHeightRef = useRef(true);
  const composerLastMeasuredDraftLengthRef = useRef(0);
  const composerLastAppliedHeightRef = useRef<number | null>(null);
  const composerLastAppliedOverflowYRef = useRef<"auto" | "hidden" | null>(null);
  const composerSizingStateRef = useRef<{
    borderHeight: number;
    minHeight: number;
    panelElement: HTMLElement | null;
    panelSlotElement: HTMLElement | null;
  } | null>(null);

  function resetAndCancelScheduledComposerResize() {
    composerResizeNeedsMetricRefreshRef.current = false;
    composerResizeShouldAnimateHeightRef.current = true;
    if (composerResizeAnimationFrameRef.current == null) {
      return;
    }

    window.cancelAnimationFrame(composerResizeAnimationFrameRef.current);
    composerResizeAnimationFrameRef.current = null;
  }

  function resetComposerSizingState() {
    composerSizingStateRef.current = null;
    composerResizeNeedsMetricRefreshRef.current = false;
    composerResizeShouldAnimateHeightRef.current = true;
    composerLastMeasuredDraftLengthRef.current = 0;
    composerLastAppliedHeightRef.current = null;
    composerLastAppliedOverflowYRef.current = null;
  }

  function restoreComposerInputTransition(
    textarea: HTMLTextAreaElement,
    previousInlineTransition: string,
  ) {
    if (previousInlineTransition) {
      textarea.style.transition = previousInlineTransition;
    } else {
      textarea.style.removeProperty("transition");
    }
  }

  function cancelScheduledComposerTransitionRestore() {
    const pendingRestore = composerTransitionRestoreRef.current;
    if (!pendingRestore) {
      return null;
    }

    window.cancelAnimationFrame(pendingRestore.frameId);
    composerTransitionRestoreRef.current = null;
    return pendingRestore.previousInlineTransition;
  }

  function cancelAndRestoreScheduledComposerTransition() {
    // A shrink can suppress height transitions for one frame. If another path
    // cancels that restore before the frame fires, only restore when the current
    // textarea is still in that temporary "none" state; otherwise a newer resize
    // has already established its own transition.
    const previousInlineTransition = cancelScheduledComposerTransitionRestore();
    const textarea = composerInputRef.current;
    if (
      previousInlineTransition !== null &&
      textarea &&
      textarea.style.transition === "none"
    ) {
      restoreComposerInputTransition(textarea, previousInlineTransition);
    }
  }

  function scheduleComposerTransitionRestore(
    textarea: HTMLTextAreaElement,
    previousInlineTransition: string,
  ) {
    cancelScheduledComposerTransitionRestore();
    const frameId = window.requestAnimationFrame(() => {
      const pendingRestore = composerTransitionRestoreRef.current;
      // A later resize may cancel or replace this restore before the rAF fires.
      // The frame id pins this callback to the restore that scheduled it.
      if (!pendingRestore || pendingRestore.frameId !== frameId) {
        return;
      }

      composerTransitionRestoreRef.current = null;
      restoreComposerInputTransition(
        textarea,
        pendingRestore.previousInlineTransition,
      );
    });
    composerTransitionRestoreRef.current = {
      frameId,
      previousInlineTransition,
    };
  }

  function getComposerSizingState(
    textarea: HTMLTextAreaElement,
    forceRefreshMetrics = false,
  ) {
    if (!composerSizingStateRef.current || forceRefreshMetrics) {
      const computedStyle = window.getComputedStyle(textarea);
      const panelElement = textarea.closest(".workspace-pane");
      const resolvedPanelElement =
        panelElement instanceof HTMLElement ? panelElement : null;
      const panelSlotElement =
        resolvedPanelElement?.parentElement instanceof HTMLElement
          ? resolvedPanelElement.parentElement
          : null;

      composerSizingStateRef.current = {
        minHeight: parseFloat(computedStyle.minHeight) || 0,
        borderHeight:
          (parseFloat(computedStyle.borderTopWidth) || 0) +
          (parseFloat(computedStyle.borderBottomWidth) || 0),
        panelElement: resolvedPanelElement,
        panelSlotElement,
      };
    }

    return composerSizingStateRef.current;
  }

  function resizeComposerInput(forceRefreshMetrics = false, animateHeight = true) {
    const textarea = composerInputRef.current;
    if (!textarea) {
      return;
    }

    const pendingPreviousInlineTransition =
      cancelScheduledComposerTransitionRestore();
    const sizingState = getComposerSizingState(textarea, forceRefreshMetrics);
    const availablePanelHeight =
      sizingState.panelSlotElement?.clientHeight ??
      (sizingState.panelElement instanceof HTMLElement
        ? sizingState.panelElement.clientHeight
        : 0);
    const maxHeight = Math.max(
      sizingState.minHeight,
      availablePanelHeight > 0 ? availablePanelHeight * 0.4 : Number.POSITIVE_INFINITY,
    );
    const currentDraftLength = textarea.value.length;
    const shouldAllowShrink =
      forceRefreshMetrics ||
      currentDraftLength < composerLastMeasuredDraftLengthRef.current;
    const previousInlineTransition =
      pendingPreviousInlineTransition !== null &&
      textarea.style.transition === "none"
        ? pendingPreviousInlineTransition
        : textarea.style.transition;
    if (
      !shouldAllowShrink &&
      pendingPreviousInlineTransition !== null &&
      textarea.style.transition === "none"
    ) {
      restoreComposerInputTransition(textarea, pendingPreviousInlineTransition);
    }
    const previousMeasuredHeight =
      composerLastAppliedHeightRef.current ??
      (parseFloat(textarea.style.height) ||
        textarea.getBoundingClientRect().height ||
        null);
    const shrinkProbeHeight = Math.max(sizingState.minHeight, 1);
    if (shouldAllowShrink) {
      textarea.style.transition = "none";
      textarea.style.height = `${shrinkProbeHeight}px`;
      composerLastAppliedHeightRef.current = null;
    }

    const contentHeight = textarea.scrollHeight + sizingState.borderHeight;
    const nextHeight = Math.min(Math.max(contentHeight, sizingState.minHeight), maxHeight);
    const nextOverflowY: "auto" | "hidden" =
      contentHeight > maxHeight + 1 ? "auto" : "hidden";

    if (shouldAllowShrink) {
      const hasPreviousMeasuredHeight = previousMeasuredHeight != null;
      const heightChanged =
        !hasPreviousMeasuredHeight ||
        Math.abs(previousMeasuredHeight - nextHeight) > 0.5;
      if (hasPreviousMeasuredHeight && heightChanged) {
        textarea.style.height = `${previousMeasuredHeight}px`;
        void textarea.offsetHeight;
        if (animateHeight) {
          restoreComposerInputTransition(textarea, previousInlineTransition);
        } else {
          scheduleComposerTransitionRestore(textarea, previousInlineTransition);
        }
      } else if (hasPreviousMeasuredHeight && forceRefreshMetrics) {
        textarea.style.height = `${previousMeasuredHeight}px`;
        restoreComposerInputTransition(textarea, previousInlineTransition);
        composerLastAppliedHeightRef.current = nextHeight;
      } else if (Math.abs(shrinkProbeHeight - nextHeight) <= 0.5) {
        restoreComposerInputTransition(textarea, previousInlineTransition);
        composerLastAppliedHeightRef.current = nextHeight;
      } else {
        scheduleComposerTransitionRestore(textarea, previousInlineTransition);
      }
    }

    if (composerLastAppliedHeightRef.current !== nextHeight) {
      textarea.style.height = `${nextHeight}px`;
      composerLastAppliedHeightRef.current = nextHeight;
    }
    if (composerLastAppliedOverflowYRef.current !== nextOverflowY) {
      textarea.style.overflowY = nextOverflowY;
      composerLastAppliedOverflowYRef.current = nextOverflowY;
    }
    composerLastMeasuredDraftLengthRef.current = currentDraftLength;
  }

  function scheduleComposerResize(forceRefreshMetrics = false, animateHeight = true) {
    if (!activeSessionId) {
      return;
    }

    composerResizeNeedsMetricRefreshRef.current =
      composerResizeNeedsMetricRefreshRef.current || forceRefreshMetrics;
    composerResizeShouldAnimateHeightRef.current =
      composerResizeShouldAnimateHeightRef.current && animateHeight;
    if (composerResizeAnimationFrameRef.current != null) {
      return;
    }

    composerResizeAnimationFrameRef.current = window.requestAnimationFrame(() => {
      composerResizeAnimationFrameRef.current = null;
      const shouldRefreshMetrics = composerResizeNeedsMetricRefreshRef.current;
      const shouldAnimateHeight = composerResizeShouldAnimateHeightRef.current;
      composerResizeNeedsMetricRefreshRef.current = false;
      composerResizeShouldAnimateHeightRef.current = true;
      resizeComposerInput(shouldRefreshMetrics, shouldAnimateHeight);
    });
  }

  useEffect(() => {
    const textarea = composerInputRef.current;
    if (!textarea || typeof ResizeObserver === "undefined") {
      return;
    }

    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    let previousWidth = textarea.getBoundingClientRect().width;
    let previousAvailablePanelHeight =
      panelSlotElement?.clientHeight ??
      (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const resizeObserver = new ResizeObserver((entries) => {
      const nextWidth =
        entries.find((entry) => entry.target === textarea)?.contentRect.width ??
        textarea.getBoundingClientRect().width;
      const nextAvailablePanelHeight =
        panelSlotElement?.clientHeight ??
        (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
      const widthChanged = Math.abs(nextWidth - previousWidth) >= 1;
      const panelHeightChanged =
        Math.abs(nextAvailablePanelHeight - previousAvailablePanelHeight) >= 1;

      if (!widthChanged && !panelHeightChanged) {
        return;
      }

      previousWidth = nextWidth;
      previousAvailablePanelHeight = nextAvailablePanelHeight;
      scheduleComposerResize(widthChanged || panelHeightChanged);
    });

    resizeObserver.observe(textarea);
    if (panelSlotElement instanceof HTMLElement) {
      resizeObserver.observe(panelSlotElement);
    } else if (panelElement instanceof HTMLElement) {
      resizeObserver.observe(panelElement);
    }

    return () => {
      resizeObserver.disconnect();
    };
  }, [activeSessionId]);

  useEffect(() => {
    return () => {
      resetComposerSizingState();
      resetAndCancelScheduledComposerResize();
      cancelAndRestoreScheduledComposerTransition();
    };
  }, []);

  return {
    composerInputRef,
    resetAndCancelScheduledComposerResize,
    resetComposerSizingState,
    cancelAndRestoreScheduledComposerTransition,
    resizeComposerInput,
    scheduleComposerResize,
  };
}
