// Preferences-tab registry.
//
// What this file owns:
//   - The `PreferencesTabId` union that enumerates every section in
//     the Settings dialog (Themes, Markdown, Editor & UI, …).
//   - The `PREFERENCES_TABS` array: id + human-facing label for the
//     tab bar, in the order they render.
//
// What this file does NOT own:
//   - Tab-bar rendering (see `./SettingsTabBar.tsx`).
//   - Per-tab panel content (each lives in its own preferences
//     panel module; the Settings dialog composes them).
//   - The currently-active tab state — that stays in `App.tsx`
//     alongside the other Settings-dialog state.
//
// Split out of `ui/src/App.tsx` as the first step of the planned
// App.tsx -> preferences/* split. Kept deliberately small so the
// extraction is a pure type + constant move with no behaviour
// change.

export type PreferencesTabId =
  | "themes"
  | "markdown"
  | "appearance"
  | "remotes"
  | "telegram"
  | "orchestrators"
  | "codex-prompts"
  | "claude-approvals"
  | "cursor"
  | "gemini";

export const PREFERENCES_TABS: ReadonlyArray<{
  id: PreferencesTabId;
  label: string;
}> = [
  { id: "themes", label: "Themes" },
  { id: "markdown", label: "Markdown" },
  { id: "appearance", label: "Editor & UI" },
  { id: "remotes", label: "Remotes" },
  { id: "telegram", label: "Telegram" },
  { id: "orchestrators", label: "Orchestrators" },
  { id: "codex-prompts", label: "Codex" },
  { id: "claude-approvals", label: "Claude" },
  { id: "cursor", label: "Cursor" },
  { id: "gemini", label: "Gemini" },
];
