// Owns: the per-node record of how much of a message stack's scroll extent is
// estimated space for history that is not loaded yet, and the clamp that keeps
// app-owned incremental scrolling (Arrow/Page keys, wheel) inside loaded
// messages, so the reader waits at the edge while the next page loads instead
// of scrolling into blank space and being moved when it arrives.
// It also measures how much of the viewport shows unloaded history, for page
// demand.
// Does not own: loading history, the estimates themselves (the conversation
// history scroll publishes them), or native scrollbar drags, which the
// position seek handles.
// New module; not split out of an existing file.

export type MessageStackUnloadedHistoryPx = { before: number; after: number };

// Estimates are fractional while scrollTop is snapped to device pixels, so a
// viewport resting on the edge can read a pixel short of it.
const EDGE_TOLERANCE_PX = 2;

const unloadedHistoryByNode = new WeakMap<HTMLElement, MessageStackUnloadedHistoryPx>();

// Dispatched on the stack when its estimates change: the viewport can come to
// show unloaded space without any scroll event (a page lands, the window
// moves), and page demand must look again.
export const MESSAGE_STACK_UNLOADED_HISTORY_EVENT =
  "termal:message-stack-unloaded-history";

export function publishMessageStackUnloadedHistory(
  node: HTMLElement,
  extent: MessageStackUnloadedHistoryPx,
) {
  const previous = unloadedHistoryByNode.get(node);
  unloadedHistoryByNode.set(node, extent);
  if (
    previous?.before !== extent.before ||
    previous?.after !== extent.after
  ) {
    node.dispatchEvent(new CustomEvent(MESSAGE_STACK_UNLOADED_HISTORY_EVENT));
  }
}

// Which edge of the loaded messages shows unloaded history inside the
// viewport, while loaded messages are visible too. A viewport wholly inside
// the estimate shows neither: that position is the position seek's to fill.
export function measureVisibleUnloadedHistory(node: HTMLElement): {
  above: boolean;
  below: boolean;
} {
  const extent = unloadedHistoryByNode.get(node);
  if (!extent) {
    return { above: false, below: false };
  }
  const top = node.scrollTop;
  const bottom = top + node.clientHeight;
  const loadedTop = extent.before;
  const loadedBottom = node.scrollHeight - extent.after;
  const showsLoaded =
    bottom > loadedTop + EDGE_TOLERANCE_PX &&
    top < loadedBottom - EDGE_TOLERANCE_PX;
  return {
    above: showsLoaded && extent.before > 0 && top < loadedTop - EDGE_TOLERANCE_PX,
    below:
      showsLoaded && extent.after > 0 && bottom > loadedBottom + EDGE_TOLERANCE_PX,
  };
}

// Returns the scrollTop an incremental move toward `target` may reach. A
// viewport that is inside loaded messages stops at their edge; one that is
// already outside them (after a scrollbar drag) is left alone.
export function clampScrollTopToLoadedHistory(
  node: HTMLElement,
  target: number,
): number {
  const extent = unloadedHistoryByNode.get(node);
  if (!extent) {
    return target;
  }
  const current = node.scrollTop;
  if (
    target < current &&
    extent.before > 0 &&
    current >= extent.before - EDGE_TOLERANCE_PX
  ) {
    return Math.max(target, Math.min(extent.before, current));
  }
  const loadedBottom = node.scrollHeight - extent.after;
  if (
    target > current &&
    extent.after > 0 &&
    current + node.clientHeight <= loadedBottom + EDGE_TOLERANCE_PX
  ) {
    return Math.min(target, Math.max(loadedBottom - node.clientHeight, current));
  }
  return target;
}
