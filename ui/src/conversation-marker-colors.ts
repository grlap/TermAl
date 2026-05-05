export const DEFAULT_CONVERSATION_MARKER_COLOR = "#3b82f6";

const CONVERSATION_MARKER_HEX_COLOR_PATTERN =
  /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

export function normalizeConversationMarkerColor(value: unknown) {
  if (typeof value !== "string") {
    return DEFAULT_CONVERSATION_MARKER_COLOR;
  }
  const trimmed = value.trim();
  if (!CONVERSATION_MARKER_HEX_COLOR_PATTERN.test(trimmed)) {
    return DEFAULT_CONVERSATION_MARKER_COLOR;
  }
  return trimmed.toLowerCase();
}
