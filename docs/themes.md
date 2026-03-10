# Configurable UI Themes

Add user-selectable color themes to TermAl, starting with light/dark and expanding to custom
themes.

## Current State

### Current built-in themes

The UI currently ships seven built-in themes:

- Warm Light
- Darkroom
- Sea Glass
- Terminal
- Violet Night
- Sunset Paper
- Blueprint

The CSS is **partially ready** for theming:

- **15 CSS custom properties** defined in `:root` — colors, fonts, shadows
- **73 usages** of `var(--...)` throughout `styles.css`
- **~35 hardcoded colors** that bypass the variable system (inline `#hex`, `rgba()`, gradients)
- **No dark mode** — `color-scheme: light` is hardcoded, no `prefers-color-scheme` media query
- **No theme selector** in the UI
- **No theme persistence** — no backend or localStorage path for user preferences
- **Background gradients** in `body` and `body::before` use hardcoded colors mixed with variables

### Hardcoded colors that need migration

| Location | Current | Should be |
|----------|---------|-----------|
| `body` background gradient | `#ede3d3` | `var(--paper-gradient-start)` |
| `body::before` line texture | `rgba(42, 30, 16, 0.03)` | `var(--texture-line)` |
| `.diff-block` background | `#1d1712` | `var(--code-bg)` |
| `.diff-block` color | `#f6efdf` | `var(--code-fg)` |
| `.diff-block` green gradient | `#182019, #141714` | `var(--diff-add-bg)` |
| `.diff-block` green text | `#d4efd7` | `var(--diff-add-fg)` |
| `.source-block` background | `#1d1712` | `var(--code-bg)` |
| `.source-block` color | `#f6efdf` | `var(--code-fg)` |
| Error text | `#8e3d25` | `var(--signal-red-muted)` |

---

## Design

### Theme structure

A theme is a named set of CSS custom property values:

```ts
type Theme = {
  id: string;             // "warm-light", "dark", "midnight", etc.
  name: string;           // Display name
  colorScheme: "light" | "dark";
  colors: {
    paper: string;
    paperStrong: string;
    ink: string;
    muted: string;
    line: string;
    panel: string;
    panelStrong: string;
    signalRed: string;
    signalGold: string;
    signalGreen: string;
    signalBlue: string;
    signalRose: string;
    shadow: string;
    // New variables needed for theming
    codeBg: string;
    codeFg: string;
    diffAddBg: string;
    diffAddFg: string;
    diffRemoveBg: string;
    diffRemoveFg: string;
    paperGradientStart: string;
    textureLine: string;
  };
  fonts?: {
    heading?: string;
    body?: string;
    code?: string;
  };
};
```

### Built-in themes

**Phase 1 — ship these two:**

#### Warm Light (current default)

The existing look. Parchment tones, serif headings, warm signal colors. No changes needed
beyond migrating hardcoded values to variables.

#### Dark

```css
[data-theme="dark"] {
  color-scheme: dark;
  --paper: #1a1a1f;
  --paper-strong: #222228;
  --ink: #e0ddd6;
  --muted: #8a8580;
  --line: rgba(255, 255, 255, 0.1);
  --panel: rgba(30, 30, 36, 0.85);
  --panel-strong: rgba(35, 35, 42, 0.92);
  --signal-red: #e07050;
  --signal-gold: #d4a844;
  --signal-green: #4aaa90;
  --signal-blue: #5a9ac0;
  --signal-rose: #cc7080;
  --shadow: 0 24px 60px rgba(0, 0, 0, 0.4);
  --code-bg: #141418;
  --code-fg: #d4d0c8;
  --diff-add-bg: #1a2e1a;
  --diff-add-fg: #a8d8a8;
  --diff-remove-bg: #2e1a1a;
  --diff-remove-fg: #d8a8a8;
  --paper-gradient-start: #16161b;
  --texture-line: rgba(255, 255, 255, 0.02);
}
```

**Phase 2 — optional extras:**

- **Midnight** — deeper blue-black, monospace everything, terminal aesthetic
- **High Contrast** — accessibility-focused, WCAG AAA compliant
- **System** — follow `prefers-color-scheme` automatically

### Theme application

Use a `data-theme` attribute on the root element:

```html
<html data-theme="dark">
```

CSS loads the correct variable set:

```css
:root, [data-theme="warm-light"] {
  /* existing warm light values */
}

[data-theme="dark"] {
  /* dark overrides */
}
```

This is the simplest approach — no JS variable injection, no CSS-in-JS, no build-time
generation. Just attribute selectors and CSS specificity.

### Theme selection UI

Add a theme picker to the sidebar footer or a settings panel:

- Minimal: a toggle button (sun/moon icon) for light/dark
- Full: a dropdown or grid of theme swatches in a settings section
- Keyboard shortcut: Cmd+Shift+T to cycle themes (optional)

