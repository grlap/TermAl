export const THEME_STORAGE_KEY = "termal-ui-theme";
export const STYLE_STORAGE_KEY = "termal-ui-style";
export const MARKDOWN_THEME_STORAGE_KEY = "termal-markdown-theme";
export const MARKDOWN_STYLE_STORAGE_KEY = "termal-markdown-style";
export const DIAGRAM_THEME_OVERRIDE_STORAGE_KEY = "termal-diagram-theme-override";
export const DIAGRAM_LOOK_STORAGE_KEY = "termal-diagram-look";
export const DIAGRAM_PALETTE_STORAGE_KEY = "termal-diagram-palette";
export const FONT_SIZE_STORAGE_KEY = "termal-ui-font-size";
export const EDITOR_FONT_SIZE_STORAGE_KEY = "termal-editor-font-size";
export const DENSITY_STORAGE_KEY = "termal-ui-density";
export const DEFAULT_FONT_SIZE_PX = 16;
export const MIN_FONT_SIZE_PX = 11;
export const MAX_FONT_SIZE_PX = 20;
export const DEFAULT_EDITOR_FONT_SIZE_PX = 13;
export const MIN_EDITOR_FONT_SIZE_PX = 11;
export const MAX_EDITOR_FONT_SIZE_PX = 24;
export const DEFAULT_DENSITY_PERCENT = 100;
export const MIN_DENSITY_PERCENT = 80;
export const MAX_DENSITY_PERCENT = 120;
export const DENSITY_STEP_PERCENT = 5;

export const STYLES = [
  {
    id: "theme-default",
    name: "Match Theme",
    description: "Use the visual treatment bundled with the selected theme.",
  },
  {
    id: "editorial",
    name: "Editorial",
    description: "Soft paper surfaces, serif hierarchy, and warmer rounded chrome.",
  },
  {
    id: "studio",
    name: "Studio",
    description: "Clean sans-serif surfaces with polished glass and restrained depth.",
  },
  {
    id: "terminal-style",
    name: "Terminal",
    description: "Monospace chrome, tighter corners, and flatter control-room surfaces.",
  },
  {
    id: "blueprint-style",
    name: "Blueprint",
    description: "Technical mono styling with sharper drafting-table geometry.",
  },
] as const;

export type StyleId = (typeof STYLES)[number]["id"];

export const DEFAULT_STYLE_ID: StyleId = "theme-default";

