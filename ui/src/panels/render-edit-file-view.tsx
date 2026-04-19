// Inline renderer for the diff panel's "Edit" view: when the user
// picks the edit-mode tab on a diff preview, this function decides
// what to render based on `latestFile.status` (idle / loading /
// ready / error) and the presence of `filePath`. Keeps the panel's
// per-viewMode switch terse.
//
// What this file owns:
//   - `renderEditFileView` — a function (not a component) that
//     returns the right piece of JSX for the edit view. Guards on
//     missing file path ("Edit mode" notice), idle / loading
//     ("Loading latest file..." shell), and error (surface the
//     stored error string); otherwise mounts a `<Suspense>`-
//     wrapped `<MonacoCodeEditor>` with a "Loading editor..."
//     fallback, wired to the caller's ref / status / save
//     handlers.
//   - Private `MonacoCodeEditor` lazy wrapper. Matching
//     `./DiffPanel.tsx`'s own lazy wrapper so the code-split
//     shape is unchanged — each module has its own lazy handle
//     but both resolve to the same `MonacoCodeEditor` export.
//
// What this file does NOT own:
//   - The diff panel's view-mode state, the editor refs, or the
//     save / rebase loop — all of that stays in `./DiffPanel.tsx`.
//     The panel passes the refs and callbacks through the
//     renderer's props.
//   - `LatestFileState` — lives in `./diff-latest-file-state`.
//   - Monaco types — live in `../MonacoCodeEditor`.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same markup, same
// fallback copy ("Loading latest file...", "Loading editor...",
// "This diff does not include a file path..."), same Suspense
// boundary placement.

import { Suspense, lazy } from "react";
import type {
  MonacoCodeEditorHandle,
  MonacoCodeEditorStatus,
} from "../MonacoCodeEditor";
import type { MonacoAppearance } from "../monaco";
import type { LatestFileState } from "./diff-latest-file-state";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);

export function renderEditFileView({
  appearance,
  editValue,
  editorRef,
  fontSizePx,
  filePath,
  language,
  latestFile,
  onChange,
  onSave,
  onStatusChange,
}: {
  appearance: MonacoAppearance;
  editValue: string;
  editorRef: { current: MonacoCodeEditorHandle | null };
  fontSizePx: number;
  filePath: string | null;
  language?: string | null;
  latestFile: LatestFileState;
  onChange: (value: string) => void;
  onSave: () => Promise<void>;
  onStatusChange: (status: MonacoCodeEditorStatus) => void;
}) {
  if (!filePath) {
    return (
      <article className="thread-notice">
        <div className="card-label">Edit mode</div>
        <p>This diff does not include a file path, so there is no file to edit.</p>
      </article>
    );
  }

  if (latestFile.status === "loading" || latestFile.status === "idle") {
    return <div className="source-editor-loading">Loading latest file...</div>;
  }

  if (latestFile.status === "error") {
    return (
      <article className="thread-notice">
        <div className="card-label">Edit mode</div>
        <p>{latestFile.error}</p>
      </article>
    );
  }

  return (
    <div className="source-editor-shell source-editor-shell-with-statusbar">
      <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
        <MonacoCodeEditor
          ref={editorRef}
          appearance={appearance}
          ariaLabel={`Edit mode for ${latestFile.path}`}
          fontSizePx={fontSizePx}
          language={latestFile.language ?? language ?? null}
          path={latestFile.path}
          value={editValue}
          onChange={onChange}
          onSave={() => void onSave()}
          onStatusChange={onStatusChange}
        />
      </Suspense>
    </div>
  );
}
