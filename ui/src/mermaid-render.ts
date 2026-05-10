// Mermaid rendering configuration, theme CSS, and the render-
// queue wrapper used by the `MermaidDiagram` component.
//
// What this file owns:
//   - `buildMermaidDiagramFrameSrcDoc` — builds the iframe srcdoc
//     HTML wrapping a Mermaid SVG, including the inline CSS that
//     zeroes body descenders / scroll-root overflow so the iframe
//     can be sized off the SVG viewBox.
//   - `getMermaidDiagramFrameStyle` — reads the SVG's viewBox and
//     returns constrained `{ aspectRatio, height: "auto", width,
//     maxWidth }` CSS for the iframe. Wide diagrams keep their
//     intrinsic scrollable width while the used height scales down
//     when `max-width: 100%` constrains the frame.
//   - `clampMermaidDiagramExtent`, `readMermaidSvgDimensions` —
//     the clamp math and the viewBox parser that back the frame-
//     style helper.
//   - `MERMAID_DIAGRAM_FRAME_MAX_WIDTH`,
//     `MERMAID_DIAGRAM_FRAME_MAX_HEIGHT` — the layout-DoS caps.
//   - `TERMAL_MERMAID_FLOWCHART_CONFIG` — our flowchart tuning
//     (tight node/rank spacing + wrappingWidth).
//   - `TERMAL_MERMAID_THEME_CSS` — the inline `themeCSS` that
//     styles edge labels, foreignObject wrappers, etc.
//   - `TERMAL_MERMAID_THEME_VARIABLES` — the base
//     `themeVariables` overrides (currently the 11px fontSize).
//   - `TERMAL_MERMAID_THEME_VARIABLES_BY_MARKDOWN_THEME` — the
//     per-Markdown-theme Mermaid palette overrides, keyed on the
//     Markdown theme id read from `document.documentElement.dataset.markdownTheme`.
//   - `TERMAL_MERMAID_BASE_CONFIG` — the Mermaid `initialize`
//     config object we reset to between renders.
//   - `readActiveMarkdownThemeId`, `buildMarkdownThemeVariables`,
//     `readActiveDiagramLook`, `readActiveDiagramPalette` — tiny
//     DOM readers that pull the active user preferences from
//     `<html>` dataset attributes.
//   - `buildTermalMermaidConfig` — composes the final per-render
//     Mermaid config from the Monaco appearance (dark/light), the
//     user's palette preference, and the active Markdown theme's
//     overrides.
//   - `renderTermalMermaidDiagram` — serialized render wrapper
//     that drains a module-level promise queue so concurrent
//     Mermaid renders do not leak config into each other via
//     Mermaid's singleton `initialize`. Strips author
//     `%%{init: ...}%%` directives + YAML theme keys through
//     `applyActiveMermaidThemeOverride` so the reader's theme
//     wins by default.
//   - `MermaidModule`, `MermaidConfigInput` type aliases over
//     `import("mermaid")` shapes for cleaner call sites.
//
// What this file does NOT own:
//   - The `MermaidDiagram` React component itself (stays in
//     `./message-cards.tsx` because it owns the render lifecycle,
//     error boundaries, and KaTeX peers).
//   - The `applyActiveMermaidThemeOverride` source rewriter
//     (lives in `./mermaid-theme-override`).
//   - Diagram look / palette sentinel values and type guards
//     (live in `./themes`).
//
// Split out of `ui/src/message-cards.tsx`. Same constants, same
// function bodies, same serialized queue semantics; consumers
// import from here directly.

import { applyActiveMermaidThemeOverride } from "./mermaid-theme-override";
import type { MonacoAppearance } from "./monaco";
import {
  DEFAULT_DIAGRAM_LOOK,
  DEFAULT_DIAGRAM_PALETTE,
  DIAGRAM_HAND_DRAWN_SEED,
  isDiagramLook,
  isDiagramPalette,
  type DiagramLook,
  type DiagramPalette,
} from "./themes";

import type { CSSProperties } from "react";