export const THEMES = [
  {
    id: "warm-light",
    name: "Warm Light",
    description: "The current parchment look, kept as the default.",
    swatches: ["#f4efe4", "#cf9d34", "#2f5f80"],
  },
  {
    id: "gallery-white",
    name: "Gallery White",
    description: "Bright studio whites with cobalt lines and a restrained vermilion edge.",
    swatches: ["#f8fbff", "#2b6cb0", "#d1573b"],
  },
  {
    id: "workbench-light",
    name: "Workbench Light",
    description: "Neutral editor whites with cool slate chrome and muted coral accents.",
    swatches: ["#f3f4f6", "#607d93", "#d86f58"],
  },
  {
    id: "silver-white",
    name: "Silver White",
    description: "Neutral whites with cool grey chrome and restrained slate accents.",
    swatches: ["#f5f6f7", "#a5adb6", "#4c5e70"],
  },
  {
    id: "porcelain-white",
    name: "Porcelain White",
    description: "Polished porcelain surfaces with jade, plum, and brushed-silver accents.",
    swatches: ["#fcfbf8", "#368873", "#8f5a7a"],
  },
  {
    id: "dark",
    name: "Darkroom",
    description: "Charcoal panels with warm copper and steel-blue accents.",
    swatches: ["#1a1a1f", "#e07050", "#5a9ac0"],
  },
  {
    id: "code-black",
    name: "Code Black",
    description: "Near-black editor chrome with graphite panels, blue actions, and amber status accents.",
    swatches: ["#1e1e1e", "#3794ff", "#d7ba7d"],
  },
  {
    id: "obsidian-black",
    name: "Obsidian Black",
    description: "True-black glass with electric cyan, ember orange, and violet highlights.",
    swatches: ["#080a0d", "#63d2ff", "#ff7a59"],
  },
  {
    id: "oxide-black",
    name: "Oxide Black",
    description: "Matte black panels with oxidized rust, brass, and pale mint accents.",
    swatches: ["#0d0b09", "#d46e43", "#6fc7a1"],
  },
  {
    id: "evergreen-night",
    name: "Evergreen Night",
    description: "Soft charcoal surfaces with emerald accents and cool jade highlights.",
    swatches: ["#141917", "#59c97b", "#88d8a5"],
  },
  {
    id: "seaglass",
    name: "Sea Glass",
    description: "A cooler daylight palette with misty teal surfaces.",
    swatches: ["#ebf6f6", "#2d6f88", "#4ea39a"],
  },
  {
    id: "terminal",
    name: "Terminal",
    description: "Deep phosphor greens with a sharper control-room feel.",
    swatches: ["#07120c", "#5ccf86", "#d7c46a"],
  },
  {
    id: "violet-night",
    name: "Violet Night",
    description: "Midnight indigo with ultraviolet accents and icy blue highlights.",
    swatches: ["#151528", "#c18cff", "#7da5ff"],
  },
  {
    id: "sunset-paper",
    name: "Sunset Paper",
    description: "Apricot paper, terracotta signals, and a mellow dusk glow.",
    swatches: ["#f7e7da", "#d96d4f", "#b05f76"],
  },
  {
    id: "blueprint",
    name: "Blueprint",
    description: "Drafting-table navy with bright cyan lines and brass accents.",
    swatches: ["#0d2132", "#79d4ff", "#f3cf7a"],
  },
  {
    id: "ember",
    name: "Ember",
    description: "Smoldering charcoal with glowing amber and deep coal-red accents.",
    swatches: ["#1c1410", "#e8944a", "#c44830"],
  },
  {
    id: "frost",
    name: "Frost",
    description: "Icy blue-white surfaces with steel blue and cool silver tones.",
    swatches: ["#edf2f8", "#3878b0", "#c04858"],
  },
  {
    id: "sakura",
    name: "Sakura",
    description: "Cherry-blossom pinks on soft petal paper with plum and muted green.",
    swatches: ["#faf2f4", "#d07090", "#6878a8"],
  },
] as const;

export type ThemeId = (typeof THEMES)[number]["id"];

export const DEFAULT_THEME_ID: ThemeId = "warm-light";

// Markdown theme / Markdown style are two axes that apply specifically
// to rendered-Markdown surfaces (message cards, rendered diff preview,
// source-panel preview, Mermaid / KaTeX rendering). They sit alongside
// the UI theme + UI style axes above — a user can keep a light
// workspace chrome while rendering Markdown with a GitHub-like or
// newspaper-like preset. See
// `docs/features/markdown-themes-and-styles.md` for the full brief.
export const MARKDOWN_THEMES = [
  {
    id: "match-ui",
    name: "Match UI",
    description:
      "Inherit Markdown colors, typography, and Mermaid / KaTeX theming from the active UI theme.",
    swatches: ["inherit", "inherit", "inherit"] as const,
  },
  {
    id: "github-light",
    name: "GitHub Light",
    description:
      "GitHub-style document rendering with blue links, neutral code blocks, and crisp table lines.",
    swatches: ["#ffffff", "#0969da", "#24292f"] as const,
  },
  {
    id: "github-dark",
    name: "GitHub Dark",
    description:
      "Dark companion to GitHub Light — deep panels with bright cyan links and soft grey prose.",
    swatches: ["#0d1117", "#58a6ff", "#c9d1d9"] as const,
  },
  {
    id: "terminal",
    name: "Terminal",
    description:
      "Monospace-first reading style with phosphor green prose and amber headings on a dark canvas.",
    swatches: ["#0a120d", "#5ccf86", "#d7c46a"] as const,
  },
] as const;

export type MarkdownThemeId = (typeof MARKDOWN_THEMES)[number]["id"];

export const DEFAULT_MARKDOWN_THEME_ID: MarkdownThemeId = "match-ui";

export const MARKDOWN_STYLES = [
  {
    id: "match-ui",
    name: "Match UI",
    description:
      "Use the typography and spacing treatment bundled with the active UI style.",
  },
  {
    id: "document",
    name: "Document",
    description:
      "Generous line height and heading margins for longer-form reading passes.",
  },
  {
    id: "compact",
    name: "Compact",
    description:
      "Tighter heading margins and denser tables for review-heavy sessions.",
  },
] as const;

