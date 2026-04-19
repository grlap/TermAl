// Pure state-shape helpers for the latest on-disk snapshot the diff
// panel tracks alongside its source / review model. Nothing here
// touches React or Monaco — these are tiny functions that either
// build or translate a `LatestFileState` record.
//
// What this file owns:
//   - `LatestFileState` — the `{ status, path, content, contentHash?,
//     error, language }` record that represents what the panel last
//     read from disk for the file under review. The `status` field
//     is `"idle" | "loading" | "ready" | "error"`.
//   - `createInitialLatestFileState` — builds the initial record
//     for a given file path: `idle` with an empty path when there's
//     no file to show, otherwise `loading` with the target path so
//     the UI can render a spinner before the first fetch lands.
//   - `toLatestFileState` — converts an API `FileResponse` into a
//     `ready` record, threading through `path`, `content`,
//     `contentHash`, and `language`.
//   - `isStaleFileSaveError` — case-insensitive substring predicate
//     for the stale-file conflict error surfaced by the save
//     endpoint ("file changed on disk before save"). Used to
//     trigger the rebase-onto-disk path instead of failing the
//     save outright.
//
// What this file does NOT own:
//   - The `setLatestFileState` / `setLatestFile` React setters, the
//     `useState` declaration, or the save / fetch wiring that
//     drives the state machine — all of that stays in
//     `./DiffPanel.tsx`.
//   - `FileResponse` — lives with the rest of the API surface in
//     `../api`.
//   - Hash / content comparison semantics (stale detection, rebase)
//     — `./content-rebase.ts` owns that.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same record shape,
// same status strings, same stale-error substring match.

import type { FileResponse } from "../api";

export type LatestFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  contentHash?: string | null;
  error: string | null;
  language: string | null;
};

export function createInitialLatestFileState(filePath: string | null): LatestFileState {
  if (!filePath) {
    return {
      status: "idle",
      path: "",
      content: "",
      contentHash: null,
      error: null,
      language: null,
    };
  }

  return {
    status: "loading",
    path: filePath,
    content: "",
    contentHash: null,
    error: null,
    language: null,
  };
}

export function toLatestFileState(response: FileResponse): LatestFileState {
  return {
    status: "ready",
    path: response.path,
    content: response.content,
    contentHash: response.contentHash ?? null,
    error: null,
    language: response.language ?? null,
  };
}

export function isStaleFileSaveError(message: string) {
  return message.toLowerCase().includes("file changed on disk before save");
}
