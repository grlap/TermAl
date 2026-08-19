// Owns detached message-stack restoration after a pane/session scroll-scope
// change: animation-frame convergence, native-scroll write ownership, bounded
// timeout, and cancellation.
// Does not own tail-follow policy, response indicators, history loading, or
// virtualizer range selection; SessionPaneView.scroll.ts supplies those
// decisions through the host callbacks below.
// Split from: ui/src/SessionPaneView.scroll.ts.

const DETACHED_RESTORE_MAX_ATTEMPTS = 60;
const SCROLL_TOP_TOLERANCE_PX = 0.5;
const NATIVE_SCROLL_TOLERANCE_PX = 1;

type DetachedRestoreRecord = {
  expectedNativeTop: number | null;
  isRetrying: boolean;
  targetTop: number;
};

type DetachedRestoreHost = {
  getCurrentKey: () => string;
  getNode: () => HTMLElement | null;
  isTailFollowAttached: () => boolean;
  notifyPositionRestore: (node: HTMLElement) => void;
  publishReachablePosition: (top: number) => void;
  publishSavedTarget: (top: number) => void;
  publishUnloadedNewerHistory: () => void;
};

export type ScheduleDetachedRestoreOptions = {
  host: DetachedRestoreHost;
  key: string;
  targetTop: number;
};

export type ConsumeDetachedRestoreScrollOptions = {
  key: string;
  node: HTMLElement;
  publishSavedTarget: (top: number) => void;
};

export type DetachedScrollRestoreController = ReturnType<
  typeof createDetachedScrollRestoreController
>;

function clampScrollTop(targetTop: number, maxScrollTop: number) {
  return Math.min(Math.max(targetTop, 0), maxScrollTop);
}