export type MarkdownStyleId = (typeof MARKDOWN_STYLES)[number]["id"];

export const DEFAULT_MARKDOWN_STYLE_ID: MarkdownStyleId = "match-ui";

// Diagram theme override is a single orthogonal toggle that decides
// whether an author-authored `%%{init: ...}%%` directive (or YAML
// frontmatter `theme:` / `themeVariables:` keys) in a Mermaid
// diagram source can override the reader's Markdown theme. See
// `docs/features/markdown-themes-and-styles.md` §Mermaid diagram
// theming for the full precedence story.
//
// "on" (default) = Override mode: strip author directives at render
//   time so TermAl's Markdown theme always wins.
// "off" = Respect mode: author directives pass through unchanged.
export type DiagramThemeOverrideMode = "on" | "off";

export const DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE: DiagramThemeOverrideMode = "on";

// Mermaid's render aesthetic. `classic` is the default sharp look
// TermAl has always used; `handDrawn` routes through Mermaid's
// rough.js integration for a sketched / whiteboard feel;
// `neo` is Mermaid's newer refreshed sharp look. Configured via
// the top-level `look` field in `mermaid.initialize`. Set to the
// user's preference at render time; changing the preference
// applies to the next Mermaid render.
export const DIAGRAM_LOOKS = [
  {
    id: "classic",
    name: "Classic",
    description:
      "Sharp geometric nodes and edges — Mermaid's default rendering.",
  },
  {
    id: "handDrawn",
    name: "Hand-drawn",
    description:
      "Sketchy rough.js strokes for a whiteboard / notebook feel. Same palette, wobbly lines.",
  },
  {
    id: "neo",
    name: "Neo",
    description:
      "Mermaid's newer sharp look — tighter corners, slightly lighter edges.",
  },
] as const;

export type DiagramLook = (typeof DIAGRAM_LOOKS)[number]["id"];

export const DEFAULT_DIAGRAM_LOOK: DiagramLook = "classic";

// Fixed seed for handDrawn renders so the sketch lines don't
// jitter between re-renders of the same diagram. Arbitrary
// non-zero value chosen for stability; see `handDrawnSeed` in
// Mermaid's config.type.d.ts.
export const DIAGRAM_HAND_DRAWN_SEED = 42;

// Mermaid ships five built-in theme presets and auto-picks between
// `default` (light) and `dark` from the `darkMode` flag. When the
// user wants a specific preset (e.g. forest) independent of their
// Markdown theme colors or Monaco appearance, they pick it here.
//
// `match` (default) = keep the current behaviour: let TermAl derive
//   Mermaid's theme from the Monaco appearance and layer the
//   Markdown-theme palette overrides on top.
// `default` / `dark` / `forest` / `neutral` / `base` = force
//   Mermaid's named preset regardless of Monaco appearance, and
//   skip the Markdown-theme palette overrides so the user sees the
//   preset's colors cleanly.
export const DIAGRAM_PALETTES = [
  {
    id: "match",
    name: "Match Markdown theme",
    description:
      "Follow the active Markdown theme's palette overrides. Best pick when prose and diagrams should share a look.",
  },
  {
    id: "default",
    name: "Default",
    description: "Mermaid's standard light palette — blue accents on a neutral surface.",
  },
  {
    id: "dark",
    name: "Dark",
    description: "Mermaid's standard dark palette — cool accents on a charcoal surface.",
  },
  {
    id: "forest",
    name: "Forest",
    description: "Green, nature-inspired palette with warm edge lines.",
  },
  {
    id: "neutral",
    name: "Neutral",
    description: "Grayscale palette — no hue emphasis, readable against any Markdown theme.",
  },
  {
    id: "base",
    name: "Base",
    description:
      "Mermaid's neutral starting point, intended for per-diagram customization through themeVariables.",
  },
] as const;

export type DiagramPalette = (typeof DIAGRAM_PALETTES)[number]["id"];

export const DEFAULT_DIAGRAM_PALETTE: DiagramPalette = "match";

export function isStyleId(value: string | null | undefined): value is StyleId {
  return STYLES.some((style) => style.id === value);
}

