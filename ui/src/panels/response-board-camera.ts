// Owns Response Board camera math and legacy zoom persistence.
// Deliberately does not own React state, DOM gestures, or workspace layout writes.
// Split from ResponseBoardPanel.tsx.

import type { ResponseBoardCard } from "../api";
import type { WorkspaceResponseBoardView } from "../workspace-types";

const BOARD_PADDING = 72;
const MIN_BOARD_ZOOM = 0.25;
const MAX_BOARD_ZOOM = 2;

export const RESPONSE_BOARD_ZOOM_STORAGE_KEY =
  "termal.response-board.zoom.v1";

export function clampBoardZoom(value: number) {
  return Math.min(MAX_BOARD_ZOOM, Math.max(MIN_BOARD_ZOOM, value));
}

export function readStoredBoardZoom() {
  try {
    const stored = Number(
      window.localStorage.getItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY),
    );
    return Number.isFinite(stored) && stored > 0 ? clampBoardZoom(stored) : 1;
  } catch {
    return 1;
  }
}

export function wheelRequestsBoardZoom(event: WheelEvent) {
  return event.ctrlKey || event.getModifierState("Fn");
}

export function zoomBoardViewAtPoint(
  view: WorkspaceResponseBoardView,
  requestedZoom: number,
  surfaceX: number,
  surfaceY: number,
): WorkspaceResponseBoardView {
  const zoom = clampBoardZoom(requestedZoom);
  if (zoom === view.zoom) {
    return view;
  }
  const logicalX = (surfaceX - view.panX) / view.zoom;
  const logicalY = (surfaceY - view.panY) / view.zoom;
  return {
    zoom,
    panX: surfaceX - logicalX * zoom,
    panY: surfaceY - logicalY * zoom,
  };
}

export function responseBoardViewShowsAnyCard(
  view: WorkspaceResponseBoardView,
  cards: ResponseBoardCard[],
  viewportWidth: number,
  viewportHeight: number,
) {
  return cards.some((card) => {
    const left = card.x * view.zoom + view.panX;
    const top = card.y * view.zoom + view.panY;
    const right = (card.x + card.w) * view.zoom + view.panX;
    const bottom = (card.y + card.h) * view.zoom + view.panY;
    return (
      right > 0 &&
      bottom > 0 &&
      left < viewportWidth &&
      top < viewportHeight
    );
  });
}

export function fitResponseBoardCardsInView(
  cards: ResponseBoardCard[],
  viewportWidth: number,
  viewportHeight: number,
): WorkspaceResponseBoardView | null {
  if (cards.length === 0 || viewportWidth <= 0 || viewportHeight <= 0) {
    return null;
  }
  const minX = Math.min(...cards.map((card) => card.x));
  const minY = Math.min(...cards.map((card) => card.y));
  const maxX = Math.max(...cards.map((card) => card.x + card.w));
  const maxY = Math.max(...cards.map((card) => card.y + card.h));
  const contentWidth = Math.max(1, maxX - minX);
  const contentHeight = Math.max(1, maxY - minY);
  const availableWidth = Math.max(1, viewportWidth - BOARD_PADDING * 2);
  const availableHeight = Math.max(1, viewportHeight - BOARD_PADDING * 2);
  const zoom = clampBoardZoom(
    Math.min(1, availableWidth / contentWidth, availableHeight / contentHeight),
  );
  return {
    zoom,
    panX: (viewportWidth - contentWidth * zoom) / 2 - minX * zoom,
    panY: (viewportHeight - contentHeight * zoom) / 2 - minY * zoom,
  };
}
