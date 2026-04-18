// Bootstraps the initial app state from localStorage on cold start.
//
// What this file owns:
//   - `createInitialWorkspaceBootstrap` — the one-shot cold-start
//     hydrator. Given a workspace view id, reads the saved
//     workspace layout (`getStoredWorkspaceLayout`) plus every
//     stored preference slot (theme, style, Markdown theme,
//     Markdown style, diagram override mode / look / palette,
//     font size, editor font size, density, control-panel side),
//     then runs the result through `hydrateControlPanelLayout` so
//     the workspace tree gets a control-panel pane with the
//     saved side if one isn't already present. Fields that are
//     absent from the saved layout fall back to each preference's
//     own default (`getStored*Preference`), which in turn falls
//     back to the hard-coded defaults in `./themes`.
//
// What this file does NOT own:
//   - React state — `App.tsx` calls this once inside a `useRef`
//     initializer on cold start and unpacks the result into
//     individual `useState` slots.
//   - localStorage read primitives themselves — those live in
//     `./themes`, `./workspace-storage`. This function composes
//     them.
//   - The workspace-layout hydration heuristics — those live in
//     `./control-panel-layout`.
//
// Split out of `ui/src/App.tsx`. Same signature and behaviour as
// the inline definition it replaced.

import { hydrateControlPanelLayout } from "./control-panel-layout";
import {
  getStoredDensityPreference,
  getStoredDiagramLookPreference,
  getStoredDiagramPalettePreference,
  getStoredDiagramThemeOverridePreference,
  getStoredEditorFontSizePreference,
  getStoredFontSizePreference,
  getStoredMarkdownStylePreference,
  getStoredMarkdownThemePreference,
  getStoredStylePreference,
  getStoredThemePreference,
  type DiagramLook,
  type DiagramPalette,
  type DiagramThemeOverrideMode,
  type MarkdownStyleId,
  type MarkdownThemeId,
  type StyleId,
  type ThemeId,
} from "./themes";
import {
  getStoredWorkspaceLayout,
  type ControlPanelSide,
} from "./workspace-storage";

export function createInitialWorkspaceBootstrap(workspaceViewId: string) {
  const storedLayout = getStoredWorkspaceLayout(workspaceViewId);
  const controlPanelSide: ControlPanelSide =
    storedLayout?.controlPanelSide ?? "left";
  const themeId: ThemeId = storedLayout?.themeId ?? getStoredThemePreference();
  const styleId: StyleId = storedLayout?.styleId ?? getStoredStylePreference();
  const markdownThemeId: MarkdownThemeId =
    storedLayout?.markdownThemeId ?? getStoredMarkdownThemePreference();
  const markdownStyleId: MarkdownStyleId =
    storedLayout?.markdownStyleId ?? getStoredMarkdownStylePreference();
  const diagramThemeOverrideMode: DiagramThemeOverrideMode =
    storedLayout?.diagramThemeOverrideMode ??
    getStoredDiagramThemeOverridePreference();
  const diagramLook: DiagramLook =
    storedLayout?.diagramLook ?? getStoredDiagramLookPreference();
  const diagramPalette: DiagramPalette =
    storedLayout?.diagramPalette ?? getStoredDiagramPalettePreference();
  const fontSizePx = storedLayout?.fontSizePx ?? getStoredFontSizePreference();
  const editorFontSizePx =
    storedLayout?.editorFontSizePx ?? getStoredEditorFontSizePreference();
  const densityPercent =
    storedLayout?.densityPercent ?? getStoredDensityPreference();
  const workspace = hydrateControlPanelLayout(
    storedLayout?.workspace ?? {
      root: null,
      panes: [],
      activePaneId: null,
    },
    controlPanelSide,
  );

  return {
    controlPanelSide,
    themeId,
    styleId,
    markdownThemeId,
    markdownStyleId,
    diagramThemeOverrideMode,
    diagramLook,
    diagramPalette,
    fontSizePx,
    editorFontSizePx,
    densityPercent,
    workspace,
  };
}
