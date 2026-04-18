// Pure helpers for the `SourceFileState` used by the source-view
// pane.
//
// What this file owns:
//   - `sourceFileStateFromResponse` — converts a successful
//     `FileResponse` into a `SourceFileState` with
//     `status: "ready"`, carrying path, content, hash, mtime,
//     size, and language through and defaulting the external /
//     stale tracking fields to their fresh-read defaults.
//   - `isSourceFileMissingError` — predicate for deciding whether
//     an error from the backend's file read path should be
//     treated as "the file disappeared from disk" versus a real
//     failure. Case-insensitive substring match against the error
//     message for `"file not found"` or `"not found"`.
//
// What this file does NOT own:
//   - The `SourceFileState` type itself — it lives next to the
//     panel that owns the state, at `./panels/SourcePanel`. This
//     module imports the type.
//   - Side effects (fetching, writing, state reconciliation) —
//     these helpers are pure data transforms.
//   - The broader source-view state machine (`setFileState`,
//     adoption, save flow) — that stays in `App.tsx` where the
//     React state lives.
//
// Split out of `ui/src/App.tsx`. Same signatures and behaviour as
// the inline definitions they replaced.

import type { FileResponse } from "./api";
import { getErrorMessage } from "./app-utils";
import type { SourceFileState } from "./panels/SourcePanel";

export function sourceFileStateFromResponse(
  response: FileResponse,
): SourceFileState {
  return {
    status: "ready",
    path: response.path,
    content: response.content,
    contentHash: response.contentHash ?? null,
    mtimeMs: response.mtimeMs ?? null,
    sizeBytes: response.sizeBytes ?? null,
    staleOnDisk: false,
    externalChangeKind: null,
    externalContentHash: null,
    externalMtimeMs: null,
    externalSizeBytes: null,
    error: null,
    language: response.language ?? null,
  };
}

export function isSourceFileMissingError(error: unknown) {
  const message = getErrorMessage(error).toLowerCase();
  return message.includes("file not found") || message.includes("not found");
}
