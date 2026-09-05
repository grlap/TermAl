import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useThemePreferencesState } from "./use-theme-preferences";

type MatchMediaController = {
  media: MediaQueryList;
  setDark: (matches: boolean) => void;
};

function installMatchMedia(initialMatches: boolean): MatchMediaController {
  let matches = initialMatches;
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  const media = {
    get matches() {
      return matches;
    },
    media: "(prefers-color-scheme: dark)",
    onchange: null,
    addEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => {
      listeners.add(listener);
    },
    removeEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => {
      listeners.delete(listener);
    },
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  } as unknown as MediaQueryList;

  vi.stubGlobal("matchMedia", vi.fn(() => media));

  return {
    media,
    setDark(nextMatches) {
      matches = nextMatches;
      const event = { matches, media: media.media } as MediaQueryListEvent;
      for (const listener of listeners) {
        listener(event);
      }
    },
  };
}

describe("useThemePreferencesState", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("switches the applied theme without a reload", () => {
    window.localStorage.setItem("termal-ui-theme", "terminal");
    installMatchMedia(false);
    const { result } = renderHook(() =>
      useThemePreferencesState({
        lightThemeId: "warm-light",
        darkThemeId: "dark",
        themeMode: "light",
      }),
    );

    expect(document.documentElement.dataset.theme).toBe("warm-light");
    act(() => result.current.toggleThemeKind());
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(window.localStorage.getItem("termal-ui-theme")).toBe("terminal");
    expect(window.localStorage.getItem("termal-ui-theme-light")).toBe("warm-light");
    expect(window.localStorage.getItem("termal-ui-theme-dark")).toBe("dark");
    expect(window.localStorage.getItem("termal-ui-theme-mode")).toBe("dark");
  });

  it("follows matchMedia live in Auto and keeps a manual session override", () => {
    const matchMedia = installMatchMedia(false);
    const { result } = renderHook(() =>
      useThemePreferencesState({
        lightThemeId: "warm-light",
        darkThemeId: "dark",
        themeMode: "auto",
      }),
    );

    expect(result.current.themeId).toBe("warm-light");
    act(() => matchMedia.setDark(true));
    expect(result.current.themeId).toBe("dark");

    act(() => result.current.toggleThemeKind());
    expect(result.current.themeId).toBe("warm-light");
    expect(result.current.themeSessionOverride).toBe("light");

    act(() => matchMedia.setDark(false));
    expect(result.current.themeId).toBe("warm-light");
    act(() => result.current.returnToAuto());
    expect(result.current.themeSessionOverride).toBeNull();
    expect(result.current.themeId).toBe("warm-light");
  });

  it("handles the global shortcut while respecting prevented and repeated events", () => {
    installMatchMedia(false);
    const { result } = renderHook(() =>
      useThemePreferencesState({
        lightThemeId: "warm-light",
        darkThemeId: "dark",
        themeMode: "light",
      }),
    );

    const toggleEvent = new KeyboardEvent("keydown", {
      cancelable: true,
      ctrlKey: true,
      key: "l",
      shiftKey: true,
    });
    act(() => window.dispatchEvent(toggleEvent));
    expect(toggleEvent.defaultPrevented).toBe(true);
    expect(result.current.themeId).toBe("dark");

    const preventedEvent = new KeyboardEvent("keydown", {
      cancelable: true,
      ctrlKey: true,
      key: "l",
      shiftKey: true,
    });
    preventedEvent.preventDefault();
    act(() => window.dispatchEvent(preventedEvent));
    expect(result.current.themeId).toBe("dark");

    const repeatedEvent = new KeyboardEvent("keydown", {
      cancelable: true,
      ctrlKey: true,
      key: "l",
      repeat: true,
      shiftKey: true,
    });
    act(() => window.dispatchEvent(repeatedEvent));
    expect(result.current.themeId).toBe("dark");
  });
});
