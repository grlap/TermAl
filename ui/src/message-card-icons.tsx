// Inline SVG icons used by the message cards.
//
// What this file owns:
//   - `CopyIcon`, `CheckIcon` — copy-to-clipboard button states.
//   - `ExpandIcon`, `CollapseIcon` — diff-card expand/collapse.
//   - `DialogCloseIcon` — the dialog "close" affordance; exported
//     because the Settings dialog shell reuses it.
//   - `PreviewIcon` — the "open preview" affordance used by the
//     diff cards.
//
// Every icon is a plain stateless SVG with
// `aria-hidden="true" focusable="false"`, sized for the 16x16
// `command-icon` class (the close icon is a bit larger and uses
// its own sizing from the dialog stylesheet). They do not close
// over any React state, hooks, or props — just return the same
// SVG every render.
//
// What this file does NOT own:
//   - The buttons that wrap these icons — those live with their
//     parent components (`CommandCard`, `DiffCard`, the
//     preferences dialog shell, etc.).
//   - Any interactive logic (copy-to-clipboard, expand/collapse,
//     close-dialog). This file is purely presentational SVG.
//
// Split out of `ui/src/message-cards.tsx`. Same SVG markup, same
// `command-icon` classNames, same ARIA wiring; consumers import
// from here directly. `DialogCloseIcon` stays re-exported from
// `./message-cards` so existing callers (`SettingsDialogShell`)
// keep working without an import path change.

export function CopyIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M5 2.5h6.5A1.5 1.5 0 0 1 13 4v7.5A1.5 1.5 0 0 1 11.5 13H5A1.5 1.5 0 0 1 3.5 11.5V4A1.5 1.5 0 0 1 5 2.5Zm0 1a.5.5 0 0 0-.5.5v7.5a.5.5 0 0 0 .5.5h6.5a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H5Z"
        fill="currentColor"
      />
      <path
        d="M2.5 5.5a.5.5 0 0 1 .5.5v6A1.5 1.5 0 0 0 4.5 13.5h5a.5.5 0 0 1 0 1h-5A2.5 2.5 0 0 1 2 12V6a.5.5 0 0 1 .5-.5Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function CheckIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M13.35 4.65a.5.5 0 0 1 0 .7l-6 6a.5.5 0 0 1-.7 0l-3-3a.5.5 0 1 1 .7-.7L7 10.29l5.65-5.64a.5.5 0 0 1 .7 0Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function ExpandIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M2.5 6V2.5H6v1H4.2l2.15 2.15-.7.7L3.5 4.2V6h-1Zm11 0V4.2l-2.15 2.15-.7-.7L12.8 3.5H11v-1h3.5V6h-1ZM6.35 10.35l.7.7L4.2 13.9H6v1H2.5v-3.5h1V12.8l2.85-2.45Zm4.6.7.7-.7 2.85 2.45v-1.4h1v3.5H11v-1h1.8l-2.85-2.85Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function CollapseIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M6.35 6.35 5.65 7.05 3.5 4.9V6h-1V2.5H6v1H4.9l1.45 1.45Zm3.3 0L11.1 4.9H10v-1h3.5V6h-1V4.9l-2.15 2.15-.7-.7Zm-3.3 3.3.7.7L4.9 12.5H6v1H2.5V10h1v1.1l2.15-2.15Zm3.3.7.7-.7 2.15 2.15V10h1v3.5H10v-1h1.1l-1.45-1.45Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function DialogCloseIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M4.25 4.25 11.75 11.75M11.75 4.25 4.25 11.75"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.6"
      />
    </svg>
  );
}

export function PreviewIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M8 3c3.38 0 6.18 2.35 7 5-.82 2.65-3.62 5-7 5S1.82 10.65 1 8c.82-2.65 3.62-5 7-5Zm0 1C5.2 4 2.82 5.82 2.05 8 2.82 10.18 5.2 12 8 12s5.18-1.82 5.95-4C13.18 5.82 10.8 4 8 4Zm0 1.5A2.5 2.5 0 1 1 5.5 8 2.5 2.5 0 0 1 8 5.5Zm0 1A1.5 1.5 0 1 0 9.5 8 1.5 1.5 0 0 0 8 6.5Z"
        fill="currentColor"
      />
    </svg>
  );
}
