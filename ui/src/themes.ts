export const THEME_STORAGE_KEY = "termal-ui-theme";

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

export function isThemeId(value: string | null | undefined): value is ThemeId {
  return THEMES.some((theme) => theme.id === value);
}

export function getStoredThemePreference(): ThemeId {
  if (typeof window === "undefined") {
    return DEFAULT_THEME_ID;
  }

  const storedTheme = window.localStorage.getItem(THEME_STORAGE_KEY);
  return isThemeId(storedTheme) ? storedTheme : DEFAULT_THEME_ID;
}

export function persistThemePreference(themeId: ThemeId) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(THEME_STORAGE_KEY, themeId);
}

export function applyThemePreference(themeId: ThemeId) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.dataset.theme = themeId;
}
