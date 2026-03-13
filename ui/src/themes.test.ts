import {
  DEFAULT_FONT_SIZE_PX,
  DEFAULT_THEME_ID,
  FONT_SIZE_STORAGE_KEY,
  MAX_FONT_SIZE_PX,
  MIN_FONT_SIZE_PX,
  THEME_STORAGE_KEY,
  THEMES,
  applyFontSizePreference,
  applyThemePreference,
  clampFontSizePreference,
  getStoredFontSizePreference,
  getStoredThemePreference,
  isThemeId,
  persistFontSizePreference,
  persistThemePreference,
} from "./themes";

describe("theme helpers", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    document.documentElement.style.removeProperty("font-size");
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

  it("applies the selected theme to the document root", () => {
    applyThemePreference("seaglass");
    expect(document.documentElement.dataset.theme).toBe("seaglass");
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

  it("keeps the theme registry aligned with the runtime guard", () => {
    const themeIds = THEMES.map((theme) => theme.id);

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
  });
});