export function isThemeId(value: string | null | undefined): value is ThemeId {
  return THEMES.some((theme) => theme.id === value);
}

export function isMarkdownThemeId(
  value: string | null | undefined,
): value is MarkdownThemeId {
  return MARKDOWN_THEMES.some((theme) => theme.id === value);
}

export function isMarkdownStyleId(
  value: string | null | undefined,
): value is MarkdownStyleId {
  return MARKDOWN_STYLES.some((style) => style.id === value);
}

export function isDiagramThemeOverrideMode(
  value: string | null | undefined,
): value is DiagramThemeOverrideMode {
  return value === "on" || value === "off";
}

export function isDiagramLook(value: string | null | undefined): value is DiagramLook {
  return DIAGRAM_LOOKS.some((look) => look.id === value);
}

export function isDiagramPalette(
  value: string | null | undefined,
): value is DiagramPalette {
  return DIAGRAM_PALETTES.some((palette) => palette.id === value);
}

export function getStoredThemePreference(): ThemeId {
  if (typeof window === "undefined") {
    return DEFAULT_THEME_ID;
  }

  const storedTheme = window.localStorage.getItem(THEME_STORAGE_KEY);
  return isThemeId(storedTheme) ? storedTheme : DEFAULT_THEME_ID;
}

export function getStoredStylePreference(): StyleId {
  if (typeof window === "undefined") {
    return DEFAULT_STYLE_ID;
  }

  const storedStyle = window.localStorage.getItem(STYLE_STORAGE_KEY);
  return isStyleId(storedStyle) ? storedStyle : DEFAULT_STYLE_ID;
}

export function persistThemePreference(themeId: ThemeId) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(THEME_STORAGE_KEY, themeId);
}

export function persistStylePreference(styleId: StyleId) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(STYLE_STORAGE_KEY, styleId);
}

export function applyThemePreference(themeId: ThemeId) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.theme = themeId;
}

export function applyStylePreference(styleId: StyleId) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.uiStyle = styleId;
}

export function getStoredMarkdownThemePreference(): MarkdownThemeId {
  if (typeof window === "undefined") {
    return DEFAULT_MARKDOWN_THEME_ID;
  }

  const stored = window.localStorage.getItem(MARKDOWN_THEME_STORAGE_KEY);
  return isMarkdownThemeId(stored) ? stored : DEFAULT_MARKDOWN_THEME_ID;
}

export function getStoredMarkdownStylePreference(): MarkdownStyleId {
  if (typeof window === "undefined") {
    return DEFAULT_MARKDOWN_STYLE_ID;
  }

  const stored = window.localStorage.getItem(MARKDOWN_STYLE_STORAGE_KEY);
  return isMarkdownStyleId(stored) ? stored : DEFAULT_MARKDOWN_STYLE_ID;
}

export function persistMarkdownThemePreference(markdownThemeId: MarkdownThemeId) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(MARKDOWN_THEME_STORAGE_KEY, markdownThemeId);
}

export function persistMarkdownStylePreference(markdownStyleId: MarkdownStyleId) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(MARKDOWN_STYLE_STORAGE_KEY, markdownStyleId);
}

export function applyMarkdownThemePreference(markdownThemeId: MarkdownThemeId) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.markdownTheme = markdownThemeId;
}

export function applyMarkdownStylePreference(markdownStyleId: MarkdownStyleId) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.markdownStyle = markdownStyleId;
}

export function getStoredDiagramThemeOverridePreference(): DiagramThemeOverrideMode {
  if (typeof window === "undefined") {
    return DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE;
  }

  const stored = window.localStorage.getItem(DIAGRAM_THEME_OVERRIDE_STORAGE_KEY);
  return isDiagramThemeOverrideMode(stored)
    ? stored
    : DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE;
}

export function persistDiagramThemeOverridePreference(mode: DiagramThemeOverrideMode) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(DIAGRAM_THEME_OVERRIDE_STORAGE_KEY, mode);
}

export function applyDiagramThemeOverridePreference(mode: DiagramThemeOverrideMode) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.diagramThemeOverride = mode;
}

export function getStoredDiagramLookPreference(): DiagramLook {
  if (typeof window === "undefined") {
    return DEFAULT_DIAGRAM_LOOK;
  }

  const stored = window.localStorage.getItem(DIAGRAM_LOOK_STORAGE_KEY);
  return isDiagramLook(stored) ? stored : DEFAULT_DIAGRAM_LOOK;
}

