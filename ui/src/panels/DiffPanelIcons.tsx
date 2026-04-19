// Inline SVG icons used by the diff panel toolbar and navigation.
//
// What this file owns:
//   - `AllLinesIcon`, `ChangedOnlyIcon` — the full / changed-only
//     toolbar toggle between rendering the whole file and only
//     the hunks with changes.
//   - `MarkdownModeIcon`, `EditModeIcon`, `RawPatchIcon` — the
//     view-mode toolbar (rendered Markdown, edit file, raw patch).
//   - `CopyIcon`, `CheckIcon` — the copy-to-clipboard toolbar
//     button states (idle vs just-copied).
//   - `DiffNavArrow` — the direction-parametrized up/down arrow
//     used by the next/previous-change navigation chips.
//
// Every icon is a plain stateless SVG with
// `aria-hidden="true" focusable="false"`. `DiffNavArrow` is the
// only one that accepts a prop; the rest are zero-arg.
//
// What this file does NOT own:
//   - The buttons that wrap these icons — those live in
//     `./DiffPanel.tsx` alongside the view-mode state and the
//     clipboard logic.
//   - The per-view layouts (`RawPatchView`, `RenderedDiffView`,
//     `MarkdownDiffView`) — also in `./DiffPanel.tsx`.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same SVG markup,
// same ARIA wiring, same paths. Consumers import from here
// directly.

export function AllLinesIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M2.25 3h11.5v1.75H2.25Zm0 4.13h11.5v1.75H2.25Zm0 4.12h11.5V13H2.25Z" fill="currentColor" />
    </svg>
  );
}

export function ChangedOnlyIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M2.25 3.25H6.5V5H2.25Zm7.25 0h4.25V5H9.5ZM2.25 11h4.25v1.75H2.25ZM9.5 11h4.25v1.75H9.5Z" fill="currentColor" />
      <path d="m6.45 8 1.6-1.6 1.48 1.48L8 9.41 6.47 7.88Z" fill="currentColor" />
    </svg>
  );
}

export function MarkdownModeIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M2.5 2.25h11A1.5 1.5 0 0 1 15 3.75v8.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 1 12.25v-8.5a1.5 1.5 0 0 1 1.5-1.5Zm0 1.5v8.5h11v-8.5Zm1.15 6.8V5.45h1.52l1.18 1.72 1.18-1.72h1.52v5.1H7.72V7.42L6.35 9.35 4.98 7.42v3.13Zm7.65 0L9.35 8.6h1.2V5.45h1.5V8.6h1.2Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function EditModeIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="m11.77 1.88 2.35 2.35-7.4 7.4-3 .65.65-3Zm-6.28 7.87 4.83-4.83-.75-.75-4.83 4.83-.3 1.35Zm6-6 .76.75.88-.88-.76-.75Z" fill="currentColor" />
    </svg>
  );
}

export function RawPatchIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M4.9 2 2 8l2.9 6h1.72L3.72 8l2.9-6Zm6.2 0L14 8l-2.9 6H9.38l2.9-6-2.9-6Z" fill="currentColor" />
    </svg>
  );
}

export function CopyIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
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
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M13.35 4.65a.5.5 0 0 1 0 .7l-6 6a.5.5 0 0 1-.7 0l-3-3a.5.5 0 1 1 .7-.7L7 10.29l5.65-5.64a.5.5 0 0 1 .7 0Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function DiffNavArrow({ direction }: { direction: "up" | "down" }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d={direction === "up" ? "M8 3.5 13 8.5l-1.15 1.15L8.8 6.61V13h-1.6V6.61L4.15 9.65 3 8.5l5-5Z" : "M8 12.5 3 7.5l1.15-1.15L7.2 9.39V3h1.6v6.39l3.05-3.04L13 7.5l-5 5Z"}
        fill="currentColor"
      />
    </svg>
  );
}