export type MermaidModule = (typeof import("mermaid"))["default"];
export type MermaidConfigInput = NonNullable<
  Parameters<MermaidModule["initialize"]>[0]
>;

export function buildMermaidDiagramFrameSrcDoc(
  svg: string,
  options: { fitToFrame?: boolean } = {},
) {
  const scrollRootCss = options.fitToFrame
    ? "html{overflow:hidden;}"
    : "html{overflow-x:auto;overflow-y:hidden;}";
  const bodyCss = options.fitToFrame
    ? "body{display:block;width:100%;min-width:0;font-size:0;line-height:0;}"
    : "body{display:inline-block;min-width:100%;font-size:0;line-height:0;}";
  const svgCss = options.fitToFrame
    ? "svg{display:block;max-width:100%;height:auto;margin:0 auto;vertical-align:top;}"
    : "svg{display:block;max-width:none;height:auto;margin:0 auto;vertical-align:top;}";
  return [
    "<!doctype html>",
    '<html><head><meta charset="utf-8">',
    "<style>",
    // Clip vertical overflow on the iframe's scroll root. The iframe
    // is sized from the SVG's viewBox but the actual rendered height
    // can differ by a couple of pixels when Mermaid's
    // `htmlLabels: true` foreignObject text is re-measured against
    // the iframe's own font metrics (Mermaid's internal pass uses a
    // temp-DOM with subtly different rendering). Hiding the vertical
    // axis keeps the frame tight against the diagram and avoids a
    // spurious scrollbar on every render. Default frames keep
    // `overflow-x: auto` so diagrams wider than the iframe can still
    // scroll horizontally inside the frame; preview fit mode scales
    // the SVG to the iframe instead.
    scrollRootCss,
    "html,body{margin:0;padding:0;background:transparent;color:inherit;}",
    // The default body can shrink-wrap wide SVGs for horizontal
    // scrolling. Fit mode uses a block body and max-width:100% SVG so
    // wide preview diagrams shrink to the pane without upscaling
    // simpler diagrams past their natural Mermaid size. In both
    // modes, zero the font + line-height + vertical-align so the
    // body's outer height matches the SVG exactly.
    bodyCss,
    svgCss,
    TERMAL_MERMAID_THEME_CSS,
    "</style></head><body>",
    svg,
    "</body></html>",
  ].join("");
}

// Upper caps protect the layout from a pathologically large Mermaid
// `viewBox` (agent output or hostile Markdown). The iframe is sandboxed,
// so this is a layout-DoS guard rather than an XSS guard. The values are
// generous enough for real flowcharts but bounded so the iframe cannot
// overflow its parent column by thousands of pixels.
export const MERMAID_DIAGRAM_FRAME_MAX_WIDTH = 4096;
export const MERMAID_DIAGRAM_FRAME_MAX_HEIGHT = 4096;

export function getMermaidDiagramFrameStyle(
  svg: string,
  options: { fitToFrame?: boolean } = {},
): CSSProperties {
  const dimensions = readMermaidSvgDimensions(svg);
  if (!dimensions) {
    return {};
  }

  const heightSlack = options.fitToFrame ? 2 : 24;
  const frameHeight = clampMermaidDiagramExtent(
    Math.ceil(dimensions.height) + heightSlack,
    60,
    MERMAID_DIAGRAM_FRAME_MAX_HEIGHT,
  );
  const frameWidth = clampMermaidDiagramExtent(
    Math.ceil(dimensions.width) + 2,
    180,
    MERMAID_DIAGRAM_FRAME_MAX_WIDTH,
  );

  return {
    // `max-width: 100%` can shrink a wide iframe below `frameWidth`.
    // If height stays fixed, the SVG scales down horizontally while
    // the iframe keeps the old unscaled height, leaving a large blank
    // area below wide ER diagrams. Use CSS aspect-ratio so the used
    // height scales with the constrained width.
    //
    // Trade-off: very tall, narrow diagrams in narrow columns can be
    // clipped at the bottom because the srcdoc intentionally keeps
    // vertical overflow hidden. That keeps wide diagrams tight instead
    // of restoring the old fixed-height blank-frame behavior.
    //
    // Default frames include 24px vertical slack for horizontal
    // scrollbar chrome / Mermaid temp-DOM text-measurement drift. Fit
    // frames do not scroll horizontally, so they keep only a small
    // 2px vertical pad matching the width pad to avoid hidden-overflow
    // clipping without restoring the old blank band.
    aspectRatio: `${frameWidth} / ${frameHeight}`,
    height: "auto",
    width: `${frameWidth}px`,
    maxWidth: "100%",
  };
}

