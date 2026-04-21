// app-preferences-state.ts
//
// Owns: the React state + apply/persist side-effect orchestration
// for App's preference surface — theme, editor style, markdown
// theme/style, diagram theme-override mode, diagram look /
// palette, UI font size, editor font size, density, plus the
// default agent/session settings (Codex sandbox / approval /
// reasoning effort, Claude approval / effort, Cursor mode,
// Gemini approval), and the remote config list seeded from
// resolveAppPreferences(null). All 18 useState + 10 apply/persist
// effects that used to live inline in App.tsx now sit inside
// `useAppPreferencesState`.
//
// Does not own: settings-dialog JSX (stays in App.tsx), clamp
// helpers (called from the settings JSX so they stay imported in
// App.tsx), and the SSE-adoption call-site of
// `resolveAppPreferences(nextState.preferences)` in App.tsx.
//
// Split out of: ui/src/App.tsx (Slice 11 of the App-split plan,
// see docs/app-split-plan.md).

import { useEffect, useLayoutEffect, useState } from "react";
import {
  applyDensityPreference,
  applyDiagramLookPreference,
  applyDiagramPalettePreference,
  applyDiagramThemeOverridePreference,
  applyFontSizePreference,
  applyMarkdownStylePreference,
  applyMarkdownThemePreference,
  applyStylePreference,
  applyThemePreference,
  persistDensityPreference,
  persistDiagramLookPreference,
  persistDiagramPalettePreference,
  persistDiagramThemeOverridePreference,
  persistEditorFontSizePreference,
  persistFontSizePreference,
  persistMarkdownStylePreference,
  persistMarkdownThemePreference,
  persistStylePreference,
  persistThemePreference,
  type DiagramLook,
  type DiagramPalette,
  type DiagramThemeOverrideMode,
  type MarkdownStyleId,
  type MarkdownThemeId,
  type StyleId,
  type ThemeId,
} from "./themes";
import {
  DEFAULT_CLAUDE_APPROVAL_MODE,
  DEFAULT_CLAUDE_EFFORT,
  DEFAULT_CODEX_REASONING_EFFORT,
  resolveAppPreferences,
} from "./session-model-utils";
import type {
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CursorMode,
  GeminiApprovalMode,
  RemoteConfig,
  SandboxMode,
} from "./types";
import type { createInitialWorkspaceBootstrap } from "./initial-workspace-bootstrap";

type InitialWorkspaceBootstrap = ReturnType<
  typeof createInitialWorkspaceBootstrap
>;

