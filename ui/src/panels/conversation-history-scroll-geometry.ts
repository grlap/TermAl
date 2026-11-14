// Owns estimated missing-history extent and native scroll-to-position mapping.
// Does not fetch history or change pane scroll authority.
// Extracted from the navigation responsibility of conversation-overview-controller.ts.
export const DEFAULT_HISTORY_MESSAGE_HEIGHT = 200;
const MAX_ESTIMATED_HISTORY_HEIGHT = 8_000_000;

/** `fixedUnit` keeps an earlier per-message estimate instead of the resident average. */
export function historyScrollExtent(count: number, start: number, resident: number, residentHeight: number, fixedUnit?: number) {
  const total = Math.max(0, count);
  const first = Math.min(Math.max(0, start), total);
  const loaded = Math.min(Math.max(0, resident), total - first);
  const missing = total - loaded;
  const average = fixedUnit !== undefined && fixedUnit > 0 ? fixedUnit
    : resident > 0 && residentHeight > 0 ? residentHeight / resident : DEFAULT_HISTORY_MESSAGE_HEIGHT;
  const unit = Math.min(Math.max(1, average), MAX_ESTIMATED_HISTORY_HEIGHT / Math.max(1, missing));
  return { prefix: first * unit, suffix: Math.max(0, total - first - loaded) * unit, unit, residentHeight: Math.max(0, residentHeight), start: first, resident: loaded, count: total };
}

export type HistoryScrollExtent = ReturnType<typeof historyScrollExtent>;

/** Keeps page positions, native extent, and imperative jumps in one space. */
export function withHistoryExtent<T extends { tops: number[]; totalHeight: number }>(
  layout: T, beforePx = 0, afterPx = 0,
) {
  if (beforePx === 0 && afterPx === 0) return layout;
  return { ...layout, tops: layout.tops.map(top => top + beforePx), totalHeight: layout.totalHeight + beforePx + afterPx };
}

export function unloadedHistoryPosition(offset: number, extent: HistoryScrollExtent): number | null {
  if (extent.count === 0) return null;
  if (offset < extent.prefix && extent.prefix > 0) {
    // Counted back from the loaded messages, like the suffix below: the same
    // as offset / unit while the estimate starts at zero, and still adjacent
    // to them after a placement moved the estimate.
    return Math.min(extent.start - 1, Math.max(0, extent.start - Math.ceil((extent.prefix - offset) / extent.unit)));
  }
  const loadedEnd = extent.prefix + extent.residentHeight;
  if (offset >= loadedEnd && extent.suffix > 0) {
    return Math.min(extent.count - 1, extent.start + extent.resident + Math.floor((offset - loadedEnd) / extent.unit));
  }
  return null;
}
