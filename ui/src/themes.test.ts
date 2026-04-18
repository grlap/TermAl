import {
  DEFAULT_DENSITY_PERCENT,
  DEFAULT_DIAGRAM_LOOK,
  DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE,
  DEFAULT_EDITOR_FONT_SIZE_PX,
  DEFAULT_FONT_SIZE_PX,
  DEFAULT_MARKDOWN_STYLE_ID,
  DEFAULT_MARKDOWN_THEME_ID,
  DEFAULT_STYLE_ID,
  DEFAULT_THEME_ID,
  DENSITY_STORAGE_KEY,
  DIAGRAM_LOOKS,
  DIAGRAM_LOOK_STORAGE_KEY,
  DIAGRAM_THEME_OVERRIDE_STORAGE_KEY,
  EDITOR_FONT_SIZE_STORAGE_KEY,
  FONT_SIZE_STORAGE_KEY,
  MARKDOWN_STYLES,
  MARKDOWN_STYLE_STORAGE_KEY,
  MARKDOWN_THEMES,
  MARKDOWN_THEME_STORAGE_KEY,
  MAX_DENSITY_PERCENT,
  MAX_EDITOR_FONT_SIZE_PX,
  MAX_FONT_SIZE_PX,
  MIN_DENSITY_PERCENT,
  MIN_EDITOR_FONT_SIZE_PX,
  MIN_FONT_SIZE_PX,
  STYLES,
  STYLE_STORAGE_KEY,
  THEME_STORAGE_KEY,
  THEMES,
  applyDensityPreference,
  applyDiagramLookPreference,
  applyDiagramThemeOverridePreference,
  applyFontSizePreference,
  applyMarkdownStylePreference,
  applyMarkdownThemePreference,
  applyStylePreference,
  applyThemePreference,
  clampDensityPreference,
  clampEditorFontSizePreference,
  clampFontSizePreference,
  getStoredDensityPreference,
  getStoredDiagramLookPreference,
  getStoredDiagramThemeOverridePreference,
  getStoredEditorFontSizePreference,
  getStoredFontSizePreference,
  getStoredMarkdownStylePreference,
  getStoredMarkdownThemePreference,
  getStoredStylePreference,
  getStoredThemePreference,
  isDiagramLook,
  isDiagramThemeOverrideMode,
  isMarkdownStyleId,
  isMarkdownThemeId,
  isStyleId,
  isThemeId,
  persistDensityPreference,
  persistDiagramLookPreference,
  persistDiagramThemeOverridePreference,
  persistEditorFontSizePreference,
  persistFontSizePreference,
  persistMarkdownStylePreference,
  persistMarkdownThemePreference,
  persistStylePreference,
  persistThemePreference,
} from "./themes";

