// The raw-patch diff view rendered when the user picks the "Raw"
// mode in the diff panel. Splits the patch string on `\n` and
// renders one row per line with a classed `.diff-preview-raw-line`
// variant so the added / removed / hunk / meta / context bands can
// be coloured via CSS.
//
// What this file owns:
//   - `RawPatchView` — the React component that lays out the raw
//     patch as a `role="table"` grid with a gutter column showing
//     1-based line numbers and the content column rendering the
//     raw patch text verbatim. Empty lines fall back to a single
//     space so the row keeps the same height.
//   - `rawDiffLineClassName` — the pure per-line classifier that
//     decides which `.diff-preview-raw-line-*` modifier applies:
//     `-hunk` for `@@ …`, `-meta` for git headers (`diff --git `,
//     `index `, `--- `, `+++ `), `-note` for the `"\\ No newline
//     at end of file"` marker, `-added` for `+…`, `-removed` for
//     `-…`, and `-context` as the default.
//
// What this file does NOT own:
//   - The "Raw" view mode selection itself — that lives with the
//     diff panel's view-mode state machine.
//   - The rendered-diff / markdown views — those stay in
//     `./DiffPanel.tsx` alongside the Monaco wiring.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same markup, same
// class names, same per-prefix ordering (hunk / meta / note /
// added / removed / context).

export function RawPatchView({
  diff,
  scrollRef,
}: {
  diff: string;
  scrollRef?: { current: HTMLDivElement | null };
}) {
  const lines = diff.split("\n");

  return (
    <div className="diff-editor-shell diff-preview-raw-shell" ref={scrollRef}>
      <div className="diff-preview-raw" role="table" aria-label="Raw patch preview">
        {lines.map((line, index) => (
          <div
            key={`${index}:${line}`}
            className={`diff-preview-raw-line ${rawDiffLineClassName(line)}`}
            role="row"
          >
            <span className="diff-preview-raw-line-number" aria-hidden="true">
              {index + 1}
            </span>
            <span className="diff-preview-raw-line-content" role="cell">
              {line || " "}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function rawDiffLineClassName(line: string) {
  if (line.startsWith("@@")) {
    return "diff-preview-raw-line-hunk";
  }

  if (
    line.startsWith("diff --git ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ")
  ) {
    return "diff-preview-raw-line-meta";
  }

  if (line === "\\ No newline at end of file") {
    return "diff-preview-raw-line-note";
  }

  if (line.startsWith("+") && !line.startsWith("+++ ")) {
    return "diff-preview-raw-line-added";
  }

  if (line.startsWith("-") && !line.startsWith("--- ")) {
    return "diff-preview-raw-line-removed";
  }

  return "diff-preview-raw-line-context";
}
