// Owns conversation-marker color parsing for browser render boundaries.
// Mirrors the backend hex-only marker color validator in src/session_markers.rs
// and provides the display fallback used by marker chips and the overview rail.
// Does not own marker state reconciliation policy; callers that compare marker
// records should use conversation-marker-state-equality.ts.

export const DEFAULT_CONVERSATION_MARKER_COLOR = "#3b82f6";

const CONVERSATION_MARKER_HEX_COLOR_PATTERN =
  /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

export function normalizeConversationMarkerColor(value: unknown) {
  return (
    canonicalConversationMarkerColor(value) ?? DEFAULT_CONVERSATION_MARKER_COLOR
  );
}

export function canonicalConversationMarkerColor(value: unknown) {
  if (typeof value !== "string") {
    return null;
  }
  const trimmed = value.trim();
  if (!CONVERSATION_MARKER_HEX_COLOR_PATTERN.test(trimmed)) {
    return null;
  }
  return trimmed.toLowerCase();
}
