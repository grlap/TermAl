// Owns the dependency-free Light/Dark/Auto state machine used by the static
// website demo. DOM rendering remains in script.js; this module keeps system
// color-scheme changes testable without a browser or application runtime.
(function installThemeDemoState(globalScope) {
  "use strict";

  const VALID_MODES = new Set(["light", "dark", "auto"]);

  function resolveThemePreviewKind(mode, systemPrefersDark) {
    return mode === "auto" ? (systemPrefersDark ? "dark" : "light") : mode;
  }

  function createThemeModeController({
    initialMode = "auto",
    mediaQuery,
    onPreviewKindChange,
  }) {
    if (!VALID_MODES.has(initialMode)) {
      throw new Error(`Unsupported theme mode: ${initialMode}`);
    }

    let mode = initialMode;
    const getPreviewKind = () =>
      resolveThemePreviewKind(mode, Boolean(mediaQuery.matches));
    const publish = () => onPreviewKindChange(getPreviewKind());
    const handleSystemChange = () => {
      if (mode === "auto") publish();
    };

    if (typeof mediaQuery.addEventListener === "function") {
      mediaQuery.addEventListener("change", handleSystemChange);
    } else if (typeof mediaQuery.addListener === "function") {
      mediaQuery.addListener(handleSystemChange);
    }

    return {
      dispose() {
        if (typeof mediaQuery.removeEventListener === "function") {
          mediaQuery.removeEventListener("change", handleSystemChange);
        } else if (typeof mediaQuery.removeListener === "function") {
          mediaQuery.removeListener(handleSystemChange);
        }
      },
      getMode() {
        return mode;
      },
      getPreviewKind,
      setMode(nextMode) {
        if (!VALID_MODES.has(nextMode)) {
          throw new Error(`Unsupported theme mode: ${nextMode}`);
        }
        mode = nextMode;
        publish();
      },
    };
  }

  const api = {
    createThemeModeController,
    resolveThemePreviewKind,
  };
  globalScope.TermalThemeDemoState = api;
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  }
})(typeof globalThis === "undefined" ? this : globalThis);
