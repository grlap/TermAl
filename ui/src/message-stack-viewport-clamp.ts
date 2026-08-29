// Owns one geometry question shared by the session pane and the virtualized
// message list: did a scrollTop drop happen because the scroll viewport grew
// while the reader was pinned to the bottom (the browser clamps scrollTop to
// the new, smaller maximum), rather than because the reader scrolled up?
// Does not own scroll event handling, tail-follow intent, or the
// new-response indicator; callers use this to keep a clamped-but-still-at-
// bottom reader attached instead of treating the clamp as user movement.

const VIEWPORT_CLAMP_TOLERANCE_PX = 1;

export type ViewportGrowthClampGeometry = {
  previousScrollTop: number;
  previousScrollHeight: number;
  previousClientHeight: number;
  currentScrollTop: number;
  currentScrollHeight: number;
  currentClientHeight: number;
};

// True only when every part of the frame is explained by the viewport growing
// under a bottom-pinned reader: the viewport got taller, the content did not
// shrink, scrollTop went down by no more than the viewport growth, and the
// reader is still at the physical bottom afterwards. A genuine upward scroll
// leaves the reader away from the bottom, so it never matches.
export function isViewportGrowthBottomClamp({
  previousScrollTop,
  previousScrollHeight,
  previousClientHeight,
  currentScrollTop,
  currentScrollHeight,
  currentClientHeight,
}: ViewportGrowthClampGeometry): boolean {
  const clientHeightGrowth = currentClientHeight - previousClientHeight;
  if (clientHeightGrowth < 0.5) {
    return false;
  }
  if (currentScrollHeight < previousScrollHeight - 0.5) {
    return false;
  }
  const scrollTopDrop = previousScrollTop - currentScrollTop;
  if (scrollTopDrop < 0.5) {
    return false;
  }
  if (scrollTopDrop > clientHeightGrowth + VIEWPORT_CLAMP_TOLERANCE_PX) {
    return false;
  }
  const distanceFromBottom =
    currentScrollHeight - currentScrollTop - currentClientHeight;
  return distanceFromBottom <= VIEWPORT_CLAMP_TOLERANCE_PX;
}
