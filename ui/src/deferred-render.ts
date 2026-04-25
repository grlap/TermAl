// Pure helpers that support the deferred-rendering wrappers in
// `./message-cards` (the heavy-code / heavy-markdown cards that
// hold off on full-cost rendering until they scroll into the
// viewport).
//
// What this file owns:
//   - `DEFERRED_RENDER_ROOT_MARGIN_PX` — the vertical root margin
//     used by both the `IntersectionObserver` and the eager
//     `isElementNearRenderViewport` measurement that activate a
//     deferred block when its viewport is nearby. Tuned to match
//     the session scroll container's cold-cache overscan so the
//     first screen of visible content renders without waiting on
//     the observer.
//   - `resolveDeferredRenderRoot` — walks the `.message-stack`
//     ancestor used as the IntersectionObserver root (falls back
//     to the window viewport when the node is outside a message
//     stack, e.g. a standalone Markdown document view).
//   - `isElementNearRenderViewport` — cheap rect-based fallback
//     used on layout-time to activate an element that the
//     IntersectionObserver would otherwise only visit after the
//     next tick.
//   - `measureTextBlock` — counts lines for the heavy-threshold
//     checks (empty strings count as one line, matching what
//     every rendering pipeline displays).
//   - `estimateCodeBlockHeight`, `estimateMarkdownBlockHeight` —
//     turn a line count into a CSS `min-height` for the
//     placeholder block so the scroll container does not jump
//     when the real content activates. Capped at
//     `MAX_DEFERRED_PLACEHOLDER_HEIGHT` so a huge block cannot
//     shove the rest of the pane off-screen.
//   - `buildDeferredPreviewText` — first-N-lines / first-N-chars
//     placeholder text, with a `\u2026` ellipsis when truncated.
//   - `buildMarkdownPreviewText` — Markdown-aware version of
//     `buildDeferredPreviewText`: strips code fences, link
//     targets, heading prefixes, blockquote markers, list
//     bullets, and backticks before handing to the generic
//     truncator.
//
// What this file does NOT own:
//   - `DeferredHeavyContent`, `DeferredHighlightedCodeBlock`,
//     `DeferredMarkdownContent` — those are React components that
//     close over the rendered children (`<HighlightedCodeBlock>`
//     / `<MarkdownContent>`) so they stay with their peers in
//     `./message-cards.tsx`.
//   - The HEAVY_* thresholds that gate whether content is
//     deferred at all — those stay inline at
//     `./message-cards.tsx` because they are tuning knobs co-
//     located with the components that consume them.
//   - `DEFERRED_PREVIEW_LINE_LIMIT`,
//     `DEFERRED_PREVIEW_CHARACTER_LIMIT`,
//     `MAX_DEFERRED_PLACEHOLDER_HEIGHT` — the shared budget
//     constants live in `./app-utils` with the rest of the UI
//     limits and pretty-printers; this module imports them.
//
// Split out of `ui/src/message-cards.tsx`. Same function bodies,
// same constant value.

import {
  DEFERRED_PREVIEW_CHARACTER_LIMIT,
  DEFERRED_PREVIEW_LINE_LIMIT,
  MAX_DEFERRED_PLACEHOLDER_HEIGHT,
} from "./app-utils";

export const DEFERRED_RENDER_ROOT_MARGIN_PX = 960;
export const DEFERRED_RENDER_RESUME_EVENT = "termal:deferred-render-resume";
export const DEFERRED_RENDER_SUSPENDED_ATTRIBUTE =
  "data-deferred-render-suspended";

export function resolveDeferredRenderRoot(node: Element) {
  const root = node.closest(".message-stack");
  return root instanceof Element ? root : null;
}

export function isDeferredRenderActivationSuspended(root: Element | null) {
  return (
    root instanceof Element &&
    root.getAttribute(DEFERRED_RENDER_SUSPENDED_ATTRIBUTE) === "true"
  );
}

export function isElementNearRenderViewport(
  node: Element,
  root: Element | null,
  marginPx: number,
) {
  const nodeRect = node.getBoundingClientRect();
  const rootRect = root?.getBoundingClientRect() ?? {
    top: 0,
    bottom: window.innerHeight,
  };

  return nodeRect.bottom >= rootRect.top - marginPx && nodeRect.top <= rootRect.bottom + marginPx;
}

export function measureTextBlock(text: string) {
  return {
    lineCount: text.length === 0 ? 1 : text.split("\n").length,
  };
}

export function estimateCodeBlockHeight(lineCount: number) {
  return Math.min(MAX_DEFERRED_PLACEHOLDER_HEIGHT, Math.max(120, lineCount * 20 + 48));
}

export function estimateMarkdownBlockHeight(lineCount: number) {
  return Math.min(MAX_DEFERRED_PLACEHOLDER_HEIGHT, Math.max(140, lineCount * 28 + 56));
}

export function buildDeferredPreviewText(text: string) {
  const preview = text
    .split("\n")
    .slice(0, DEFERRED_PREVIEW_LINE_LIMIT)
    .join("\n")
    .slice(0, DEFERRED_PREVIEW_CHARACTER_LIMIT)
    .trimEnd();

  return preview.length < text.length ? `${preview}\n\u2026` : preview;
}

export function buildMarkdownPreviewText(markdown: string) {
  const preview = markdown
    .replace(/```[\s\S]*?```/g, "[code block]")
    .replace(/\[(.*?)\]\((.*?)\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^>\s?/gm, "")
    .replace(/^[*-]\s+/gm, "")
    .replace(/`/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();

  return buildDeferredPreviewText(preview);
}
