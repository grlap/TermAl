// Pure helpers for tracking and converging scroll position in
// long, virtualized message lists.
//
// What this file owns:
//   - `syncMessageStackScrollPosition` — reads a scrollable node's
//     `scrollHeight` / `scrollTop` / `clientHeight`, writes the
//     resulting `{ top, shouldStick }` into the caller's
//     per-pane scroll-position record, and returns the same
//     `{ top, shouldStick }`. `shouldStick` is true when the user
//     is within 72 px of the bottom (the "sticky-bottom" zone); the
//     calling code later uses the value to decide whether the
//     next layout pass should re-pin to the latest message.
//   - `resolveSettledScrollMinimumAttempts` — picks how many
//     "settled" measurement attempts to require before calling a
//     virtualized scroll position stable. Long conversations
//     (more than 12 attempts allowed) default to 8, shorter ones
//     to 4; callers can override via the optional `minAttempts`
//     argument, and the result is clamped to the cap.
//
// What this file does NOT own:
//   - The React state that records scroll position (that lives in
//     `App.tsx` on `paneScrollPositionsRef.current`).
//   - The virtualized list logic itself — see
//     `./panels/AgentSessionPanel.tsx`. That file deliberately
//     mirrors the `< 72` sticky-bottom threshold; if this module's
//     threshold changes, the mirror has to move too.
//   - Any DOM side effects — both helpers take the values they need
//     as arguments.
//
// Split out of `ui/src/App.tsx`. Same function signatures and
// behaviour as the inline definitions they replaced; consumers
// (including `App.test.tsx`) import from here directly.

export function syncMessageStackScrollPosition(
  node: Pick<HTMLElement, "scrollHeight" | "scrollTop" | "clientHeight">,
  scrollStateKey: string,
  paneScrollPositions: Record<string, { top: number; shouldStick: boolean }>,
) {
  const shouldStick =
    node.scrollHeight - node.scrollTop - node.clientHeight < 72;
  paneScrollPositions[scrollStateKey] = {
    top: node.scrollTop,
    shouldStick,
  };

  return {
    top: node.scrollTop,
    shouldStick,
  };
}

export function resolveSettledScrollMinimumAttempts(
  maxAttempts: number,
  minAttempts?: number,
) {
  // Long virtualized conversations can keep moving the bottom while cards
  // measure, even after scrollHeight looks stable for a frame or two.
  const defaultMinimumAttempts = maxAttempts > 12 ? 8 : 4;
  return Math.min(minAttempts ?? defaultMinimumAttempts, maxAttempts);
}
