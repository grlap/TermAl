import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installMonacoCancellationRejectionFilter } from "./monaco-cancellation-filter";
import {
  applyDensityPreference,
  applyFontSizePreference,
  applyStylePreference,
  applyThemePreference,
  getStoredDensityPreference,
  getStoredFontSizePreference,
  getStoredStylePreference,
  getStoredThemePreferences,
  getSystemThemeKind,
  refreshThemeKindCacheFromDocument,
  resolveEffectiveThemeId,
} from "./themes";
import { ensureWorkspaceViewId, getStoredWorkspaceLayout } from "./workspace-storage";
import "./themes/index.css";
import "./styles.css";

installMonacoCancellationRejectionFilter();
refreshThemeKindCacheFromDocument();

// Read UI settings from the per-workspace localStorage cache when available,
// falling back to the global preference keys for workspaces that haven't saved yet.
const earlyWorkspaceLayout = getStoredWorkspaceLayout(ensureWorkspaceViewId());
const earlyThemePreferences = getStoredThemePreferences({
  lightThemeId: earlyWorkspaceLayout?.lightThemeId,
  darkThemeId: earlyWorkspaceLayout?.darkThemeId,
  themeMode: earlyWorkspaceLayout?.themeMode,
});
applyThemePreference(
  resolveEffectiveThemeId(earlyThemePreferences, getSystemThemeKind(), null),
);
applyStylePreference(earlyWorkspaceLayout?.styleId ?? getStoredStylePreference());
applyFontSizePreference(earlyWorkspaceLayout?.fontSizePx ?? getStoredFontSizePreference());
applyDensityPreference(earlyWorkspaceLayout?.densityPercent ?? getStoredDensityPreference());

// Preload Monaco editor chunks in the background so the first file/diff open is instant.
// The lazy() calls in SourcePanel/DiffPanel still gate rendering, but the network fetch
// starts immediately rather than waiting for the user to open a file tab.
void import("./MonacoCodeEditor");
void import("./MonacoDiffEditor");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
