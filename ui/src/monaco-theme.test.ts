// Owns the shared Monaco palette and repaint contract. These tests inspect the
// theme packet directly because jsdom does not rasterize Monaco's minimap
// canvas; the production canvas consumes the exact colors asserted here.

import { afterEach, describe, expect, it, vi } from "vitest";

import { syncMonacoTheme, type MonacoModule } from "./monaco";

type CapturedTheme = {
  base: string;
  colors: Record<string, string>;
};

const THEME_VARIABLES = ["--panel-strong", "--muted", "--signal-blue"];

afterEach(() => {
  for (const variableName of THEME_VARIABLES) {
    document.documentElement.style.removeProperty(variableName);
  }
});

function fakeMonaco() {
  const defineTheme = vi.fn();
  const setTheme = vi.fn();
  return {
    defineTheme,
    module: {
      editor: { defineTheme, setTheme },
    } as unknown as MonacoModule,
    setTheme,
  };
}

function capturedTheme(defineTheme: ReturnType<typeof vi.fn>, callIndex = 0) {
  return defineTheme.mock.calls[callIndex]?.[1] as CapturedTheme;
}

describe("Monaco theme", () => {
  it("uses an opaque dark surface for both the editor and minimap canvas", () => {
    document.documentElement.style.setProperty(
      "--panel-strong",
      "rgba(20, 30, 40, 0.42)",
    );
    document.documentElement.style.setProperty(
      "--muted",
      "rgba(100, 110, 120, 0.5)",
    );
    document.documentElement.style.setProperty("--signal-blue", "#79d4ff");
    const monaco = fakeMonaco();

    syncMonacoTheme(monaco.module, "dark");

    const theme = capturedTheme(monaco.defineTheme);
    expect(theme.base).toBe("vs-dark");
    expect(theme.colors["editor.background"]).toBe("#141e28");
    expect(theme.colors["minimap.background"]).toBe("#141e28");
    expect(theme.colors["editor.background"]).toMatch(/^#[0-9a-f]{6}$/);
    expect(theme.colors["minimap.background"]).toMatch(/^#[0-9a-f]{6}$/);
    expect(theme.colors["minimapSlider.background"]).toBe(
      theme.colors["scrollbarSlider.background"],
    );
    expect(theme.colors["minimapSlider.hoverBackground"]).toBe(
      theme.colors["scrollbarSlider.hoverBackground"],
    );
    expect(theme.colors["minimapSlider.activeBackground"]).toBe(
      theme.colors["scrollbarSlider.activeBackground"],
    );
  });

  it("redefines and reapplies the active theme when appearance changes", () => {
    const monaco = fakeMonaco();
    document.documentElement.style.setProperty("--panel-strong", "#141e28");

    syncMonacoTheme(monaco.module, "dark");
    document.documentElement.style.setProperty("--panel-strong", "#f5f6f7");
    syncMonacoTheme(monaco.module, "light");

    expect(monaco.defineTheme).toHaveBeenNthCalledWith(
      1,
      "termal-dark",
      expect.any(Object),
    );
    expect(monaco.defineTheme).toHaveBeenNthCalledWith(
      2,
      "termal-light",
      expect.any(Object),
    );
    expect(monaco.setTheme.mock.calls).toEqual([
      ["termal-dark"],
      ["termal-light"],
    ]);
    expect(capturedTheme(monaco.defineTheme, 0).colors["minimap.background"]).toBe(
      "#141e28",
    );
    expect(capturedTheme(monaco.defineTheme, 1).colors["minimap.background"]).toBe(
      "#f5f6f7",
    );
  });
});
