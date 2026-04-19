// Shared guard for the `onMouseDown -> onClose` pattern used by
// every modal backdrop (`.dialog-backdrop`) in the app.
//
// What this file owns:
//   - `isDialogBackdropDismissMouseDown(event)` — the predicate
//     that decides whether a `mousedown` on a modal backdrop
//     should dismiss the dialog. Returns `false` for non-primary
//     buttons (middle-click paste anchor on Linux, physical
//     right-click context menu on every platform) AND for macOS
//     Ctrl-click, which fires as a primary-button `mousedown`
//     (`button === 0`) but the OS interprets as a secondary-click
//     gesture that opens a context menu. Without the Ctrl guard
//     on macOS the dialog would close before the browser could
//     open the menu.
//   - A local `isApplePlatform()` helper that reads
//     `navigator.userAgentData?.platform` (modern Chromium)
//     falling back to `navigator.platform` (Firefox / Safari /
//     older Chromium). Matches the `detectPlatform` logic in
//     `./pane-keyboard.ts` but stays local so a dialog import
//     does not pull in keyboard-routing code.
//
// What this file does NOT own:
//   - Stopping propagation on the dialog body — each shell still
//     attaches its own `onMouseDown={event.stopPropagation}` to
//     the inner `<section>` so inside-the-card interactions do
//     not bubble up to the backdrop.
//   - The `onClose` side effects (releasing draft attachments,
//     clearing request errors, etc.) — those live with the
//     dialog shell that owns the state.
//   - Dialog-open/close orchestration (keyboard Escape, focus
//     trap, ARIA wiring) — dialog shells handle those.
//
// Consumed by:
//   - `./preferences/SettingsDialogShell.tsx`
//   - `./App.tsx` (create-session + create-project inline dialogs)
//
// Kept as a tiny standalone module so the predicate can be
// unit-tested against stubbed `navigator.platform` values without
// mounting a dialog, and so future dialog shells can adopt the
// same contract without re-implementing the three-button +
// ctrl-on-mac check.

export function isDialogBackdropDismissMouseDown(
  event: Pick<MouseEvent, "button" | "ctrlKey">,
): boolean {
  if (event.button !== 0) {
    // Middle-click (1) triggers paste-anchor on Linux / auto-scroll
    // on Windows; right-click (2) opens the native context menu.
    // Either gesture gets swallowed if we dismiss here.
    return false;
  }

  if (event.ctrlKey && isApplePlatform()) {
    // macOS Ctrl-click fires with `button === 0` but the OS
    // escalates it to a secondary-click context menu.
    return false;
  }

  return true;
}

function isApplePlatform(): boolean {
  if (typeof navigator === "undefined") {
    return false;
  }

  const navigatorWithUserAgentData = navigator as Navigator & {
    userAgentData?: { platform?: string };
  };
  const platform =
    navigatorWithUserAgentData.userAgentData?.platform ??
    navigator.platform ??
    "";

  return /mac|iphone|ipad|ipod/i.test(platform);
}
