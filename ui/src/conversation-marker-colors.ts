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

export function conversationMarkerColorsMatchForState(
  left: unknown,
  right: unknown,
) {
  const leftCanonical = canonicalConversationMarkerColor(left);
  const rightCanonical = canonicalConversationMarkerColor(right);
  if (leftCanonical !== null && rightCanonical !== null) {
    return leftCanonical === rightCanonical;
  }
  return left === right;
}
