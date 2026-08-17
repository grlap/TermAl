# Configurable UI Themes

This document describes the theme system that currently ships in TermAl.

> Related: Markdown rendering has its own in-progress theme axes covered in
> [`features/markdown-themes-and-styles.md`](./features/markdown-themes-and-styles.md).
> The Markdown preference inherits from the UI theme by default but can be
> overridden independently.

## Status

Implemented.

TermAl separates color theme, chrome style, font size, editor font size, and UI
density. Color themes are selected as a light/dark pair. The Light, Dark, or
Auto mode decides which member is active, and Auto follows
`prefers-color-scheme` live. These preferences are runtime switchable and
persisted in `localStorage` and the workspace layout.

## Theme assets

Theme CSS lives in `ui/src/themes/`. The registry lives in `ui/src/themes.ts`.

Current color themes:

- Warm Light
- Gallery White
- Workbench Light
- Silver White
- Porcelain White
- Darkroom
- Code Black
- Obsidian Black
- Oxide Black
- Evergreen Night
- Sea Glass
- Terminal
- Violet Night
- Fjord
- Amber Trace
- Heather
- Sunset Paper
- Blueprint
- Ember
- Frost
- Sakura

Current chrome style options:

- Match Theme
- Editorial
- Studio
- Terminal
- Blueprint

`Match Theme` keeps the visual treatment bundled with the selected color theme.
The other four style presets override chrome treatment independently from the
palette.

## Runtime preferences

The active preferences are stored under:

| Preference | Storage key | Default |
|------------|-------------|---------|
| Active color theme compatibility key | `termal-ui-theme` | resolved member of the pair |
| Light color theme | `termal-ui-theme-light` | `warm-light` |
| Dark color theme | `termal-ui-theme-dark` | `dark` |
| Theme mode | `termal-ui-theme-mode` | `auto` |
| Chrome style | `termal-ui-style` | `theme-default` |
| UI font size | `termal-ui-font-size` | `16` |
| Editor font size | `termal-editor-font-size` | `13` |
| UI density | `termal-ui-density` | `100` |

The DOM application model is:

```html
<html data-theme="warm-light" data-ui-style="theme-default">
```

Each theme CSS file declares `color-scheme: light` or `color-scheme: dark`.
The registry derives a theme's slot from that computed property instead of
maintaining separate metadata. Color themes set CSS variables such as `--paper`, `--ink`, `--line`,
`--panel`, `--signal-blue`, and Monaco-related colors. Chrome style files layer
layout, border, radius, and typography treatments over the selected palette.

## Design rules

- Theme switching should be pure DOM/CSS state. Do not inject generated style
  strings at runtime.
- New color themes should be `.css` files plus one entry in `THEMES`.
- New chrome styles should be `.css` files plus one entry in `STYLES`.
- Theme CSS should prefer existing semantic variables before introducing a new
  variable.
- The selected pair and mode are stored with the workspace layout, while the
  local keys provide the cold-start fallback before workspace hydration.
- `Cmd+Shift+L` (`Ctrl+Shift+L` outside macOS) and the shell button switch the
  effective light/dark member without a reload. In Auto, a manual switch is a
  session-only override until the user returns to Auto. Editor-local keymaps
  take precedence when Monaco handles the same chord; the shell button remains
  available in that context.
- Monaco should continue to derive its colors from the active TermAl palette so
  source and diff views match the rest of the app.

## Adding a Color Theme

1. Add a new `ui/src/themes/<id>.css`.
2. Import it from `ui/src/themes/index.css`.
3. Add a `THEMES` entry in `ui/src/themes.ts` with `id`, `name`, `description`,
   and three swatches.
4. Run `cd ui && npx tsc --noEmit`.
5. Run `cd ui && npx vitest run themes.test.ts`.

## Adding a Chrome Style

1. Add a new `ui/src/themes/style-<id>.css`.
2. Import it from `ui/src/themes/index.css`.
3. Add a `STYLES` entry in `ui/src/themes.ts`.
4. Verify the main workspace, control panel, message cards, Monaco panes, and
   terminal panel with at least one light and one dark color theme.

## Testing

Automated coverage currently checks theme registry invariants and preference
clamping. Visual QA is still manual and should cover:

- all control-panel sections
- session chat, prompt settings, and slash palette
- source and diff Monaco panes
- file/git panels
- terminal panel
- orchestrator template canvas and library
- light and dark palettes
- each chrome style at compact, default, and expanded density
