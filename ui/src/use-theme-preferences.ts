/*
Theme preference runtime
------------------------
Owns the live system-color subscription, the session-only Auto override,
application/localStorage mirroring, and the global theme-toggle shortcut.
Workspace-layout persistence and server reconciliation remain in the workspace
layout modules; this hook only exposes state and immediate runtime effects.
*/

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import { isThemeModeToggleShortcut } from "./pane-keyboard";
import {
  applyThemePreference,
  getSystemThemeKind,
  getThemeKind,
  persistThemePreference,
  persistThemePreferences,
  resolveEffectiveThemeId,
  resolveEffectiveThemeKind,
  subscribeToSystemThemeKind,
  type ThemeId,
  type ThemeKind,
  type ThemeMode,
  type ThemePreferences,
} from "./themes";

export type ThemePreferencesState = {
  darkThemeId: ThemeId;
  effectiveThemeKind: ThemeKind;
  lightThemeId: ThemeId;
  returnToAuto: () => void;
  setDarkThemeId: Dispatch<SetStateAction<ThemeId>>;
  setLightThemeId: Dispatch<SetStateAction<ThemeId>>;
  setThemeId: Dispatch<SetStateAction<ThemeId>>;
  setThemeMode: Dispatch<SetStateAction<ThemeMode>>;
  systemThemeKind: ThemeKind;
  themeId: ThemeId;
  themeMode: ThemeMode;
  themeSessionOverride: ThemeKind | null;
  toggleThemeKind: () => void;
};

export function useThemePreferencesState(
  initialPreferences: ThemePreferences,
): ThemePreferencesState {
  const [lightThemeId, setLightThemeId] = useState<ThemeId>(
    initialPreferences.lightThemeId,
  );
  const [darkThemeId, setDarkThemeId] = useState<ThemeId>(
    initialPreferences.darkThemeId,
  );
  const [themeMode, setThemeModeState] = useState<ThemeMode>(
    initialPreferences.themeMode,
  );
  const [systemThemeKind, setSystemThemeKind] = useState<ThemeKind>(() =>
    getSystemThemeKind(),
  );
  const [themeSessionOverride, setThemeSessionOverride] =
    useState<ThemeKind | null>(null);
  const themePreferences = useMemo<ThemePreferences>(
    () => ({ darkThemeId, lightThemeId, themeMode }),
    [darkThemeId, lightThemeId, themeMode],
  );
  const effectiveThemeKind = resolveEffectiveThemeKind(
    themeMode,
    systemThemeKind,
    themeSessionOverride,
  );
  const themeId = resolveEffectiveThemeId(
    themePreferences,
    systemThemeKind,
    themeSessionOverride,
  );

  useEffect(() => subscribeToSystemThemeKind(setSystemThemeKind), []);

  useLayoutEffect(() => {
    applyThemePreference(themeId);
    persistThemePreference(themeId);
    persistThemePreferences(themePreferences);
  }, [themeId, themePreferences]);

  const setThemeId = useCallback<Dispatch<SetStateAction<ThemeId>>>(
    (value) => {
      const nextThemeId = typeof value === "function" ? value(themeId) : value;
      if (getThemeKind(nextThemeId) === "dark") {
        setDarkThemeId(nextThemeId);
      } else {
        setLightThemeId(nextThemeId);
      }
    },
    [themeId],
  );

  const setThemeMode = useCallback<Dispatch<SetStateAction<ThemeMode>>>((value) => {
    setThemeSessionOverride(null);
    setThemeModeState((current) =>
      typeof value === "function" ? value(current) : value,
    );
  }, []);

  const toggleThemeKind = useCallback(() => {
    const nextKind: ThemeKind = effectiveThemeKind === "dark" ? "light" : "dark";
    if (themeMode === "auto") {
      setThemeSessionOverride(nextKind);
      return;
    }

    setThemeModeState(nextKind);
    setThemeSessionOverride(null);
  }, [effectiveThemeKind, themeMode]);

  const returnToAuto = useCallback(() => {
    setThemeModeState("auto");
    setThemeSessionOverride(null);
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      // Editor-local keymaps deliberately win. Monaco prevents its own
      // Cmd/Ctrl+Shift+L command before this window handler runs, while the
      // shell-level shortcut remains available everywhere else.
      if (
        event.defaultPrevented ||
        event.repeat ||
        !isThemeModeToggleShortcut(event)
      ) {
        return;
      }

      event.preventDefault();
      toggleThemeKind();
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [toggleThemeKind]);

  return {
    darkThemeId,
    effectiveThemeKind,
    lightThemeId,
    returnToAuto,
    setDarkThemeId,
    setLightThemeId,
    setThemeId,
    setThemeMode,
    systemThemeKind,
    themeId,
    themeMode,
    themeSessionOverride,
    toggleThemeKind,
  };
}
