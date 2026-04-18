// Modal chrome for the Settings dialog.
//
// What this file owns:
//   - Backdrop element (`dialog-backdrop`) and its click-to-close
//     handler (stopPropagation on the inner <section> so clicks
//     inside the dialog body do not bubble up and dismiss).
//   - The dialog `<section>` (ARIA `role="dialog"`, `aria-modal="true"`,
//     `aria-labelledby="settings-dialog-title"`), its id
//     `settings-dialog`, and its header (label + title + description +
//     close button).
//   - The inner `<div class="settings-dialog-body">` wrapper where
//     the caller's `children` are rendered.
//
// What this file does NOT own:
//   - Whether the dialog is open — callers conditionally render
//     `<SettingsDialogShell>` only when their `isSettingsOpen` flag
//     is true.
//   - The tab bar itself (see `./SettingsTabBar.tsx`).
//   - The per-tab panel content (each preference panel is its own
//     component; the caller composes them as children).
//   - The active-tab state (still held by `App.tsx`).
//
// Split out of `ui/src/App.tsx` as the second step of the planned
// App.tsx -> preferences/* series. Behaviour-equivalent to the
// inline JSX it replaces — same classNames, same ARIA attributes,
// same element ids, same copy.

import type { ReactNode } from "react";
import { DialogCloseIcon } from "../message-cards";

export function SettingsDialogShell({
  onClose,
  children,
}: {
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <div
      className="dialog-backdrop"
      onMouseDown={() => {
        onClose();
      }}
    >
      <section
        id="settings-dialog"
        className="dialog-card panel settings-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-dialog-title"
        onMouseDown={(event) => {
          event.stopPropagation();
        }}
      >
        <div className="settings-dialog-header">
          <div>
            <div className="card-label">Preferences</div>
            <h2 id="settings-dialog-title">Settings</h2>
            <p className="dialog-copy settings-dialog-copy">
              Tune the interface and manage reusable orchestrator templates
              without disturbing active sessions.
            </p>
          </div>

          <button
            className="ghost-button settings-dialog-close"
            type="button"
            aria-label="Close dialog"
            title="Close"
            onClick={() => {
              onClose();
            }}
          >
            <DialogCloseIcon />
          </button>
        </div>

        <div className="settings-dialog-body">{children}</div>
      </section>
    </div>
  );
}