export function clampMermaidDiagramExtent(value: number, lowerBound: number, upperBound: number) {
  return Math.min(Math.max(lowerBound, value), upperBound);
}

export function readMermaidSvgDimensions(svg: string) {
  const viewBoxMatch = svg.match(
    /\bviewBox=["']\s*[-+]?\d*\.?\d+\s+[-+]?\d*\.?\d+\s+([-+]?\d*\.?\d+)\s+([-+]?\d*\.?\d+)\s*["']/i,
  );
  if (!viewBoxMatch) {
    return null;
  }

  const width = Number(viewBoxMatch[1]);
  const height = Number(viewBoxMatch[2]);
  return Number.isFinite(width) && Number.isFinite(height)
    ? { height, width }
    : null;
}

export const TERMAL_MERMAID_FLOWCHART_CONFIG = {
  defaultRenderer: "dagre-wrapper",
  diagramPadding: 1,
  // Tight-by-default spacing. Mermaid's own defaults (50 / 50) leave
  // huge whitespace around short flowcharts, and earlier TermAl
  // values (24 / 30) were still larger than most diagrams need. The
  // numbers below are tuned for agent-generated and doc-comment
  // flowcharts, which tend to be short and wide. A second pass
  // tightened them again after the first round was still leaving
  // extra whitespace around short diagrams on wider viewports.
  nodeSpacing: 12,
  padding: 3,
  rankSpacing: 18,
  useMaxWidth: false,
  // Narrower wrap so long node labels (e.g. "Contains Mermaid fence?")
  // produce shorter-but-taller boxes instead of a single wide line
  // that forces the diagram's total width up.
  wrappingWidth: 90,
} as const;

export const TERMAL_MERMAID_THEME_CSS = `
.nodeLabel,
.edgeLabel,
.label {
  line-height: 1.2;
}
/* Edge-label pill shape. Mermaid renders the actual background as
   either a child rect or a labelBkg div (varies by renderer
   + html-labels setting), sized to fit the text. Padding on the
   outer edgeLabel pushes text AWAY from that background and
   produces a weird narrow-tall ghost shape. Apply the border-
   radius to all Mermaid-generated inner wrappers AND to the SVG
   rect so whichever element carries the background gets the rounded
   corners. No padding — Mermaid's built-in label sizing is already
   generous enough at 12px. */
.edgeLabel,
.edgeLabel .label,
.edgeLabel span,
.edgeLabel p,
.edgeLabel div,
.edgeLabel .labelBkg,
.edgeLabel rect {
  border-radius: 12px;
  rx: 6;
  ry: 6;
}
/* Visible outline on the background-carrying element. Mermaid's
   default edge-label stroke is very faint (or absent) — calling out
   the pill shape with a 1.5px border helps the yes/no labels read
   as decisions rather than stray text. Applied to both the
   html-labels labelBkg div and the SVG rect fallback.

   Scoping detail: Mermaid emits the full
   .edgeLabel > foreignObject > div.labelBkg > span structure for
   every edge regardless of whether it has a label, and
   positionEdgeLabel only sets a translate transform when
   edge.label is truthy — unlabeled edges stack at SVG origin and
   would render as a ghost pill near the top-left. The inner span
   is the discriminator: labeled edges get span innerHTML set to
   the label text (so the span has a text-node child and is NOT
   :empty); unlabeled edges get span.html("") which leaves a truly
   empty <span></span>. :has(foreignObject span:not(:empty)) keeps
   the outline on real labels only. */
.edgeLabel:has(foreignObject span:not(:empty)) .labelBkg {
  border: 1.5px solid currentColor;
  padding: 2px 4px;
}
.edgeLabel:has(foreignObject span:not(:empty)) rect {
  stroke: currentColor;
  stroke-width: 1.5px;
}
.nodeLabel p,
.edgeLabel p,
.label p,
.nodeLabel div,
.edgeLabel div,
.label div {
  margin: 0;
  line-height: 1.2;
}
.nodeLabel p,
.nodeLabel div,
.label p,
.label div {
  padding: 0;
}
foreignObject {
  overflow: visible;
}
`;

// Mermaid's defaults render labels in 16px. For in-document
// flowcharts that sit inside doc comments, inline Monaco zones, or
// messages, that's too big — every node becomes a small poster.
// 12px was the first shrink; 11px is the second pass for even
// tighter rendering when a flowchart shares the pane with source
// text and the diagram does not need to be read from across the
// room. Mermaid sizes boxes to fit text, so shrinking the font
// scales node dimensions proportionally.
export const TERMAL_MERMAID_THEME_VARIABLES = {
  fontSize: "11px",
} as const;

// Per-Markdown-theme Mermaid palette overrides. The active Markdown
// theme id is read from `document.documentElement.dataset.markdownTheme`
// at render time (applied by `applyMarkdownThemePreference` in
// `ui/src/themes.ts`). The lookup is a plain object keyed on the
// Markdown theme id so adding a new preset in the registry only
// requires a new entry here — no plumbing through React props.
//
// `match-ui` passes through: no overrides, so Mermaid keeps the
// (Monaco-appearance-derived) default/dark theme plus our font-size
// override. Other entries shift the core palette variables so
// flowcharts match the prose theme instead of the workspace chrome.
export const TERMAL_MERMAID_THEME_VARIABLES_BY_MARKDOWN_THEME: Record<
  string,
  Readonly<Record<string, string>>
> = {
  "match-ui": {},
  "github-light": {
    background: "#ffffff",
    primaryColor: "#dbe8f7",
    primaryTextColor: "#24292f",
    primaryBorderColor: "#0969da",
    secondaryColor: "#f6f8fa",
    lineColor: "#57606a",
    tertiaryColor: "#f6f8fa",
  },
  "github-dark": {
    background: "#0d1117",
    primaryColor: "#1f2933",
    primaryTextColor: "#c9d1d9",
    primaryBorderColor: "#58a6ff",
    secondaryColor: "#161b22",
    lineColor: "#8b949e",
    tertiaryColor: "#161b22",
  },
  terminal: {
    background: "#0a120d",
    primaryColor: "#112618",
    primaryTextColor: "#c8e3cc",
    primaryBorderColor: "#5ccf86",
    secondaryColor: "#0e1a12",
    lineColor: "#7fa48a",
    tertiaryColor: "#0e1a12",
  },
};

export function readActiveMarkdownThemeId(): string {
  if (typeof document === "undefined") {
    return "match-ui";
  }
  return document.documentElement.dataset.markdownTheme ?? "match-ui";
}

export function buildMarkdownThemeVariables(): Readonly<Record<string, string>> {
  const id = readActiveMarkdownThemeId();
  return TERMAL_MERMAID_THEME_VARIABLES_BY_MARKDOWN_THEME[id] ?? {};
}

export const TERMAL_MERMAID_BASE_CONFIG = {
  flowchart: {
    ...TERMAL_MERMAID_FLOWCHART_CONFIG,
  },
  htmlLabels: true,
  securityLevel: "strict",
  startOnLoad: false,
  themeCSS: TERMAL_MERMAID_THEME_CSS,
  themeVariables: {
    ...TERMAL_MERMAID_THEME_VARIABLES,
  },
} satisfies MermaidConfigInput;

let mermaidRenderQueue: Promise<unknown> = Promise.resolve();

export function renderTermalMermaidDiagram(
  mermaid: MermaidModule,
  diagramId: string,
  code: string,
  appearance: MonacoAppearance,
) {
  const config = buildTermalMermaidConfig(appearance);
  // Strip author `%%{init: ...}%%` directives and YAML frontmatter
  // theme keys when the user has diagram-theme Override mode enabled
  // (the default). Applied on the string we hand to mermaid.render,
  // not the stored source — so the reader's Markdown theme wins
  // without rewriting anyone's diagram file. Respect mode (user
  // toggled the Settings preference off) leaves the source alone.
  const renderSource = applyActiveMermaidThemeOverride(code);
  const renderJob = mermaidRenderQueue.then(async () => {
    // Mermaid keeps config in a module-level singleton. Serialize
    // initialize/render/reset so light and dark diagrams do not leak config
    // into each other or into future Mermaid consumers in this tab.
    mermaid.initialize(config);
    try {
      const result = await mermaid.render(diagramId, renderSource);
      // Mermaid 11.x does not always THROW on syntax errors — for
      // some malformed diagrams it returns a "syntax error" SVG
      // containing the red bomb icon plus "Syntax error in text"
      // and "mermaid version X.Y.Z" footer text. Sandboxed into our
      // iframe that visualization renders as a jarring red bomb in
      // the middle of the document, bypassing `MermaidDiagram`'s
      // own clean error fallback (which shows a one-line message
      // and the diagram source highlighted as code). Detect the
      // error SVG and throw so the caller routes through that
      // fallback path instead.
      if (isMermaidErrorVisualizationSvg(result.svg)) {
        throw new Error("Mermaid syntax error");
      }
      return result;
    } finally {
      mermaid.initialize(TERMAL_MERMAID_BASE_CONFIG);
      // Mermaid 11.x renders into a temp container parented in
      // `document.body` (id `d${diagramId}` containing an `<svg>`
      // whose id is `${diagramId}`). On throw paths — including
      // every parse error, draw error, or `isMermaidErrorVisualizationSvg`
      // re-throw above — the container survives in the page DOM
      // with an embedded bomb-icon error visualization. Without
      // explicit cleanup, the user sees a jarring red bomb floating
      // outside our `MermaidDiagram` iframe even though the
      // component-level error fallback rendered correctly. Remove
      // the temp container here so it cannot leak past the catch
      // boundary. Idempotent: `getElementById` returns null when
      // Mermaid has already cleaned up after a successful render,
      // and `removeChild` is gated on `parentElement`.
      cleanupMermaidTempContainer(diagramId);
    }
  });
  mermaidRenderQueue = renderJob.then(
    () => undefined,
    () => undefined,
  );
  return renderJob;
}

/**
 * Returns `true` when `svg` is the "syntax error" visualization
 * Mermaid 11.x produces in place of a real diagram render. Mermaid
 * does not always throw on parse failures — for several malformed-
 * diagram shapes (especially flowchart/sequenceDiagram label/edge
 * grammar errors) it instead resolves the `render()` promise with
 * an SVG containing the red bomb icon and a "Syntax error in text"
 * message. That SVG must not be piped into the diagram iframe;
 * `renderTermalMermaidDiagram` re-throws when this returns `true`
 * so the caller's existing error-state branch can show the diagram
 * source + a clean one-line message instead.
 *
 * Detection is intentionally cheap and defensive. Root-level
 * `aria-roledescription="error"` is enough by itself; otherwise require
 * multiple weaker Mermaid-error markers so a valid diagram containing
 * "Syntax error in text" as a label does not false-positive.
 *
 *   - `aria-roledescription="error"` on the root `<svg>` element
 *     (the strongest signal in 11.x).
 *   - The literal text "Syntax error in text" together with
 *     `class="error-icon"` (the bomb-icon group).
 */
/**
 * Removes Mermaid 11.x's temp render container from `document.body`.
 * The container is created lazily by `mermaid.render(diagramId, ...)`
 * with id `d${diagramId}` (a `<div>` wrapping the `<svg id={diagramId}>`
 * Mermaid was building). On every throw path the container survives,
 * with an embedded bomb-icon error visualization that renders to the
 * page outside our intended `MermaidDiagram` iframe. Calling this in
 * a `finally` block guarantees we do not leak the artifact regardless
 * of which error mode Mermaid hit (parser, draw, post-detection
 * re-throw, etc.). Idempotent — when Mermaid cleans up itself on the
 * happy path the lookups simply miss.
 */
function cleanupMermaidTempContainer(diagramId: string): void {
  if (typeof document === "undefined") {
    return;
  }
  const wrapper = document.getElementById(`d${diagramId}`);
  if (wrapper?.parentElement) {
    wrapper.parentElement.removeChild(wrapper);
  }
  // Defensive: if Mermaid ever skips the wrapping `<div>` (e.g.,
  // certain renderer paths inject the SVG directly), prune the SVG
  // by id too.
  const orphanSvg = document.getElementById(diagramId);
  if (orphanSvg?.parentElement) {
    orphanSvg.parentElement.removeChild(orphanSvg);
  }
}

export function isMermaidErrorVisualizationSvg(svg: string): boolean {
  const hasRootErrorRole =
    /<svg\b[^>]*\baria-roledescription=["']error["'][^>]*>/i.test(svg);
  if (hasRootErrorRole) {
    return true;
  }
  const hasSyntaxText = svg.includes("Syntax error in text");
  const hasErrorIconClass =
    /\bclass=["'][^"']*\berror-icon\b[^"']*["']/i.test(svg);
  return hasSyntaxText && hasErrorIconClass;
}

export function buildTermalMermaidConfig(appearance: MonacoAppearance): MermaidConfigInput {
  const isDark = appearance === "dark";
  const look = readActiveDiagramLook();
  const palette = readActiveDiagramPalette();

  // Palette policy: when the user has picked a specific Mermaid
  // preset (anything other than `match`), force that preset and
  // skip our Markdown-theme palette overrides so they see the
  // preset cleanly. `match` keeps the current behaviour: derive
  // the theme from Monaco appearance and layer the Markdown theme
  // on top via `themeVariables`.
  const theme: "default" | "dark" | "forest" | "neutral" | "base" =
    palette === "match" ? (isDark ? "dark" : "default") : palette;
  const markdownOverrides =
    palette === "match" ? buildMarkdownThemeVariables() : {};

  return {
    ...TERMAL_MERMAID_BASE_CONFIG,
    darkMode: isDark,
    theme,
    // Render aesthetic. `handDrawn` routes through Mermaid's rough.js
    // integration; `classic` is the long-standing default. We also
    // pin `handDrawnSeed` so the sketch is deterministic across
    // re-renders of the same diagram
    // (otherwise every keystroke in Source mode re-wobbles the
    // strokes, which is distracting).
    look,
    handDrawnSeed: DIAGRAM_HAND_DRAWN_SEED,
    // Re-apply the theme variables AFTER theme selection. Mermaid's
    // theme presets set their own `fontSize` defaults; spreading our
    // overrides last keeps the tighter 12px in force regardless of
    // which theme the diagram ends up in. Markdown-theme palette
    // overrides apply only when palette is `match` — other presets
    // render with their own colors so the user sees the preset
    // honestly.
    themeVariables: {
      ...TERMAL_MERMAID_THEME_VARIABLES,
      ...markdownOverrides,
    },
  };
}

export function readActiveDiagramLook(): DiagramLook {
  if (typeof document === "undefined") {
    return DEFAULT_DIAGRAM_LOOK;
  }
  const stored = document.documentElement.dataset.diagramLook;
  return isDiagramLook(stored) ? stored : DEFAULT_DIAGRAM_LOOK;
}

export function readActiveDiagramPalette(): DiagramPalette {
  if (typeof document === "undefined") {
    return DEFAULT_DIAGRAM_PALETTE;
  }
  const stored = document.documentElement.dataset.diagramPalette;
  return isDiagramPalette(stored) ? stored : DEFAULT_DIAGRAM_PALETTE;
}
