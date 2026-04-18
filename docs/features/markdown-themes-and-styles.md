# Feature Brief: Markdown Themes And Styles

## Status

Phases 1–4 shipped. The Settings panel exposes a "Markdown" tab with
a Markdown-theme picker and a Markdown-style picker; selections
persist across reloads (localStorage) and follow the same layout
persistence pipeline as the UI theme preferences. Three Markdown
themes (`github-light`, `github-dark`, `terminal`) and two Markdown
styles (`document`, `compact`) ship alongside the `match-ui` default.
Mermaid's `themeVariables` palette is routed through the active
Markdown theme so flowcharts follow the reading theme rather than
the workspace chrome. Complements the existing app-wide theme system
described in [`../themes.md`](../themes.md).

## Problem

Rendered Markdown currently inherits the app's theme uniformly: the
chrome and the document content share one palette, one chrome style,
and one set of colors for code blocks, links, quotes, tables, Mermaid
diagrams, and KaTeX math. That's fine as a default, but it rules out
a few reasonable preferences:

- A user who wants a typographic-first reading style (serif, generous
  leading, GitHub-style headings) for rendered documents while keeping
  a compact studio-style chrome for the workspace.
- A user who wants a GitHub-style code-block look inside Markdown cards
  while the rest of the UI stays on the warm-light palette.
- A user with a specific Mermaid color palette in mind, distinct from
  the palette they like for the workspace background.

Today the only way to get any of these is to edit theme CSS by hand and
rebuild. We already ship 18 palettes and 5 chrome styles for the
workspace; Markdown deserves the same treatment for its own surfaces.

## Goals

- Add a **Markdown theme** axis and a **Markdown style** axis to the
  Settings panel, orthogonal to the existing UI theme and UI style.
- Provide a curated set of Markdown-specific presets that cover the
  obvious reading modes (GitHub-like, academic-paper-like, compact,
  terminal). Each preset is a CSS file plus a registry entry, mirroring
  the app theme workflow.
- Apply the preset to every rendered-Markdown surface: message cards,
  diff preview rendered side, source panel preview, and inline view
  zones inside Monaco (Mermaid / KaTeX).
- Route Mermaid and KaTeX theming through the Markdown theme (not the
  UI theme) so the diagram colors and math colors follow the reading
  theme.
- Default to `Match UI theme` so existing users keep the current
  behavior unchanged.

## Non-goals (V1)

- Arbitrary user-supplied CSS. A Markdown theme is a registered preset,
  not a free-form stylesheet. Free-form CSS would require sandboxing
  against selectors that could reach workspace chrome.
- Per-document overrides (pin a specific theme to a specific file).
  Global preference only for V1.
- Per-file Markdown theme overrides in frontmatter.
- Cross-browser sync. The preference is localStorage-local, matching
  the rest of the theme system.
- Editing theme presets inside TermAl. Ship presets via the registry;
  contributions land as commits.

## Product model

### Preferences

Two new localStorage keys, mirroring the existing theme preferences:

| Preference | Storage key | Default |
|------------|-------------|---------|
| Markdown theme | `termal-markdown-theme` | `match-ui` |
| Markdown style | `termal-markdown-style` | `match-ui` |

`match-ui` is the default token that means "inherit from the active
UI theme / UI style". Explicitly selecting a Markdown theme or style
overrides just the Markdown surface.

### DOM application

Applied to the same `<html>` element as the existing theme attributes,
so CSS selectors can cascade cleanly:

```html
<html
  data-theme="warm-light"
  data-ui-style="theme-default"
  data-markdown-theme="github-light"
  data-markdown-style="document"
>
```

When the preference is `match-ui`, the attribute is omitted (or set to
`match-ui`) and Markdown surfaces fall back to the same variables the
UI theme provides. When it's overridden, the Markdown preset file
supplies its own variable overrides under
`[data-markdown-theme="github-light"]` scoped to the Markdown roots.

### Scoping

The Markdown theme applies to:

- `.markdown-copy` (rendered-Markdown roots inside source + diff panels)
- `.markdown-card` (rendered-Markdown inside message cards)
- `.mermaid-diagram-frame` iframe srcDoc (via the Mermaid theme
  variables + themeCSS path already in `message-cards.tsx`)
- KaTeX output via `rehype-katex` — the math stylesheet uses the
  Markdown theme's text color variables

The Markdown theme does **not** apply to Monaco editors (they follow
the UI theme to stay visually consistent with the surrounding
workspace) or to workspace chrome.

### What a preset controls

