// Owns Markdown source-position to rendered-line metadata helpers.
// Does not own Markdown rendering, streaming splits, or DOM measurement.
// Split from ui/src/message-cards.tsx.

export type MarkdownSourcePosition = {
  start?: {
    line?: number | null;
  } | null;
  end?: {
    line?: number | null;
  } | null;
} | null;

export type MarkdownLineAttributes = {
  "data-markdown-line-start": number;
  "data-markdown-line-range": string;
  title: string;
};

export type MarkdownLineMarker = {
  line: number;
  range: string;
  top: number;
};

export function normalizeMarkdownStartLineNumber(
  startLineNumber: number | null,
) {
  if (
    startLineNumber == null ||
    !Number.isFinite(startLineNumber) ||
    startLineNumber < 1
  ) {
    return null;
  }

  return Math.floor(startLineNumber);
}

export function getMarkdownLineAttributes(
  sourcePosition: MarkdownSourcePosition | undefined,
  startLineNumber: number | null,
  showLineNumbers: boolean,
): MarkdownLineAttributes | null {
  if (!showLineNumbers || startLineNumber == null) {
    return null;
  }

  const lineNumber = getMarkdownRenderedLineNumber(
    sourcePosition,
    startLineNumber,
  );
  if (lineNumber == null) {
    return null;
  }

  const rangeLabel = getMarkdownRenderedLineRangeLabel(
    sourcePosition,
    startLineNumber,
  );
  const title =
    rangeLabel === String(lineNumber)
      ? `Line ${lineNumber}`
      : `Lines ${rangeLabel}`;

  return {
    "data-markdown-line-start": lineNumber,
    "data-markdown-line-range": rangeLabel,
    title,
  };
}

function getMarkdownRenderedLineNumber(
  sourcePosition: MarkdownSourcePosition | undefined,
  startLineNumber: number,
) {
  const sourceLine = sourcePosition?.start?.line;
  if (
    typeof sourceLine !== "number" ||
    !Number.isFinite(sourceLine) ||
    sourceLine < 1
  ) {
    return null;
  }

  return startLineNumber + sourceLine - 1;
}

function getMarkdownRenderedLineRangeLabel(
  sourcePosition: MarkdownSourcePosition | undefined,
  startLineNumber: number,
) {
  const start = getMarkdownRenderedLineNumber(sourcePosition, startLineNumber);
  if (start == null) {
    return "";
  }

  const endLine = sourcePosition?.end?.line;
  if (typeof endLine !== "number" || !Number.isFinite(endLine) || endLine < 1) {
    return String(start);
  }

  const end = startLineNumber + endLine - 1;
  return end > start ? `${start}-${end}` : String(start);
}

export function areMarkdownLineMarkersEqual(
  currentMarkers: MarkdownLineMarker[],
  nextMarkers: MarkdownLineMarker[],
) {
  if (currentMarkers.length !== nextMarkers.length) {
    return false;
  }

  return currentMarkers.every((marker, index) => {
    const nextMarker = nextMarkers[index];
    return (
      marker.line === nextMarker?.line &&
      marker.range === nextMarker.range &&
      marker.top === nextMarker.top
    );
  });
}