describe("theme helpers", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    document.documentElement.removeAttribute("data-ui-style");
    document.documentElement.removeAttribute("data-markdown-theme");
    document.documentElement.removeAttribute("data-markdown-style");
    document.documentElement.removeAttribute("data-diagram-theme-override");
    document.documentElement.removeAttribute("data-diagram-look");
    document.documentElement.style.removeProperty("font-size");
    document.documentElement.style.removeProperty("--density-scale");
  });

  it("returns the default theme when storage is empty or invalid", () => {
    expect(getStoredThemePreference()).toBe(DEFAULT_THEME_ID);

    window.localStorage.setItem(THEME_STORAGE_KEY, "not-a-theme");
    expect(getStoredThemePreference()).toBe(DEFAULT_THEME_ID);
  });

  it("reads and persists valid stored themes", () => {
    persistThemePreference("terminal");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("terminal");
    expect(getStoredThemePreference()).toBe("terminal");
  });

  it("reads and persists valid stored styles", () => {
    expect(getStoredStylePreference()).toBe(DEFAULT_STYLE_ID);

    persistStylePreference("terminal-style");
    expect(window.localStorage.getItem(STYLE_STORAGE_KEY)).toBe("terminal-style");
    expect(getStoredStylePreference()).toBe("terminal-style");
  });

  it("applies the selected theme and style to the document root", () => {
    applyThemePreference("seaglass");
    applyStylePreference("blueprint-style");
    expect(document.documentElement.dataset.theme).toBe("seaglass");
    expect(document.documentElement.dataset.uiStyle).toBe("blueprint-style");
  });

  it("returns the default font size when storage is empty or invalid", () => {
    expect(getStoredFontSizePreference()).toBe(DEFAULT_FONT_SIZE_PX);

    window.localStorage.setItem(FONT_SIZE_STORAGE_KEY, "not-a-size");
    expect(getStoredFontSizePreference()).toBe(DEFAULT_FONT_SIZE_PX);
  });

  it("reads and persists a valid stored font size", () => {
    persistFontSizePreference(18);
    expect(window.localStorage.getItem(FONT_SIZE_STORAGE_KEY)).toBe("18");
    expect(getStoredFontSizePreference()).toBe(18);
  });

  it("clamps and applies the selected font size to the document root", () => {
    expect(clampFontSizePreference(MIN_FONT_SIZE_PX - 2)).toBe(MIN_FONT_SIZE_PX);
    expect(clampFontSizePreference(MAX_FONT_SIZE_PX + 3)).toBe(MAX_FONT_SIZE_PX);

    applyFontSizePreference(18);
    expect(document.documentElement.style.fontSize).toBe("18px");
  });

  it("returns the default editor font size when storage is empty or invalid", () => {
    expect(getStoredEditorFontSizePreference()).toBe(DEFAULT_EDITOR_FONT_SIZE_PX);

    window.localStorage.setItem(EDITOR_FONT_SIZE_STORAGE_KEY, "not-a-size");
    expect(getStoredEditorFontSizePreference()).toBe(DEFAULT_EDITOR_FONT_SIZE_PX);
  });

  it("reads, persists, and clamps a valid editor font size", () => {
    persistEditorFontSizePreference(18);
    expect(window.localStorage.getItem(EDITOR_FONT_SIZE_STORAGE_KEY)).toBe("18");
    expect(getStoredEditorFontSizePreference()).toBe(18);
    expect(clampEditorFontSizePreference(MIN_EDITOR_FONT_SIZE_PX - 2)).toBe(MIN_EDITOR_FONT_SIZE_PX);
    expect(clampEditorFontSizePreference(MAX_EDITOR_FONT_SIZE_PX + 3)).toBe(MAX_EDITOR_FONT_SIZE_PX);
  });

  it("reads, persists, and applies density preferences", () => {
    expect(getStoredDensityPreference()).toBe(DEFAULT_DENSITY_PERCENT);

    window.localStorage.setItem(DENSITY_STORAGE_KEY, "not-a-density");
    expect(getStoredDensityPreference()).toBe(DEFAULT_DENSITY_PERCENT);

    persistDensityPreference(85);
    expect(window.localStorage.getItem(DENSITY_STORAGE_KEY)).toBe("85");
    expect(getStoredDensityPreference()).toBe(85);
    expect(clampDensityPreference(MIN_DENSITY_PERCENT - 9)).toBe(MIN_DENSITY_PERCENT);
    expect(clampDensityPreference(MAX_DENSITY_PERCENT + 9)).toBe(MAX_DENSITY_PERCENT);

    applyDensityPreference(85);
    expect(document.documentElement.style.getPropertyValue("--density-scale")).toBe("0.85");
  });

  it("keeps the theme and style registries aligned with the runtime guards", () => {
    const themeIds = THEMES.map((theme) => theme.id);
    const styleIds = STYLES.map((style) => style.id);

    expect(themeIds).toContain(DEFAULT_THEME_ID);
    expect(themeIds).toEqual(
      expect.arrayContaining([
        "gallery-white",
        "workbench-light",
        "silver-white",
        "porcelain-white",
        "code-black",
        "obsidian-black",
        "oxide-black",
        "evergreen-night",
        "violet-night",
        "sunset-paper",
        "blueprint",
      ]),
    );
    expect(themeIds.every((themeId) => isThemeId(themeId))).toBe(true);
    expect(styleIds).toContain(DEFAULT_STYLE_ID);
    expect(styleIds.every((styleId) => isStyleId(styleId))).toBe(true);
  });

  it("returns the default markdown theme and style when storage is empty or invalid", () => {
    expect(getStoredMarkdownThemePreference()).toBe(DEFAULT_MARKDOWN_THEME_ID);
    expect(getStoredMarkdownStylePreference()).toBe(DEFAULT_MARKDOWN_STYLE_ID);

    window.localStorage.setItem(MARKDOWN_THEME_STORAGE_KEY, "not-a-markdown-theme");
    window.localStorage.setItem(MARKDOWN_STYLE_STORAGE_KEY, "not-a-markdown-style");
    expect(getStoredMarkdownThemePreference()).toBe(DEFAULT_MARKDOWN_THEME_ID);
    expect(getStoredMarkdownStylePreference()).toBe(DEFAULT_MARKDOWN_STYLE_ID);
  });

  it("reads and persists valid markdown theme / style preferences", () => {
    persistMarkdownThemePreference(DEFAULT_MARKDOWN_THEME_ID);
    persistMarkdownStylePreference(DEFAULT_MARKDOWN_STYLE_ID);
    expect(window.localStorage.getItem(MARKDOWN_THEME_STORAGE_KEY)).toBe(
      DEFAULT_MARKDOWN_THEME_ID,
    );
    expect(window.localStorage.getItem(MARKDOWN_STYLE_STORAGE_KEY)).toBe(
      DEFAULT_MARKDOWN_STYLE_ID,
    );
    expect(getStoredMarkdownThemePreference()).toBe(DEFAULT_MARKDOWN_THEME_ID);
    expect(getStoredMarkdownStylePreference()).toBe(DEFAULT_MARKDOWN_STYLE_ID);
  });

  it("applies the selected markdown theme and style to the document root", () => {
    applyMarkdownThemePreference(DEFAULT_MARKDOWN_THEME_ID);
    applyMarkdownStylePreference(DEFAULT_MARKDOWN_STYLE_ID);
    expect(document.documentElement.dataset.markdownTheme).toBe(
      DEFAULT_MARKDOWN_THEME_ID,
    );
    expect(document.documentElement.dataset.markdownStyle).toBe(
      DEFAULT_MARKDOWN_STYLE_ID,
    );
  });

  it("keeps the markdown theme and style registries aligned with the runtime guards", () => {
    const markdownThemeIds = MARKDOWN_THEMES.map((theme) => theme.id);
    const markdownStyleIds = MARKDOWN_STYLES.map((style) => style.id);

    expect(markdownThemeIds).toContain(DEFAULT_MARKDOWN_THEME_ID);
    expect(markdownThemeIds.every((themeId) => isMarkdownThemeId(themeId))).toBe(
      true,
    );
    expect(markdownStyleIds).toContain(DEFAULT_MARKDOWN_STYLE_ID);
    expect(markdownStyleIds.every((styleId) => isMarkdownStyleId(styleId))).toBe(
      true,
    );
    // Sentinel list: Phase 3 shipped the `github-light`,
    // `github-dark`, and `terminal` themes plus the `document` and
    // `compact` styles on top of the `match-ui` default. Update
    // this expectation when later phases add more presets so the
    // registry change shows up intentionally in review diffs.
    expect(markdownThemeIds).toEqual([
      "match-ui",
      "github-light",
      "github-dark",
      "terminal",
    ]);
    expect(markdownStyleIds).toEqual(["match-ui", "document", "compact"]);
  });

  it("returns the default diagram-theme-override mode when storage is empty or invalid", () => {
    expect(getStoredDiagramThemeOverridePreference()).toBe(
      DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE,
    );
    expect(DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE).toBe("on");

    window.localStorage.setItem(DIAGRAM_THEME_OVERRIDE_STORAGE_KEY, "maybe");
    expect(getStoredDiagramThemeOverridePreference()).toBe(
      DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE,
    );
  });

  it("reads, persists, and applies the diagram-theme-override preference", () => {
    persistDiagramThemeOverridePreference("off");
    expect(window.localStorage.getItem(DIAGRAM_THEME_OVERRIDE_STORAGE_KEY)).toBe(
      "off",
    );
    expect(getStoredDiagramThemeOverridePreference()).toBe("off");

    applyDiagramThemeOverridePreference("off");
    expect(document.documentElement.dataset.diagramThemeOverride).toBe("off");

    persistDiagramThemeOverridePreference("on");
    applyDiagramThemeOverridePreference("on");
    expect(window.localStorage.getItem(DIAGRAM_THEME_OVERRIDE_STORAGE_KEY)).toBe(
      "on",
    );
    expect(document.documentElement.dataset.diagramThemeOverride).toBe("on");
  });

  it("runtime-guards the diagram-theme-override mode", () => {
    expect(isDiagramThemeOverrideMode("on")).toBe(true);
    expect(isDiagramThemeOverrideMode("off")).toBe(true);
    expect(isDiagramThemeOverrideMode(null)).toBe(false);
    expect(isDiagramThemeOverrideMode(undefined)).toBe(false);
    expect(isDiagramThemeOverrideMode("maybe")).toBe(false);
  });

  it("returns the default diagram look when storage is empty or invalid", () => {
    expect(getStoredDiagramLookPreference()).toBe(DEFAULT_DIAGRAM_LOOK);
    expect(DEFAULT_DIAGRAM_LOOK).toBe("classic");

    window.localStorage.setItem(DIAGRAM_LOOK_STORAGE_KEY, "sketchy");
    expect(getStoredDiagramLookPreference()).toBe(DEFAULT_DIAGRAM_LOOK);
  });

  it("reads, persists, and applies the diagram look", () => {
    persistDiagramLookPreference("handDrawn");
    expect(window.localStorage.getItem(DIAGRAM_LOOK_STORAGE_KEY)).toBe("handDrawn");
    expect(getStoredDiagramLookPreference()).toBe("handDrawn");

    applyDiagramLookPreference("handDrawn");
    expect(document.documentElement.dataset.diagramLook).toBe("handDrawn");

    persistDiagramLookPreference("neo");
    applyDiagramLookPreference("neo");
    expect(window.localStorage.getItem(DIAGRAM_LOOK_STORAGE_KEY)).toBe("neo");
    expect(document.documentElement.dataset.diagramLook).toBe("neo");
  });

  it("keeps the diagram-look registry aligned with the runtime guard", () => {
    const ids = DIAGRAM_LOOKS.map((entry) => entry.id);
    expect(ids).toContain(DEFAULT_DIAGRAM_LOOK);
    expect(ids.every((id) => isDiagramLook(id))).toBe(true);
    // Sentinel list — Mermaid 11.x ships classic, handDrawn, neo.
    expect(ids).toEqual(["classic", "handDrawn", "neo"]);
    expect(isDiagramLook("sketchy")).toBe(false);
    expect(isDiagramLook(null)).toBe(false);
  });
});