| Axis | Example variables |
|------|-------------------|
| Typography | `--markdown-font-family`, `--markdown-heading-font-family`, `--markdown-heading-scale`, `--markdown-paragraph-leading` |
| Headings | `--markdown-h1-weight`, `--markdown-h1-size`, `--markdown-h1-color`, same for h2–h6 |
| Links | `--markdown-link-color`, `--markdown-link-hover`, `--markdown-link-underline` |
| Code | `--markdown-code-bg`, `--markdown-code-fg`, `--markdown-code-border` |
| Code blocks | `--markdown-code-block-bg`, syntax-highlighting class overrides |
| Quotes | `--markdown-quote-border`, `--markdown-quote-bg`, `--markdown-quote-color` |
| Tables | `--markdown-table-border`, `--markdown-table-header-bg`, `--markdown-table-row-zebra` |
| Lists | `--markdown-bullet-color`, `--markdown-list-indent` |
| Mermaid | `themeVariables` + `themeCSS` passed to `mermaid.initialize` |
| KaTeX | text color, delimiter color for inline/block math wrappers |

Style presets (the second axis) control layout-level treatments:
line-length caps, spacing rhythm, heading separators, table density.

### Initial preset catalogue

Curated small set for V1; each one is a CSS file under
`ui/src/themes/markdown/<id>.css` plus a `MARKDOWN_THEMES` entry in
`ui/src/themes.ts`.

Markdown themes:

- `match-ui` (default)
- `github-light`
- `github-dark`
- `terminal`
- `newspaper`
- `solarized-light`
- `solarized-dark`

Markdown styles:

- `match-ui` (default)
- `document` (wide margins, serif, newspaper-ish leading)
- `compact` (tighter heading margins, denser tables)
- `book` (narrow measure, centered, academic)

## Mermaid diagram theming

Mermaid has its own theme and style mechanism, independent from
TermAl's Markdown theme. A diagram author can pick one of Mermaid's
built-in theme presets (`default`, `dark`, `forest`, `neutral`,
`base`) and can further customise individual palette variables,
either via:

- an inline init directive at the top of the diagram source, e.g.
  `%%{init: {"theme": "forest"}}%%` or
  `%%{init: {"themeVariables": {"primaryColor": "#ffcc00"}}}%%`; or
- YAML frontmatter block at the top of the fenced diagram source
  (newer Mermaid releases); or
- Mermaid's automatic `default` / `dark` picks based on the
  `darkMode` flag TermAl already sets from the Monaco editor
  appearance.

TermAl's Markdown theme adds a third layer that the user controls
from Settings. The three inputs have to compose in a predictable
order.

### Layering model

Precedence from lowest to highest:

1. **Mermaid's built-in theme defaults.** The fallback when no
   TermAl override and no author override apply. Historically this
   is `default` in light mode, `dark` in dark mode.
2. **TermAl's Markdown theme overrides.** When the user has picked
   a Markdown theme other than `match-ui`, the
   `TERMAL_MERMAID_THEME_VARIABLES_BY_MARKDOWN_THEME` lookup in
   `ui/src/message-cards.tsx` contributes palette variables
   (`primaryColor`, `primaryBorderColor`, `lineColor`, …) that
   align the diagram with the active prose theme. `match-ui`
   contributes no palette overrides and is a no-op by design.
3. **Diagram author overrides** (`%%{init: …}%%` or frontmatter).
   By convention these win, because the author had a specific
   intent — e.g. a product screenshot or a deck that must look a
   certain way regardless of the reader's Markdown theme.

The current implementation respects the author-override-wins rule
only incidentally: TermAl calls `mermaid.initialize(config)` before
each render, and Mermaid's init directive is then parsed out of the
source during `render()` and applied on top of the initial config.
The behaviour is stable but undocumented.

### User control: Respect vs. Force

V1 defaults to **Respect mode**: author directives win. If a
diagram says `%%{init: {"theme": "forest"}}%%`, the user's
Markdown-theme palette overrides are ignored for that diagram.
This matches the precedence layering above and gives authors
predictable rendering across readers.

A **Force mode** preference can ship later for users who prefer
their Markdown theme applied uniformly, even over explicit author
choices. Proposed surfacing:

- A third row in the Markdown section of Settings, under the theme
  and style pickers, labelled "Override diagram themes" with a
  two-state toggle (`Off` / `On`).
- `Off` (default): keep Respect mode. Author directives pass
  through.
- `On`: before handing the diagram source to `mermaid.render`, strip
  the `%%{init: …}%%` directive and remove any `theme:` /
  `themeVariables:` entries from a YAML frontmatter block. The
  resulting source is rendered under the user's selected Markdown-
  theme palette only.
- The stripping is purely a render-time transform of the input
  string; the saved file stays untouched.

