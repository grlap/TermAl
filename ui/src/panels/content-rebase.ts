// Pure helpers that rebase a local edit onto a disk version that
// changed under the editor's feet.
//
// What this file owns:
//   - `ContentRebaseResult` — discriminated `clean` / `conflict`
//     union the save path inspects to decide whether to write
//     the merged content or bubble a conflict notice to the
//     user.
//   - `MAX_REBASE_DIFF_CELLS` — LCS-table cell cap that keeps
//     the rebase O(N·M) but bounded. Large files fall back to
//     "conflict" rather than risk a slow merge.
//   - `LineDiffRange` type used internally by the merge loop.
//   - `rebaseContentOntoDisk` — top-level three-way merger. Runs
//     two independent LCS-based diffs against the common base,
//     detects conflicts (explicit overlap or dual insertions at
//     the same line), merges clean ranges into a single splice
//     list, and returns either `clean` + merged content or
//     `conflict` + a human-readable reason.
//   - `splitContentLines` — keeps trailing newlines attached to
//     each line so `join("")` round-trips to the original
//     string. Matches `[^\n]*\n|[^\n]+`.
//   - `diffLineRanges` — turns two arrays of lines + a shared
//     LCS anchor list into the minimal `LineDiffRange[]` set;
//     filters out no-op ranges.
//   - `buildLineLcsAnchors` — classic suffix LCS with a
//     `Uint32Array` backing table. Returns `null` if the table
//     would exceed `MAX_REBASE_DIFF_CELLS`.
//   - `lineDiffRangesConflict` — conflict predicate. Identical
//     ranges are not a conflict; two inserts at the same line
//     are; otherwise range overlap is the usual
//     `left.start < right.end && right.start < left.end`.
//   - `dedupeEquivalentRanges` — removes exact-duplicate ranges
//     before the merge (which can arise when a local edit and a
//     disk edit both made the same change).
//   - `lineArraysEqual` — shallow line-by-line string equality.
//
// What this file does NOT own:
//   - The save path itself, the file watcher, or the stale-on-
//     disk detection — those stay in `./SourcePanel.tsx` with
//     the save state machine.
//   - React / DOM. All helpers are pure.
//
// Split out of `ui/src/panels/SourcePanel.tsx`. Same constants,
// same function bodies, same `MAX_REBASE_DIFF_CELLS` cap;
// consumers (including `DiffPanel.tsx` and
// `SourcePanel.test.tsx`) import from here directly.

export type ContentRebaseResult =
  | { status: "clean"; content: string }
  | { status: "conflict"; reason: string };

export const MAX_REBASE_DIFF_CELLS = 4_000_000;

type LineDiffRange = {
  start: number;
  end: number;
  replacement: string[];
};

export function rebaseContentOntoDisk(
  baseContent: string,
  localContent: string,
  diskContent: string,
): ContentRebaseResult {
  // Be conservative here: false conflicts are recoverable, silent edit loss is not.
  const baseLines = splitContentLines(baseContent);
  const localLines = splitContentLines(localContent);
  const diskLines = splitContentLines(diskContent);
  const localRanges = diffLineRanges(baseLines, localLines);
  const diskRanges = diffLineRanges(baseLines, diskLines);

  if (!localRanges || !diskRanges) {
    return {
      status: "conflict",
      reason:
        "Could not apply edits automatically because the file is too large to merge safely.",
    };
  }

  for (const localRange of localRanges) {
    for (const diskRange of diskRanges) {
      if (!lineDiffRangesConflict(localRange, diskRange)) {
        continue;
      }

      return {
        status: "conflict",
        reason:
          "Could not apply your edits cleanly because they overlap with disk changes.",
      };
    }
  }

  const mergedRanges = dedupeEquivalentRanges([...diskRanges, ...localRanges]);
  mergedRanges.sort((left, right) => {
    const startOrder = left.start - right.start;
    if (startOrder !== 0) {
      return startOrder;
    }
    return left.end - right.end;
  });

  const mergedLines: string[] = [];
  let cursor = 0;
  for (const range of mergedRanges) {
    if (range.start < cursor) {
      return {
        status: "conflict",
        reason:
          "Could not apply your edits cleanly because the merged ranges overlap.",
      };
    }

    mergedLines.push(...baseLines.slice(cursor, range.start));
    mergedLines.push(...range.replacement);
    cursor = range.end;
  }
  mergedLines.push(...baseLines.slice(cursor));

  return {
    status: "clean",
    content: mergedLines.join(""),
  };
}

