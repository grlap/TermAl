// Owns: pure resize-threshold and accumulated-baseline policy for an attached
// session transcript.
// Does not own: ResizeObserver wiring, tail-follow state, or scroll writes.
// Split from: ui/src/SessionPaneView.scroll.ts.

export function shouldRepinSessionPaneAfterResize({
  activePageChanged,
  contentHeightDelta,
  shouldRepinEveryMeasuredPixel,
  viewportHeightDelta,
}: {
  activePageChanged: boolean;
  contentHeightDelta: number;
  shouldRepinEveryMeasuredPixel: boolean;
  viewportHeightDelta: number;
}) {
  const contentChangeThreshold = shouldRepinEveryMeasuredPixel ? 0.5 : 2;
  return (
    activePageChanged ||
    Math.abs(contentHeightDelta) > contentChangeThreshold ||
    Math.abs(viewportHeightDelta) > 0.5
  );
}

export function resolveSessionPaneResizeMeasurement({
  activePageChanged,
  nextContentHeight,
  nextViewportHeight,
  previousContentHeight,
  previousViewportHeight,
  shouldRepinEveryMeasuredPixel,
}: {
  activePageChanged: boolean;
  nextContentHeight: number;
  nextViewportHeight: number;
  previousContentHeight: number;
  previousViewportHeight: number;
  shouldRepinEveryMeasuredPixel: boolean;
}) {
  const shouldRepin = shouldRepinSessionPaneAfterResize({
    activePageChanged,
    contentHeightDelta: nextContentHeight - previousContentHeight,
    shouldRepinEveryMeasuredPixel,
    viewportHeightDelta: nextViewportHeight - previousViewportHeight,
  });
  return {
    nextContentHeightBaseline: shouldRepin
      ? nextContentHeight
      : previousContentHeight,
    nextViewportHeightBaseline: shouldRepin
      ? nextViewportHeight
      : previousViewportHeight,
    shouldRepin,
  };
}
