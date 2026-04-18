// Small types and helpers for the banner/state that surfaces how
// the UI is currently talking to the backend.
//
// What this file owns:
//   - `BackendConnectionState` — the four-variant discriminated
//     union used by the shell's connection indicator:
//     `"connecting"` (initial handshake in progress),
//     `"connected"` (SSE open + last sync succeeded),
//     `"reconnecting"` (actively recovering), `"offline"` (the
//     browser reports no network).
//   - `BACKEND_UNAVAILABLE_ISSUE_DETAIL` — the generic copy shown
//     when the backend is reachable over TCP but responding with
//     HTML instead of JSON (e.g., stale dev server) and the
//     incompatible-server helper did not flag it as
//     `restartRequired`.
//   - `BACKEND_SYNC_ISSUE_DETAIL` — the generic copy shown when
//     the backend is reachable but a live state update failed to
//     apply.
//   - `describeBackendConnectionIssueDetail` — picks between the
//     above two strings based on whether the error is a backend
//     unavailability (`isBackendUnavailableError`), forwarding
//     the server's restart-required message when applicable.
//
// What this file does NOT own:
//   - The reconnect logic (SSE setup, fallback polling, watchdog)
//     — that lives in App.tsx and `./live-updates`. This module
//     only provides the display-side vocabulary.
//   - Rendering — `ControlPanelConnectionIndicator` consumes
//     `BackendConnectionState` via its own props.
//
// Split out of `ui/src/App.tsx`. Same values, same strings, same
// function signatures.

import { isBackendUnavailableError } from "./api";

export type BackendConnectionState =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "offline";

export const BACKEND_UNAVAILABLE_ISSUE_DETAIL =
  "Could not reach the TermAl backend. Retrying automatically.";

export const BACKEND_SYNC_ISSUE_DETAIL =
  "A live backend update could not be processed. Waiting for the next successful sync.";

export function describeBackendConnectionIssueDetail(error: unknown) {
  if (isBackendUnavailableError(error)) {
    // Incompatible backend serving HTML instead of JSON — surface the restart
    // instruction directly rather than the generic connectivity message.
    return error.restartRequired
      ? error.message
      : BACKEND_UNAVAILABLE_ISSUE_DETAIL;
  }
  return BACKEND_SYNC_ISSUE_DETAIL;
}
