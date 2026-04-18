import { describe, expect, it, beforeEach } from "vitest";
import {
  applyActiveMermaidThemeOverride,
  readActiveDiagramThemeOverrideMode,
  stripMermaidAuthorThemeDirectives,
} from "./mermaid-theme-override";

describe("stripMermaidAuthorThemeDirectives", () => {
  it("leaves diagrams with no author directives untouched", () => {
    const source = ["flowchart TD", "  A --> B", "  B --> C"].join("\n");
    expect(stripMermaidAuthorThemeDirectives(source)).toBe(source);
  });

  it("strips a leading `%%{init: ...}%%` directive", () => {
    const source = [
      '%%{init: {"theme": "forest"}}%%',
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    const next = stripMermaidAuthorThemeDirectives(source);
    expect(next).not.toContain("init");
    expect(next).toContain("flowchart TD");
    expect(next).toContain("A --> B");
  });

  it("strips an init directive embedded mid-source", () => {
    const source = [
      "flowchart TD",
      '%%{init: {"themeVariables": {"primaryColor": "#fc0"}}}%%',
      "  A --> B",
    ].join("\n");
    const next = stripMermaidAuthorThemeDirectives(source);
    expect(next).not.toContain("init");
    expect(next).not.toContain("primaryColor");
    expect(next).toContain("flowchart TD");
    expect(next).toContain("A --> B");
  });

  it("strips `theme:` scalar from YAML frontmatter", () => {
    const source = [
      "---",
      "title: My diagram",
      "theme: dark",
      "---",
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    const next = stripMermaidAuthorThemeDirectives(source);
    expect(next).toContain("title: My diagram");
    expect(next).not.toContain("theme: dark");
    expect(next).toContain("flowchart TD");
  });

  it("strips `themeVariables:` block including nested children", () => {
    const source = [
      "---",
      "title: My diagram",
      "themeVariables:",
      "  primaryColor: '#ffcc00'",
      "  lineColor: '#333'",
      "---",
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    const next = stripMermaidAuthorThemeDirectives(source);
    expect(next).toContain("title: My diagram");
    expect(next).not.toContain("themeVariables");
    expect(next).not.toContain("primaryColor");
    expect(next).not.toContain("lineColor");
    expect(next).toContain("flowchart TD");
  });

  it("removes frontmatter entirely when it contains only theme keys", () => {
    const source = [
      "---",
      "theme: dark",
      "themeVariables:",
      "  primaryColor: '#fc0'",
      "---",
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    const next = stripMermaidAuthorThemeDirectives(source);
    expect(next.startsWith("---")).toBe(false);
    expect(next).toContain("flowchart TD");
    expect(next).toContain("A --> B");
  });

  it("leaves non-leading `---` separators alone", () => {
    // A `---` that is not the first line is a Mermaid arrow, not a
    // YAML frontmatter fence. Avoid stripping anything.
    const source = [
      "flowchart TD",
      "  A --> B",
      "  ---",
      "  C --> D",
    ].join("\n");
    expect(stripMermaidAuthorThemeDirectives(source)).toBe(source);
  });
});

describe("applyActiveMermaidThemeOverride", () => {
  beforeEach(() => {
    document.documentElement.removeAttribute("data-diagram-theme-override");
  });

  it("defaults to Override mode when the attribute is missing", () => {
    const source = [
      '%%{init: {"theme": "forest"}}%%',
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    const next = applyActiveMermaidThemeOverride(source);
    expect(next).not.toContain("init");
    expect(readActiveDiagramThemeOverrideMode()).toBe("on");
  });

  it("strips author directives when the attribute reads `on`", () => {
    document.documentElement.dataset.diagramThemeOverride = "on";
    const source = [
      '%%{init: {"theme": "forest"}}%%',
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    const next = applyActiveMermaidThemeOverride(source);
    expect(next).not.toContain("init");
    expect(next).toContain("flowchart TD");
  });

  it("passes the source through unchanged when the attribute reads `off`", () => {
    document.documentElement.dataset.diagramThemeOverride = "off";
    const source = [
      '%%{init: {"theme": "forest"}}%%',
      "flowchart TD",
      "  A --> B",
    ].join("\n");
    expect(applyActiveMermaidThemeOverride(source)).toBe(source);
    expect(readActiveDiagramThemeOverrideMode()).toBe("off");
  });
});
