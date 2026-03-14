import type { ReactNode } from "react";
import {
  type DiffPreviewCell,
  type DiffPreviewHighlight,
  type DiffPreviewHunk,
  type DiffPreviewModel,
  type DiffPreviewRow,
} from "../diff-preview";

export function StructuredDiffView({
  filePath,
  preview,
}: {
  filePath: string | null;
  preview: DiffPreviewModel;
}) {
  return (
    <div
      className="diff-editor-shell structured-diff-shell"
      data-testid="structured-diff-view"
    >
      <div className="structured-diff">
        <div className="structured-diff-column-headings" aria-hidden="true">
          <div className="structured-diff-column-heading">
            <span className="structured-diff-column-eyebrow">Original</span>
            <strong>{filePath ?? "Previous version"}</strong>
          </div>
          <div className="structured-diff-column-heading">
            <span className="structured-diff-column-eyebrow">Updated</span>
            <strong>{filePath ?? "Patched version"}</strong>
          </div>
        </div>

        <div className="structured-diff-body">
          {preview.hunks.map((hunk, index) => (
            <section
              key={`${index}:${hunk.header ?? "patch"}`}
              className="structured-diff-hunk"
            >
              <header className="structured-diff-hunk-header">
                <span className="structured-diff-hunk-badge">{`Hunk ${index + 1}`}</span>
                <code>{formatHunkLabel(hunk)}</code>
              </header>
              <div
                className="structured-diff-grid"
                role="table"
                aria-label={filePath ? `Diff preview for ${filePath}` : "Diff preview"}
              >
                {hunk.rows.map((row, rowIndex) => (
                  <StructuredDiffRow
                    key={`${index}:${rowIndex}:${row.kind}:${row.left.text}:${row.right.text}`}
                    row={row}
                  />
                ))}
              </div>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}

function StructuredDiffRow({ row }: { row: DiffPreviewRow }) {
  if (row.kind === "omitted") {
    return (
      <div className="structured-diff-row structured-diff-row-omitted" role="row">
        <div className="structured-diff-omitted" role="cell">
          Unchanged lines omitted
        </div>
      </div>
    );
  }

  return (
    <div className={`structured-diff-row structured-diff-row-${row.kind}`} role="row">
      <DiffCell cell={row.left} side="left" kind={row.kind} />
      <DiffCell cell={row.right} side="right" kind={row.kind} />
    </div>
  );
}

function DiffCell({
  cell,
  kind,
  side,
}: {
  cell: DiffPreviewCell;
  kind: DiffPreviewRow["kind"];
  side: "left" | "right";
}) {
  const sign = diffCellSign(kind, side);
  const isEmpty = cell.lineNumber === null && cell.text.length === 0;

  return (
    <div
      className={`structured-diff-cell structured-diff-cell-${side} ${
        isEmpty ? "structured-diff-cell-empty" : ""
      }`}
      role="cell"
    >
      <span className="structured-diff-line-number" aria-hidden="true">
        {cell.lineNumber ?? ""}
      </span>
      <span className="structured-diff-line-sign" aria-hidden="true">
        {sign}
      </span>
      <span className="structured-diff-line-text">
        {renderDiffText(cell.text, cell.highlights)}
      </span>
    </div>
  );
}

function renderDiffText(text: string, highlights: DiffPreviewHighlight[]) {
  if (text.length === 0) {
    return <span className="structured-diff-line-placeholder">&nbsp;</span>;
  }

  if (highlights.length === 0) {
    return text;
  }

  const segments: ReactNode[] = [];
  let cursor = 0;

  for (const [index, range] of highlights.entries()) {
    if (range.start > cursor) {
      segments.push(
        <span key={`plain:${index}:${cursor}`}>{text.slice(cursor, range.start)}</span>,
      );
    }
    segments.push(
      <mark key={`mark:${index}:${range.start}`} className="structured-diff-inline-change">
        {text.slice(range.start, range.end)}
      </mark>,
    );
    cursor = range.end;
  }

  if (cursor < text.length) {
    segments.push(<span key={`tail:${cursor}`}>{text.slice(cursor)}</span>);
  }

  return segments;
}

function diffCellSign(kind: DiffPreviewRow["kind"], side: "left" | "right") {
  if (kind === "added") {
    return side === "right" ? "+" : "";
  }
  if (kind === "removed") {
    return side === "left" ? "-" : "";
  }
  if (kind === "changed") {
    return side === "left" ? "-" : "+";
  }
  return "";
}

function formatHunkLabel(hunk: DiffPreviewHunk) {
  if (hunk.header) {
    return hunk.header;
  }

  const oldRange = formatHunkRange(hunk.oldStart, hunk.oldCount);
  const newRange = formatHunkRange(hunk.newStart, hunk.newCount);
  if (!oldRange && !newRange) {
    return "Patch changes";
  }

  return `${oldRange ?? "-"} -> ${newRange ?? "+"}`;
}

function formatHunkRange(start: number | null, count: number | null) {
  if (start === null) {
    return null;
  }
  if (count === null) {
    return `${start}`;
  }
  return `${start},${count}`;
}