Force mode is a per-user preference, not per-document. Document-
level opt-outs (a fence attribute like ` ```mermaid {respect} `)
are a follow-up option if Force mode by itself proves too blunt.

### Implementation notes

- The registry entries for each Markdown theme live under the
  `TERMAL_MERMAID_THEME_VARIABLES_BY_MARKDOWN_THEME` map. Adding
  a new Markdown theme requires one new entry there; forgetting
  to add the entry gracefully falls through to the empty object
  and diagrams render under Mermaid's built-in defaults.
- `match-ui` is intentionally empty (no overrides). Diagrams
  rendered under `match-ui` behave exactly as they did before
  this feature landed — critical for the "no visual change by
  default" promise from Phase 1.
- The lookup is read from `document.documentElement.dataset.
  markdownTheme` at render time, not passed as a prop. This is
  deliberate: it keeps Mermaid's rendering path out of the React
  prop graph, so changing the Markdown theme makes the **next**
  diagram render pick up the new palette without forcing a
  re-render of the enclosing React tree.

### Non-goals (V1)

- A Mermaid-specific theme picker separate from the Markdown
  theme picker. Mermaid's palette follows the Markdown theme;
  users who want Mermaid-specific tuning use the author directive
  inside their diagram source.
- Per-diagram UI to flip Respect / Force. That's a Force-mode
  follow-up if demand surfaces.
- CSS-level overrides through `themeCSS` per Markdown theme.
  V1 uses `themeVariables` only, which is enough to re-colour
  nodes, edges, and labels. `themeCSS` stays at the TermAl-wide
  level (shared `TERMAL_MERMAID_THEME_CSS` for structural rules
  like edge-label pill shape).

## Cross-surface consistency

- Settings panel surfaces the four independent axes as rows: UI theme,
  UI style, Markdown theme, Markdown style.
- Each row shows the current selection + a preview swatch set.
- Previews render a small canned Markdown snippet (heading +
  paragraph + inline code + link + mini Mermaid) using the prospective
  preset without actually applying it globally.
- Switching is pure DOM/CSS — no React remount, no Mermaid re-init
  for the whole workspace, only a re-render of the surfaces that read
  the Markdown variables.

## Interaction with existing systems

- [`themes.md`](../themes.md) keeps its current scope (UI palette + UI
  chrome). This new feature extends the registry model rather than
  replacing it.
- [`markdown-document-view.md`](./markdown-document-view.md) — the
  rendered Markdown diff editor already targets `.markdown-copy` for
  styling; the new theme attaches under the same root.
- [`source-renderers.md`](./source-renderers.md) — Mermaid / KaTeX
  rendering lives here. The theme variables Mermaid currently receives
  (`TERMAL_MERMAID_THEME_VARIABLES` in `ui/src/message-cards.tsx`) move
  from hardcoded to a lookup keyed on the active Markdown theme.
- [`editor-buffer-persistence.md`](./editor-buffer-persistence.md) —
  the Markdown theme preference, like UI theme, is a browser-local
  preference stored separately from the per-tab buffer.

## Phased delivery plan

### Phase 1: Infrastructure

- Add `MARKDOWN_THEMES` and `MARKDOWN_STYLES` registries in
  `ui/src/themes.ts` with the `match-ui` entries only.
- Plumb `termal-markdown-theme` and `termal-markdown-style` through
  the existing preference reader/writer, including the layout persist
  path.
- Apply `data-markdown-theme` / `data-markdown-style` to `<html>`.
- Ship a `match-ui.css` that introduces the Markdown variable names
  as aliases to existing UI theme variables, so switching from the
  old behavior to the new one is a no-op visually.

### Phase 2: Settings UI

- Add two rows to the Settings panel under a "Markdown" section.
- Preview swatches that render a canned snippet per option.
- Persist selection, broadcast across open tabs via the storage event.

### Phase 3: First presets

- Ship `github-light`, `github-dark`, and `terminal` as the initial
  non-default Markdown themes.
- Ship `document` and `compact` as the initial non-default Markdown
  styles.
- Each preset overrides the Markdown variables introduced in Phase 1.

### Phase 4: Mermaid + KaTeX integration

- Re-key `buildTermalMermaidConfig` on the active Markdown theme so
  the Mermaid `themeVariables` and `themeCSS` are preset-specific.
- Route KaTeX delimiter / math color variables through the same
  Markdown theme.
- Re-render affected frames when the preference changes (Mermaid
  needs a full re-init; KaTeX output is already pure CSS).

## Testing

- Unit: extend `ui/src/themes.test.ts` to cover the Markdown registry
  invariants (every preset has a CSS file, every preset id is unique,
  `match-ui` is always present).
- Unit: `MarkdownContent.test.tsx` — render a doc under at least two
  distinct `data-markdown-theme` values and assert variable-driven
  class changes reach the expected elements.
- Unit: assert `buildTermalMermaidConfig` output depends on the active
  Markdown theme, not the UI theme.
- Visual QA: cycle each Markdown theme against each Markdown style,
  confirm readability across the canned snippet set.

## Open questions

- Should the Markdown style registry be flat (one list mixing layout
  treatments) or cross-cutting (layout vs. typography vs. spacing
  separately)? Flat is simpler for V1.
- When a user selects a Markdown theme whose contrast is poor against
  the current UI palette (dark Markdown theme inside a light
  workspace), do we warn or just render? Render, with a future
  "theme linter" hook in Settings.
- Do we expose the canned preview snippet as a settings feature
  (useful for screenshots, theme authoring) or keep it internal?
  Internal for V1.
- Syntax-highlighting themes are currently tied to the UI theme via
  `highlight.js`'s CSS. Do we layer a second highlighting theme under
  the Markdown theme, or use the UI theme's? Use the UI theme's for
  V1; revisit if a user wants GitHub code colors inside a dark
  workspace.
