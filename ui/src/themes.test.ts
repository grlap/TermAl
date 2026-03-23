import {
  DEFAULT_DENSITY_PERCENT,
  DEFAULT_EDITOR_FONT_SIZE_PX,
  DEFAULT_FONT_SIZE_PX,
  DEFAULT_STYLE_ID,
  DEFAULT_THEME_ID,
  DENSITY_STORAGE_KEY,
  EDITOR_FONT_SIZE_STORAGE_KEY,
  FONT_SIZE_STORAGE_KEY,
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
  applyFontSizePreference,
  applyStylePreference,
  applyThemePreference,
  clampDensityPreference,
  clampEditorFontSizePreference,
  clampFontSizePreference,
  getStoredDensityPreference,
  getStoredEditorFontSizePreference,
  getStoredFontSizePreference,
  getStoredStylePreference,
  getStoredThemePreference,
  isStyleId,
  isThemeId,
  persistDensityPreference,
  persistEditorFontSizePreference,
  persistFontSizePreference,
  persistStylePreference,
  persistThemePreference,
} from "./themes";

describe("theme helpers", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    document.documentElement.removeAttribute("data-ui-style");
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
        "porcelain-white",
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
});
