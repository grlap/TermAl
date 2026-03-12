export type PaneTabTooltipAnchorRect = {
  bottom: number;
  left: number;
  width: number;
};

export type PaneTabStatusTooltipPosition = {
  arrowLeft: number;
  left: number;
  top: number;
  width: number;
};

const VIEWPORT_PADDING_PX = 12;
const TOOLTIP_GAP_PX = 9;
const TOOLTIP_MAX_WIDTH_PX = 608;
const TOOLTIP_ARROW_PADDING_PX = 18;

export function measurePaneTabStatusTooltipPosition(
  anchorRect: PaneTabTooltipAnchorRect,
  viewportWidth: number,
): PaneTabStatusTooltipPosition {
  const availableWidth = Math.max(viewportWidth - VIEWPORT_PADDING_PX * 2, 0);
  const width = Math.min(TOOLTIP_MAX_WIDTH_PX, availableWidth);
  const maxLeft = Math.max(viewportWidth - VIEWPORT_PADDING_PX - width, VIEWPORT_PADDING_PX);
  const left = clamp(anchorRect.left, VIEWPORT_PADDING_PX, maxLeft);
  const anchorCenter = anchorRect.left + anchorRect.width / 2;
  const arrowLeft = clamp(
    anchorCenter - left,
    TOOLTIP_ARROW_PADDING_PX,
    Math.max(width - TOOLTIP_ARROW_PADDING_PX, TOOLTIP_ARROW_PADDING_PX),
  );

  return {
    arrowLeft,
    left,
    top: anchorRect.bottom + TOOLTIP_GAP_PX,
    width,
  };
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}