export function persistDiagramLookPreference(look: DiagramLook) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(DIAGRAM_LOOK_STORAGE_KEY, look);
}

export function applyDiagramLookPreference(look: DiagramLook) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.diagramLook = look;
}

export function getStoredDiagramPalettePreference(): DiagramPalette {
  if (typeof window === "undefined") {
    return DEFAULT_DIAGRAM_PALETTE;
  }

  const stored = window.localStorage.getItem(DIAGRAM_PALETTE_STORAGE_KEY);
  return isDiagramPalette(stored) ? stored : DEFAULT_DIAGRAM_PALETTE;
}

export function persistDiagramPalettePreference(palette: DiagramPalette) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(DIAGRAM_PALETTE_STORAGE_KEY, palette);
}

export function applyDiagramPalettePreference(palette: DiagramPalette) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.diagramPalette = palette;
}

export function clampFontSizePreference(value: number): number {
  if (!Number.isFinite(value)) {
    return DEFAULT_FONT_SIZE_PX;
  }

  return Math.min(MAX_FONT_SIZE_PX, Math.max(MIN_FONT_SIZE_PX, Math.round(value)));
}

export function getStoredFontSizePreference(): number {
  if (typeof window === "undefined") {
    return DEFAULT_FONT_SIZE_PX;
  }

  const storedFontSize = window.localStorage.getItem(FONT_SIZE_STORAGE_KEY);
  if (!storedFontSize) {
    return DEFAULT_FONT_SIZE_PX;
  }

  return clampFontSizePreference(Number.parseInt(storedFontSize, 10));
}

export function persistFontSizePreference(fontSizePx: number) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(
    FONT_SIZE_STORAGE_KEY,
    clampFontSizePreference(fontSizePx).toString(),
  );
}

export function applyFontSizePreference(fontSizePx: number) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.style.fontSize = `${clampFontSizePreference(fontSizePx)}px`;
}

export function clampEditorFontSizePreference(value: number): number {
  if (!Number.isFinite(value)) {
    return DEFAULT_EDITOR_FONT_SIZE_PX;
  }

  return Math.min(MAX_EDITOR_FONT_SIZE_PX, Math.max(MIN_EDITOR_FONT_SIZE_PX, Math.round(value)));
}

export function getStoredEditorFontSizePreference(): number {
  if (typeof window === "undefined") {
    return DEFAULT_EDITOR_FONT_SIZE_PX;
  }

  const storedFontSize = window.localStorage.getItem(EDITOR_FONT_SIZE_STORAGE_KEY);
  if (!storedFontSize) {
    return DEFAULT_EDITOR_FONT_SIZE_PX;
  }

  return clampEditorFontSizePreference(Number.parseInt(storedFontSize, 10));
}

export function persistEditorFontSizePreference(fontSizePx: number) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(
    EDITOR_FONT_SIZE_STORAGE_KEY,
    clampEditorFontSizePreference(fontSizePx).toString(),
  );
}

export function clampDensityPreference(value: number): number {
  if (!Number.isFinite(value)) {
    return DEFAULT_DENSITY_PERCENT;
  }

  const snappedValue = Math.round(value / DENSITY_STEP_PERCENT) * DENSITY_STEP_PERCENT;
  return Math.min(MAX_DENSITY_PERCENT, Math.max(MIN_DENSITY_PERCENT, snappedValue));
}

export function getStoredDensityPreference(): number {
  if (typeof window === "undefined") {
    return DEFAULT_DENSITY_PERCENT;
  }

  const storedDensity = window.localStorage.getItem(DENSITY_STORAGE_KEY);
  if (!storedDensity) {
    return DEFAULT_DENSITY_PERCENT;
  }

  return clampDensityPreference(Number.parseInt(storedDensity, 10));
}

export function persistDensityPreference(densityPercent: number) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(
    DENSITY_STORAGE_KEY,
    clampDensityPreference(densityPercent).toString(),
  );
}

export function applyDensityPreference(densityPercent: number) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.style.setProperty(
    "--density-scale",
    (clampDensityPreference(densityPercent) / 100).toFixed(2),
  );
}
