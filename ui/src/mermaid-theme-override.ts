// Render-time transform that strips a Mermaid diagram author's theme
// directives so the reader's TermAl Markdown theme wins. See
// `docs/features/markdown-themes-and-styles.md` §Mermaid diagram
// theming for the precedence model this implements.
//
// Two author-controlled surfaces need stripping:
//
// 1. Inline `%%{init: …}%%` directives anywhere in the source. Mermaid
//    parses these during `render()` and merges them on top of the
//    config we pass to `initialize()`. Left in place, they override
//    our Markdown-theme palette for any variable they specify.
// 2. YAML frontmatter at the top of the source (``` ---\nkey: value
//    \n--- ```) with `theme:` / `themeVariables:` / `themeCSS:` keys.
//    Supported in newer Mermaid versions as an alternative to the
//    inline directive.
//
// The transform is purely on the string we hand to `mermaid.render`.
// The saved file is untouched; the user sees their preferred theme
// without having to rewrite anyone's diagram source.
//
// Intentional V1 simplifications:
//
// - Regex-based frontmatter parse rather than a full YAML parser. We
//   recognise top-level `theme:` / `themeVariables:` / `themeCSS:`
//   lines; `themeVariables:` + an indented block is recognised by
//   the follow-on indented-line heuristic. Complex YAML (anchors,
//   flow-style mappings split across lines, tagged scalars) is out
//   of scope; if someone hits that edge case we can swap in a real
//   parser.
// - The inline-directive regex matches the entire `%%{init: ...}%%`
//   span greedily across newlines. Mermaid treats these as
//   single-line directives, but some editors soft-wrap them; the
//   pattern tolerates both.

const MERMAID_INIT_DIRECTIVE_PATTERN = /%%\{\s*init\s*:[\s\S]*?\}%%\s*/g;

/**
 * Remove author-controlled theme directives from a Mermaid diagram
 * source. Used when the "Diagram theme override" preference is `on`
 * (the default) so the reader's Markdown theme wins uniformly.
 */
export function stripMermaidAuthorThemeDirectives(code: string): string {
  let next = code.replace(MERMAID_INIT_DIRECTIVE_PATTERN, "");
  next = stripThemeKeysFromLeadingYamlFrontmatter(next);
  return next;
}

function stripThemeKeysFromLeadingYamlFrontmatter(code: string): string {
  // Only the LEADING block (before any diagram content) is valid
  // YAML frontmatter in Mermaid's syntax. Anchor with `^` and bail
  // out immediately if the file does not start with `---`.
  const match = code.match(/^---[ \t]*\n([\s\S]*?)\n---[ \t]*(?:\n|$)/);
  if (!match) {
    return code;
  }

  const original = match[0];
  const body = match[1];
  const lines = body.split("\n");
  const keptLines: string[] = [];
  let skippingBlock = false;

  for (const line of lines) {
    if (skippingBlock) {
      // A themeVariables: / themeCSS: block continues while lines
      // are indented (leading whitespace) and non-empty. The first
      // unindented non-empty line ends the block.
      if (line.length === 0 || /^[ \t]/.test(line)) {
        continue;
      }
      skippingBlock = false;
    }

    const topKeyMatch = /^(theme|themeVariables|themeCSS)[ \t]*:(.*)$/.exec(line);
    if (topKeyMatch) {
      const remainder = topKeyMatch[2].trim();
      if (remainder.length === 0) {
        // Scalar value was on its own line (key followed by newline,
        // indented children below) — skip until we hit something
        // non-indented.
        skippingBlock = true;
      }
      // Inline scalar (`theme: forest`) — just skip the line.
      continue;
    }

    keptLines.push(line);
  }

  const keptBody = keptLines.join("\n").trim();
  const afterFrontmatter = code.slice(original.length);

  if (keptBody.length === 0) {
    // The frontmatter held nothing but theme directives — remove it
    // outright so Mermaid doesn't get a bare `---\n---`.
    return afterFrontmatter;
  }

  return `---\n${keptBody}\n---\n${afterFrontmatter}`;
}

/**
 * Decide, from the `data-diagram-theme-override` attribute on
 * <html>, whether the stripper should run. Reading the attribute
 * keeps the Mermaid rendering path out of the React prop graph:
 * changing the preference causes the NEXT render to pick up the
 * new mode without forcing a re-render of the enclosing tree.
 *
 * Missing / unknown values are treated as `on` — matching the
 * `DEFAULT_DIAGRAM_THEME_OVERRIDE_MODE` in `themes.ts`.
 */
export function readActiveDiagramThemeOverrideMode(): "on" | "off" {
  if (typeof document === "undefined") {
    return "on";
  }
  return document.documentElement.dataset.diagramThemeOverride === "off"
    ? "off"
    : "on";
}

/**
 * Single entry point used by the Mermaid renderer. Applies the
 * stripper when and only when the user has Override mode active.
 */
export function applyActiveMermaidThemeOverride(code: string): string {
  return readActiveDiagramThemeOverrideMode() === "on"
    ? stripMermaidAuthorThemeDirectives(code)
    : code;
}