function splitContentLines(content: string) {
  return content.match(/[^\n]*\n|[^\n]+/g) ?? [];
}

function diffLineRanges(
  baseLines: string[],
  changedLines: string[],
): LineDiffRange[] | null {
  const anchors = buildLineLcsAnchors(baseLines, changedLines);
  if (!anchors) {
    return null;
  }

  const ranges: LineDiffRange[] = [];
  let baseCursor = 0;
  let changedCursor = 0;

  for (const anchor of anchors) {
    if (anchor.baseIndex > baseCursor || anchor.changedIndex > changedCursor) {
      ranges.push({
        start: baseCursor,
        end: anchor.baseIndex,
        replacement: changedLines.slice(changedCursor, anchor.changedIndex),
      });
    }
    baseCursor = anchor.baseIndex + 1;
    changedCursor = anchor.changedIndex + 1;
  }

  if (baseCursor < baseLines.length || changedCursor < changedLines.length) {
    ranges.push({
      start: baseCursor,
      end: baseLines.length,
      replacement: changedLines.slice(changedCursor),
    });
  }

  return ranges.filter(
    (range) => range.start !== range.end || range.replacement.length > 0,
  );
}

function buildLineLcsAnchors(baseLines: string[], changedLines: string[]) {
  const rows = baseLines.length + 1;
  const columns = changedLines.length + 1;
  if (rows * columns > MAX_REBASE_DIFF_CELLS) {
    return null;
  }

  const lengths = new Uint32Array(rows * columns);
  const indexFor = (row: number, column: number) => row * columns + column;

  for (let row = baseLines.length - 1; row >= 0; row -= 1) {
    for (let column = changedLines.length - 1; column >= 0; column -= 1) {
      lengths[indexFor(row, column)] =
        baseLines[row] === changedLines[column]
          ? lengths[indexFor(row + 1, column + 1)] + 1
          : Math.max(
              lengths[indexFor(row + 1, column)],
              lengths[indexFor(row, column + 1)],
            );
    }
  }

  const anchors: Array<{ baseIndex: number; changedIndex: number }> = [];
  let row = 0;
  let column = 0;
  while (row < baseLines.length && column < changedLines.length) {
    if (baseLines[row] === changedLines[column]) {
      anchors.push({ baseIndex: row, changedIndex: column });
      row += 1;
      column += 1;
    } else if (
      lengths[indexFor(row + 1, column)] >=
      lengths[indexFor(row, column + 1)]
    ) {
      row += 1;
    } else {
      column += 1;
    }
  }

  return anchors;
}

function lineDiffRangesConflict(left: LineDiffRange, right: LineDiffRange) {
  const leftMatchesRight =
    left.start === right.start &&
    left.end === right.end &&
    lineArraysEqual(left.replacement, right.replacement);
  if (leftMatchesRight) {
    return false;
  }

  const bothInsertAtSamePosition =
    left.start === left.end &&
    right.start === right.end &&
    left.start === right.start;
  if (bothInsertAtSamePosition) {
    return true;
  }

  return left.start < right.end && right.start < left.end;
}

function dedupeEquivalentRanges(ranges: LineDiffRange[]) {
  const deduped: LineDiffRange[] = [];
  for (const range of ranges) {
    if (
      deduped.some(
        (current) =>
          current.start === range.start &&
          current.end === range.end &&
          lineArraysEqual(current.replacement, range.replacement),
      )
    ) {
      continue;
    }
    deduped.push(range);
  }
  return deduped;
}

function lineArraysEqual(left: string[], right: string[]) {
  return (
    left.length === right.length &&
    left.every((line, index) => line === right[index])
  );
}
