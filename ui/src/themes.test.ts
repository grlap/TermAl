import {
  DEFAULT_THEME_ID,
  THEME_STORAGE_KEY,
  THEMES,
  applyThemePreference,
  getStoredThemePreference,
  isThemeId,
  persistThemePreference,
} from "./themes";

describe("theme helpers", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
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
