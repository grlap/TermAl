// Shared thresholds and convergence helpers for long, virtualized message
// lists.
//
// What this file owns:
//   - `SESSION_STICKY_BOTTOM_BAND_PX` — the shared near-bottom geometry
//     tolerance used by both pane scroll ownership and virtualization.
//   - `resolveSettledScrollMinimumAttempts` — picks how many
//     "settled" measurement attempts to require before calling a
//     virtualized scroll position stable. Long conversations
//     (more than 12 attempts allowed) default to 8, shorter ones
//     to 4; callers can override via the optional `minAttempts`
//     argument, and the result is clamped to the cap.
//
// What this file does NOT own:
//   - The React state/ref record that stores pane scroll position.
//   - The virtualized list logic itself — see
//     `./panels/AgentSessionPanel.tsx`.
//   - Any DOM side effects — both helpers take the values they need
//     as arguments.
//
// Split out of `ui/src/App.tsx`.

export const SESSION_STICKY_BOTTOM_BAND_PX = 72;

export function resolveSettledScrollMinimumAttempts(
  maxAttempts: number,
  minAttempts?: number,
) {
  // Long virtualized conversations can keep moving the bottom while cards
  // measure, even after scrollHeight looks stable for a frame or two.
  const defaultMinimumAttempts = maxAttempts > 12 ? 8 : 4;
  return Math.min(minAttempts ?? defaultMinimumAttempts, maxAttempts);
}