### Theme persistence

Two tiers:

1. **localStorage** — instant, no backend needed, per-browser
   ```ts
   localStorage.setItem("termal-theme", "dark");
   ```

2. **Backend settings** (later) — sync across devices via the existing settings API
   ```
   POST /api/settings { "theme": "dark" }
   ```

Start with localStorage. Migrate to backend when user settings become a broader feature.

### System preference detection

Respect `prefers-color-scheme` as the default when no explicit choice is saved:

```ts
function getInitialTheme(): string {
  const saved = localStorage.getItem("termal-theme");
  if (saved) return saved;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "warm-light";
}
```

Listen for system changes so "System" mode stays in sync:

```ts
window.matchMedia("(prefers-color-scheme: dark)")
  .addEventListener("change", (e) => {
    if (currentTheme === "system") {
      applyTheme(e.matches ? "dark" : "warm-light");
    }
  });
```

---

## Implementation Plan

### Phase 1: Variable cleanup + dark mode (3-4 days)

| Task | Effort | Detail |
|------|--------|--------|
| Audit all hardcoded colors in `styles.css` | 0.5 day | ~35 values to find and categorize |
| Add new CSS variables for code blocks, diffs, gradients | 0.5 day | ~10 new variables in `:root` |
| Migrate hardcoded colors to variables | 1 day | Replace each with `var(--name)`, verify nothing breaks visually |
| Add `[data-theme="dark"]` rule block | 0.5 day | Define all variable overrides for dark |
| Add theme toggle to UI | 0.5 day | Button in sidebar footer, applies `data-theme` attribute |
| Add localStorage persistence + system detection | 0.5 day | Read on load, write on change, listen for system changes |

**Deliverable:** working light/dark toggle with persistence.

### Phase 2: Theme infrastructure (2-3 days)

| Task | Effort | Detail |
|------|--------|--------|
| Define `Theme` type and built-in theme registry | 0.5 day | TypeScript types, theme objects |
| Theme picker UI (settings panel or dropdown) | 1 day | Grid of swatches, active indicator, live preview |
| "System" option with media query listener | 0.5 day | Auto-switch on OS theme change |
| Transition animations | 0.5 day | Smooth `background-color`, `color` transitions on theme change |

**Deliverable:** theme picker with 3 options (Warm Light, Dark, System).

### Phase 3: Custom themes + extras (2-3 days, optional)

| Task | Effort | Detail |
|------|--------|--------|
| Additional built-in themes (Midnight, High Contrast) | 1 day | New color sets, visual QA |
| Custom theme editor | 1-2 days | Color pickers for each variable, live preview, export/import JSON |
| Backend persistence via settings API | 0.5 day | `POST /api/settings` with theme field |

---

## Effort Summary

| Phase | Effort | Depends on |
|-------|--------|------------|
| **Phase 1:** Light/dark toggle | 3-4 days | Nothing |
| **Phase 2:** Theme picker + System | 2-3 days | Phase 1 |
| **Phase 3:** Custom themes | 2-3 days | Phase 2, optional |
| **Total** | **3-4 days minimum, 7-10 days full** | |

Phase 1 is self-contained and shippable on its own. The variable cleanup it requires also
benefits every future UI feature (diff viewer, review comments, etc.) since new components
can use the variable system from day one.

---

## Testing

### Visual QA (manual)

- Toggle between light and dark in every view mode (session, prompt, commands, diffs, source)
- Check all message card types render correctly in both themes
- Check sidebar, composer, settings panels, approval cards
- Check split pane dividers and drag handles
- Check scrollbar styling (if customized)
- Verify background gradients and textures adapt

### Automated (Vitest)

```
theme.test.ts

- getInitialTheme returns "warm-light" when no saved preference and system is light
- getInitialTheme returns "dark" when no saved preference and system is dark
- getInitialTheme returns saved theme from localStorage when present
- applyTheme sets data-theme attribute on document root
- applyTheme persists choice to localStorage
- theme registry contains all built-in themes
- all built-in themes define every required color variable
- all built-in themes have a valid colorScheme value
```

### Accessibility

- Check contrast ratios in dark theme against WCAG AA (4.5:1 for text, 3:1 for large text)
- Signal colors must remain distinguishable in both themes
- Focus indicators must be visible in both themes

---

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Hardcoded colors missed during audit | Low | Grep for `#`, `rgb`, `rgba`, `hsl` outside `:root` after migration |
| Dark theme looks bad with existing gradients | Medium | The `body` background gradient and `body::before` texture are the trickiest — design both carefully, test on multiple screens |
| Third-party component styling (react-markdown) | Low | react-markdown inherits `color` and `font` from parent — just ensure wrapper has correct variables |
| Theme flash on load (FOUC) | Low | Apply `data-theme` in a `<script>` in `index.html` before React hydrates, not in a `useEffect` |
