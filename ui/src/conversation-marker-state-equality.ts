// Owns state-reconciliation equality for conversation marker records.
// Kept separate from render color normalization so invalid persisted/remote
// colors do not accidentally compare equal to the safe display fallback. Valid
// hex colors compare by canonical lowercase form; invalid values compare only
// by raw identity so authoritative safe snapshots can replace corrupt local
// state.

import { canonicalConversationMarkerColor } from "./conversation-marker-colors";

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