export function useAppPreferencesState(
  initialWorkspaceBootstrap: InitialWorkspaceBootstrap,
) {
  const [themeId, setThemeId] = useState<ThemeId>(
    initialWorkspaceBootstrap.themeId,
  );
  const [styleId, setStyleId] = useState<StyleId>(
    initialWorkspaceBootstrap.styleId,
  );
  const [markdownThemeId, setMarkdownThemeId] = useState<MarkdownThemeId>(
    initialWorkspaceBootstrap.markdownThemeId,
  );
  const [markdownStyleId, setMarkdownStyleId] = useState<MarkdownStyleId>(
    initialWorkspaceBootstrap.markdownStyleId,
  );
  const [diagramThemeOverrideMode, setDiagramThemeOverrideMode] =
    useState<DiagramThemeOverrideMode>(
      initialWorkspaceBootstrap.diagramThemeOverrideMode,
    );
  const [diagramLook, setDiagramLook] = useState<DiagramLook>(
    initialWorkspaceBootstrap.diagramLook,
  );
  const [diagramPalette, setDiagramPalette] = useState<DiagramPalette>(
    initialWorkspaceBootstrap.diagramPalette,
  );
  const [fontSizePx, setFontSizePx] = useState<number>(
    initialWorkspaceBootstrap.fontSizePx,
  );
  const [editorFontSizePx, setEditorFontSizePx] = useState<number>(
    initialWorkspaceBootstrap.editorFontSizePx,
  );
  const [densityPercent, setDensityPercent] = useState<number>(
    initialWorkspaceBootstrap.densityPercent,
  );
  const [defaultCodexSandboxMode, setDefaultCodexSandboxMode] =
    useState<SandboxMode>("workspace-write");
  const [defaultCodexApprovalPolicy, setDefaultCodexApprovalPolicy] =
    useState<ApprovalPolicy>("never");
  const [defaultCodexReasoningEffort, setDefaultCodexReasoningEffort] =
    useState<CodexReasoningEffort>(DEFAULT_CODEX_REASONING_EFFORT);
  const [defaultClaudeApprovalMode, setDefaultClaudeApprovalMode] =
    useState<ClaudeApprovalMode>(DEFAULT_CLAUDE_APPROVAL_MODE);
  const [defaultClaudeEffort, setDefaultClaudeEffort] =
    useState<ClaudeEffortLevel>(DEFAULT_CLAUDE_EFFORT);
  const [remoteConfigs, setRemoteConfigs] = useState<RemoteConfig[]>(
    () => resolveAppPreferences(null).remotes,
  );
  const [defaultCursorMode, setDefaultCursorMode] =
    useState<CursorMode>("agent");
  const [defaultGeminiApprovalMode, setDefaultGeminiApprovalMode] =
    useState<GeminiApprovalMode>("default");

  useLayoutEffect(() => {
    applyThemePreference(themeId);
    // Also update the global fallback key so main.tsx can use it for new workspaces
    persistThemePreference(themeId);
  }, [themeId]);

  useLayoutEffect(() => {
    applyStylePreference(styleId);
    persistStylePreference(styleId);
  }, [styleId]);

  useLayoutEffect(() => {
    applyMarkdownThemePreference(markdownThemeId);
    persistMarkdownThemePreference(markdownThemeId);
  }, [markdownThemeId]);

  useLayoutEffect(() => {
    applyMarkdownStylePreference(markdownStyleId);
    persistMarkdownStylePreference(markdownStyleId);
  }, [markdownStyleId]);

  useLayoutEffect(() => {
    applyDiagramThemeOverridePreference(diagramThemeOverrideMode);
    persistDiagramThemeOverridePreference(diagramThemeOverrideMode);
  }, [diagramThemeOverrideMode]);

  useLayoutEffect(() => {
    applyDiagramLookPreference(diagramLook);
    persistDiagramLookPreference(diagramLook);
  }, [diagramLook]);

  useLayoutEffect(() => {
    applyDiagramPalettePreference(diagramPalette);
    persistDiagramPalettePreference(diagramPalette);
  }, [diagramPalette]);

  useLayoutEffect(() => {
    applyFontSizePreference(fontSizePx);
    persistFontSizePreference(fontSizePx);
  }, [fontSizePx]);

  useLayoutEffect(() => {
    applyDensityPreference(densityPercent);
    persistDensityPreference(densityPercent);
  }, [densityPercent]);

  useEffect(() => {
    persistEditorFontSizePreference(editorFontSizePx);
  }, [editorFontSizePx]);

  return {
    themeId,
    setThemeId,
    styleId,
    setStyleId,
    markdownThemeId,
    setMarkdownThemeId,
    markdownStyleId,
    setMarkdownStyleId,
    diagramThemeOverrideMode,
    setDiagramThemeOverrideMode,
    diagramLook,
    setDiagramLook,
    diagramPalette,
    setDiagramPalette,
    fontSizePx,
    setFontSizePx,
    editorFontSizePx,
    setEditorFontSizePx,
    densityPercent,
    setDensityPercent,
    defaultCodexSandboxMode,
    setDefaultCodexSandboxMode,
    defaultCodexApprovalPolicy,
    setDefaultCodexApprovalPolicy,
    defaultCodexReasoningEffort,
    setDefaultCodexReasoningEffort,
    defaultClaudeApprovalMode,
    setDefaultClaudeApprovalMode,
    defaultClaudeEffort,
    setDefaultClaudeEffort,
    defaultCursorMode,
    setDefaultCursorMode,
    defaultGeminiApprovalMode,
    setDefaultGeminiApprovalMode,
    remoteConfigs,
    setRemoteConfigs,
  } as const;
}