export function createDetachedScrollRestoreController() {
  const restoreByKey = new Map<string, DetachedRestoreRecord>();
  let pendingRestore: { cancel: () => void; key: string } | null = null;

  function cancel(key: string) {
    if (pendingRestore?.key === key) {
      const pendingCancel = pendingRestore.cancel;
      pendingRestore = null;
      pendingCancel();
    }
    restoreByKey.delete(key);
  }

  function consumeNativeScroll({
    key,
    node,
    publishSavedTarget,
  }: ConsumeDetachedRestoreScrollOptions) {
    const detachedRestore = restoreByKey.get(key);
    if (!detachedRestore) {
      return false;
    }

    // Native scroll events can arrive after either the clamped first write or
    // the final write of a virtualized restore. Explicit navigation cancels
    // this controller before moving. Keep the saved absolute target while
    // convergence is active, then release ownership so later virtualizer
    // anchor corrections can publish their real positions.
    const matchesExpectedWrite =
      detachedRestore.expectedNativeTop !== null &&
      Math.abs(node.scrollTop - detachedRestore.expectedNativeTop) <=
        NATIVE_SCROLL_TOLERANCE_PX;
    if (matchesExpectedWrite || detachedRestore.isRetrying) {
      publishSavedTarget(detachedRestore.targetTop);
      if (matchesExpectedWrite) {
        detachedRestore.expectedNativeTop = null;
        if (!detachedRestore.isRetrying) {
          cancel(key);
        }
      }
      return true;
    }

    cancel(key);
    return false;
  }

  function schedule({
    host,
    key: restoreKey,
    targetTop,
  }: ScheduleDetachedRestoreOptions) {
    cancel(restoreKey);
    const detachedRestore: DetachedRestoreRecord = {
      expectedNativeTop: null,
      isRetrying: true,
      targetTop,
    };
    restoreByKey.set(restoreKey, detachedRestore);
    let frameId = 0;
    let releaseFrameId = 0;
    let attempts = 0;
    let cancelled = false;
    const initialNode = host.getNode();
    let previousMaxScrollTop = Math.max(
      (initialNode?.scrollHeight ?? 0) - (initialNode?.clientHeight ?? 0),
      0,
    );

    const clearPendingCancel = () => {
      if (pendingRestore?.cancel === cleanup) {
        pendingRestore = null;
      }
    };
    const cleanup = () => {
      cancelled = true;
      clearPendingCancel();
      if (restoreByKey.get(restoreKey) === detachedRestore) {
        restoreByKey.delete(restoreKey);
      }
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
        frameId = 0;
      }
      if (releaseFrameId !== 0) {
        window.cancelAnimationFrame(releaseFrameId);
        releaseFrameId = 0;
      }
    };
    const finish = () => {
      detachedRestore.isRetrying = false;
      if (detachedRestore.expectedNativeTop === null) {
        cleanup();
        return;
      }
      // Browser native scroll delivery normally consumes the marker first.
      // This one-frame bound prevents a missing/coalesced native event from
      // turning restore ownership into permanent scroll authority.
      releaseFrameId = window.requestAnimationFrame(() => {
        releaseFrameId = 0;
        cleanup();
      });
    };
    const restoreTarget = () => {
      const node = host.getNode();
      const currentRestore = restoreByKey.get(restoreKey);
      if (
        !node ||
        currentRestore !== detachedRestore ||
        currentRestore.targetTop !== targetTop ||
        host.isTailFollowAttached()
      ) {
        return false;
      }

      const maxScrollTop = Math.max(
        node.scrollHeight - node.clientHeight,
        0,
      );
      const nextTop = clampScrollTop(targetTop, maxScrollTop);
      detachedRestore.expectedNativeTop = null;
      host.publishSavedTarget(targetTop);
      if (Math.abs(node.scrollTop - nextTop) > SCROLL_TOP_TOLERANCE_PX) {
        node.scrollTop = nextTop;
        detachedRestore.expectedNativeTop = node.scrollTop;
      }
      // The virtualizer must adopt restore authority even when the reused DOM
      // already has this exact numeric offset.
      host.notifyPositionRestore(node);
      return targetTop <= maxScrollTop + NATIVE_SCROLL_TOLERANCE_PX;
    };
    const tick = () => {
      frameId = 0;
      if (
        cancelled ||
        host.getCurrentKey() !== restoreKey ||
        restoreByKey.get(restoreKey) !== detachedRestore
      ) {
        return;
      }

      attempts += 1;
      const node = host.getNode();
      const maxScrollTop = node
        ? Math.max(node.scrollHeight - node.clientHeight, 0)
        : previousMaxScrollTop;
      const geometryChanged =
        Math.abs(maxScrollTop - previousMaxScrollTop) >
        SCROLL_TOP_TOLERANCE_PX;
      previousMaxScrollTop = maxScrollTop;
      const nextTop = clampScrollTop(targetTop, maxScrollTop);
      const actualPositionChanged = node
        ? Math.abs(node.scrollTop - nextTop) > SCROLL_TOP_TOLERANCE_PX
        : false;
      const reachedTarget =
        node && (geometryChanged || actualPositionChanged)
          ? restoreTarget()
          : Boolean(
              node &&
                targetTop <= maxScrollTop + NATIVE_SCROLL_TOLERANCE_PX &&
                Math.abs(node.scrollTop - targetTop) <=
                  SCROLL_TOP_TOLERANCE_PX,
            );
      if (reachedTarget) {
        finish();
        return;
      }
      if (attempts >= DETACHED_RESTORE_MAX_ATTEMPTS) {
        // A virtualizer may never make the old absolute offset reachable
        // after history truncation or materially different measurements.
        // Release authority deterministically at the actual clamped position.
        detachedRestore.expectedNativeTop = null;
        detachedRestore.isRetrying = false;
        if (node) {
          host.publishReachablePosition(node.scrollTop);
        }
        host.publishUnloadedNewerHistory();
        cleanup();
        return;
      }
      frameId = window.requestAnimationFrame(tick);
    };

    pendingRestore = {
      cancel: cleanup,
      key: restoreKey,
    };
    // Verify once after the virtualizer commits the position_restore range.
    // Outgoing DOM can make the target look synchronously reachable while the
    // incoming range changes geometry before the next paint.
    restoreTarget();
    frameId = window.requestAnimationFrame(tick);
    return cleanup;
  }

  return {
    cancel,
    consumeNativeScroll,
    schedule,
  };
}
