import {
  countMathExpressions,
  countMermaidFences,
  detectRenderableRegions,
  hasRenderableRegions,
  isMathFenceLanguage,
  isMermaidFenceLanguage,
  MAX_MATH_EXPRESSIONS_PER_DOCUMENT,
  MAX_MERMAID_DIAGRAMS_PER_DOCUMENT,
  MAX_MERMAID_SOURCE_CHARS,
  type SourceRenderContext,
  type SourceRenderableRegion,
} from "./source-renderers";

function markdownContext(
  content: string,
  overrides: Partial<SourceRenderContext> = {},
): SourceRenderContext {
  return {
    path: overrides.path ?? "doc.md",
    language: overrides.language ?? "markdown",
    content,
    mode: overrides.mode ?? "source",
  };
}

describe("source-renderers: budget constants", () => {
  it("exports the documented render-budget ceilings", () => {
    // These match the values in `docs/features/source-renderers.md`.
    // Pinning them prevents a silent drift when future renderer work
    // adjusts one constant without cross-checking the spec.
    expect(MAX_MERMAID_SOURCE_CHARS).toBe(50_000);
    expect(MAX_MERMAID_DIAGRAMS_PER_DOCUMENT).toBe(20);
    expect(MAX_MATH_EXPRESSIONS_PER_DOCUMENT).toBe(100);
  });
});

describe("source-renderers: fence language predicates", () => {
  it("recognizes the documented Mermaid fence aliases", () => {
    expect(isMermaidFenceLanguage("mermaid")).toBe(true);
    expect(isMermaidFenceLanguage("MERMAID")).toBe(true);
    expect(isMermaidFenceLanguage("  mermaid  ")).toBe(true);
    expect(isMermaidFenceLanguage("flow")).toBe(false);
    expect(isMermaidFenceLanguage("")).toBe(false);
    expect(isMermaidFenceLanguage(null)).toBe(false);
  });

  it("recognizes the four documented math fence aliases", () => {
    expect(isMathFenceLanguage("math")).toBe(true);
    expect(isMathFenceLanguage("latex")).toBe(true);
    expect(isMathFenceLanguage("tex")).toBe(true);
    expect(isMathFenceLanguage("katex")).toBe(true);
    expect(isMathFenceLanguage("MATH")).toBe(true);
    expect(isMathFenceLanguage("mathematica")).toBe(false);
    expect(isMathFenceLanguage(null)).toBe(false);
  });
});

describe("source-renderers: count helpers", () => {
  it("counts Mermaid fences, ignoring non-Mermaid fenced blocks", () => {
    const md = [
      "# Title",
      "",
      "```mermaid",
      "flowchart TD",
      "  A --> B",
      "```",
      "",
      "```rust",
      "fn hello() {}",
      "```",
      "",
      "```mermaid",
      "sequenceDiagram",
      "  Alice->>Bob: Hi",
      "```",
    ].join("\n");
    expect(countMermaidFences(md)).toBe(2);
  });

  it("counts inline and block math outside code fences", () => {
    const md = [
      "Inline: $a + b$ and $c = d$",
      "",
      "Block:",
      "",
      "$$",
      "x = 1",
      "$$",
      "",
      "```",
      "echo $HOME",
      "```",
    ].join("\n");
    // Two inline + one block = 3.
    expect(countMathExpressions(md)).toBe(3);
  });

  it("does not count `$` inside fenced code as math", () => {
    const md = ["```bash", "echo $HOME $PATH $USER", "```"].join("\n");
    expect(countMathExpressions(md)).toBe(0);
  });

  it("counts two consecutive multi-line `$$...$$` blocks separated by a blank line", () => {
    // Pins the state-machine toggle in `countMathExpressions`: a
    // trailing `$$` that closes a block, followed by a new opening
    // `$$` on a later line, must re-open the counter so the two
    // blocks land as 2. A regression that left the state "inside
    // block" after the close would report 1 (one long block) or 0
    // (ignored second opener).
    const md = ["$$", "x = 1", "$$", "", "$$", "y = 2", "$$"].join("\n");
    expect(countMathExpressions(md)).toBe(2);
  });

  it("does not count `$$` inside a fenced code block as math", () => {
    // Complements the single-`$` inside-fence test above: the existing
    // coverage only proves bash-style variables are ignored. A
    // regression that special-cased double-dollar before consulting
    // the fence state would incorrectly count these as block math.
    const md = ["```", "$$", "not math", "$$", "```"].join("\n");
    expect(countMathExpressions(md)).toBe(0);
  });

  it("counts same-line `$$...$$` pairs as block math", () => {
    // Pins the `sameLineBlocks` branch — both `$$` openers and
    // closers appear on a single prose line, so the counter picks
    // them up outside the multi-line block path.
    expect(
      countMathExpressions("Block: $$x=1$$ and $$y=2$$ both."),
    ).toBe(2);
  });

  it("returns zero for documents with no math or fences", () => {
    expect(countMathExpressions("A plain paragraph.")).toBe(0);
    expect(countMermaidFences("A plain paragraph.")).toBe(0);
  });
});

describe("detectRenderableRegions: Markdown files", () => {
  it("returns nothing for plain Markdown with no diagrams or math", () => {
    const regions = detectRenderableRegions(
      markdownContext("# Heading\n\nJust prose here."),
    );
    expect(regions).toEqual([]);
    expect(hasRenderableRegions(markdownContext("# Heading\n\nJust prose here."))).toBe(false);
  });

  it("locates Mermaid fences with 1-based source line ranges", () => {
    const md = [
      "# Title", // line 1
      "", // 2
      "```mermaid", // 3
      "flowchart TD", // 4
      "  A --> B", // 5
      "```", // 6
      "", // 7
      "After.", // 8
    ].join("\n");
    const regions = detectRenderableRegions(markdownContext(md));
    expect(regions).toHaveLength(1);
    const region = regions[0] as SourceRenderableRegion;
    expect(region.renderer).toBe("mermaid");
    expect(region.sourceStartLine).toBe(3);
    expect(region.sourceEndLine).toBe(6);
    // sourceText spans the opening + body + closing lines.
    expect(region.sourceText).toBe(
      "```mermaid\nflowchart TD\n  A --> B\n```",
    );
    // displayText is the body WITHOUT fence markers.
    expect(region.displayText).toBe("flowchart TD\n  A --> B");
    expect(region.editable).toBe(true);
  });

  it("keeps Mermaid region ids stable when lines are inserted above a fence", () => {
    const before = [
      "# Title",
      "",
      "```mermaid",
      "flowchart TD",
      "  A --> B",
      "```",
    ].join("\n");
    const after = [
      "# Title",
      "",
      "Inserted paragraph.",
      "",
      "```mermaid",
      "flowchart TD",
      "  A --> B",
      "```",
    ].join("\n");

    expect(detectRenderableRegions(markdownContext(after))[0]?.id).toBe(
      detectRenderableRegions(markdownContext(before))[0]?.id,
    );
  });

  it("deduplicates same-body Mermaid region ids by same-body ordinal", () => {
    const md = [
      "```mermaid",
      "flowchart TD",
      "  A --> B",
      "```",
      "",
      "```mermaid",
      "flowchart TD",
      "  A --> B",
      "```",
    ].join("\n");

    const ids = detectRenderableRegions(markdownContext(md)).map(
      (region) => region.id,
    );

    expect(ids).toHaveLength(2);
    expect(ids[0]).toMatch(/^mermaid:0:/);
    expect(ids[1]).toMatch(/^mermaid:1:/);
    expect(ids[0]?.replace(/^mermaid:\d+:/, "mermaid:")).toBe(
      ids[1]?.replace(/^mermaid:\d+:/, "mermaid:"),
    );
  });

  it("locates `math` / `latex` / `tex` / `katex` fences as math regions", () => {
    const md = [
      "Opening.", // 1
      "", // 2
      "```math", // 3
      "E = mc^2", // 4
      "```", // 5
    ].join("\n");
    const regions = detectRenderableRegions(markdownContext(md));
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("math");
    expect(regions[0]?.sourceStartLine).toBe(3);
    expect(regions[0]?.sourceEndLine).toBe(5);
    expect(regions[0]?.displayText).toBe("E = mc^2");
  });

  it("locates inline `$...$` math regions with their line number", () => {
    const md = [
      "First line with no math.", // 1
      "Second line has $E = mc^2$ inline.", // 2
      "", // 3
      "Third line has $a + b$ and $c + d$ both.", // 4
    ].join("\n");
    const regions = detectRenderableRegions(markdownContext(md));
    // Three inline math regions.
    expect(regions).toHaveLength(3);
    expect(regions.every((region) => region.renderer === "math")).toBe(true);
    // Line 2 has one, line 4 has two.
    const byLine = regions.reduce<Record<number, number>>((acc, region) => {
      acc[region.sourceStartLine] = (acc[region.sourceStartLine] ?? 0) + 1;
      return acc;
    }, {});
    expect(byLine[2]).toBe(1);
    expect(byLine[4]).toBe(2);
  });

  it("locates multi-line `$$...$$` block math regions", () => {
    const md = [
      "Intro.", // 1
      "", // 2
      "$$", // 3
      "\\int_0^1 x^2 \\, dx = \\frac{1}{3}", // 4
      "$$", // 5
      "", // 6
      "Outro.", // 7
    ].join("\n");
    const regions = detectRenderableRegions(markdownContext(md));
    expect(regions).toHaveLength(1);
    const region = regions[0] as SourceRenderableRegion;
    expect(region.renderer).toBe("math");
    expect(region.sourceStartLine).toBe(3);
    expect(region.sourceEndLine).toBe(5);
    expect(region.displayText).toBe("\\int_0^1 x^2 \\, dx = \\frac{1}{3}");
  });

  it("ignores `$` inside code fences when locating math regions", () => {
    const md = [
      "```bash", // 1
      "echo $HOME $PATH", // 2
      "```", // 3
      "After the fence we have $a+b$ as math.", // 4
    ].join("\n");
    const regions = detectRenderableRegions(markdownContext(md));
    // Only the post-fence inline math should surface.
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("math");
    expect(regions[0]?.sourceStartLine).toBe(4);
  });

  it("sorts regions by source line so callers can iterate in document order", () => {
    const md = [
      "", // 1
      "```mermaid", // 2
      "flow", // 3
      "```", // 4
      "", // 5
      "Inline $x$ math here.", // 6
      "", // 7
      "```math", // 8
      "y = 1", // 9
      "```", // 10
    ].join("\n");
    const regions = detectRenderableRegions(markdownContext(md));
    expect(regions.map((region) => region.renderer)).toEqual([
      "mermaid",
      "math",
      "math",
    ]);
    // Sorted ascending by start line.
    const starts = regions.map((region) => region.sourceStartLine);
    expect(starts).toEqual([...starts].sort((left, right) => left - right));
  });

  it("produces stable ids for the same region across re-detection passes", () => {
    const md = "# Title\n\n```mermaid\nflowchart\n```\n";
    const first = detectRenderableRegions(markdownContext(md));
    const second = detectRenderableRegions(markdownContext(md));
    expect(first.map((region) => region.id)).toEqual(
      second.map((region) => region.id),
    );
  });

  it("marks regions as read-only in diff / markdown-diff mode", () => {
    const md = "```mermaid\nflowchart\n```\n";
    const diffRegions = detectRenderableRegions(
      markdownContext(md, { mode: "diff" }),
    );
    const mdDiffRegions = detectRenderableRegions(
      markdownContext(md, { mode: "markdown-diff" }),
    );
    expect(diffRegions.every((region) => !region.editable)).toBe(true);
    expect(mdDiffRegions.every((region) => !region.editable)).toBe(true);
  });

  it("marks regions as editable in source / diff-edit mode", () => {
    const md = "```mermaid\nflowchart\n```\n";
    const sourceRegions = detectRenderableRegions(
      markdownContext(md, { mode: "source" }),
    );
    const diffEditRegions = detectRenderableRegions(
      markdownContext(md, { mode: "diff-edit" }),
    );
    expect(sourceRegions.every((region) => region.editable)).toBe(true);
    expect(diffEditRegions.every((region) => region.editable)).toBe(true);
  });
});

describe("detectRenderableRegions: dedicated file types", () => {
  it("treats `.mmd` files as a single whole-file Mermaid region", () => {
    const content = "flowchart TD\n  A --> B\n  B --> C\n";
    const regions = detectRenderableRegions({
      path: "diagram.mmd",
      language: null,
      content,
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("mermaid");
    expect(regions[0]?.sourceStartLine).toBe(1);
    expect(regions[0]?.sourceEndLine).toBe(4);
    expect(regions[0]?.displayText).toBe(content);
    expect(regions[0]?.editable).toBe(true);
  });

  it("treats `.mermaid` extension the same as `.mmd`", () => {
    const regions = detectRenderableRegions({
      path: "x.mermaid",
      language: null,
      content: "flow",
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("mermaid");
  });

  it("surfaces no region for empty dedicated Mermaid files", () => {
    expect(
      detectRenderableRegions({
        path: "empty.mmd",
        language: null,
        content: "   \n\t\n",
        mode: "source",
      }),
    ).toEqual([]);
  });

  it("recognizes explicit `mermaid` language regardless of extension", () => {
    const regions = detectRenderableRegions({
      path: "unknown.txt",
      language: "mermaid",
      content: "flowchart TD",
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("mermaid");
  });

  it("returns nothing for unknown file types with no matching language", () => {
    expect(
      detectRenderableRegions({
        path: "src/main.rs",
        language: "rust",
        content: "fn main() {}",
        mode: "source",
      }),
    ).toEqual([]);
  });
});

describe("detectRenderableRegions: Rust doc comments (Phase 5)", () => {
  it("returns nothing for Rust files that contain only regular comments", () => {
    const src = [
      "// A regular one-line comment.", // 1
      "fn add(a: i32, b: i32) -> i32 { a + b }", // 2
      "", // 3
      "/* A regular block comment.", // 4
      "   No Mermaid here.", // 5
      "*/", // 6
    ].join("\n");
    expect(
      detectRenderableRegions({
        path: "src/lib.rs",
        language: "rust",
        content: src,
        mode: "source",
      }),
    ).toEqual([]);
  });

  it("finds a Mermaid fence inside an outer `///` doc-comment block", () => {
    // 1-based line numbers in the Rust source:
    //   1: `/// Architecture:`
    //   2: `///`
    //   3: `/// ```mermaid`
    //   4: `/// sequenceDiagram`
    //   5: `///   Alice->>Bob: Hi`
    //   6: `/// ```
    //   7: `pub fn example() {}`
    const src = [
      "/// Architecture:",
      "///",
      "/// ```mermaid",
      "/// sequenceDiagram",
      "///   Alice->>Bob: Hi",
      "/// ```",
      "pub fn example() {}",
    ].join("\n");
    const regions = detectRenderableRegions({
      path: "src/lib.rs",
      language: "rust",
      content: src,
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    const region = regions[0] as SourceRenderableRegion;
    expect(region.renderer).toBe("mermaid");
    // The Mermaid fence spans original Rust lines 3-6.
    expect(region.sourceStartLine).toBe(3);
    expect(region.sourceEndLine).toBe(6);
    expect(region.displayText).toBe("sequenceDiagram\n  Alice->>Bob: Hi");
  });

  it("finds a math block inside a module-level `//!` doc-comment block", () => {
    const src = [
      "//! Module invariant:", // 1
      "//!", // 2
      "//! ```math", // 3
      "//! revision_{n+1} = revision_n + 1", // 4
      "//! ```", // 5
      "", // 6
      "pub struct Foo;", // 7
    ].join("\n");
    const regions = detectRenderableRegions({
      path: "src/lib.rs",
      language: "rust",
      content: src,
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("math");
    expect(regions[0]?.sourceStartLine).toBe(3);
    expect(regions[0]?.sourceEndLine).toBe(5);
    expect(regions[0]?.displayText).toBe("revision_{n+1} = revision_n + 1");
  });

  it("finds a Mermaid fence inside a `/** ... */` block doc comment", () => {
    const src = [
      "/**", // 1
      " * Architecture sketch:", // 2
      " *", // 3
      " * ```mermaid", // 4
      " * flowchart TD", // 5
      " *   A --> B", // 6
      " * ```", // 7
      " */", // 8
      "pub fn example() {}", // 9
    ].join("\n");
    const regions = detectRenderableRegions({
      path: "src/lib.rs",
      language: "rust",
      content: src,
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    const region = regions[0] as SourceRenderableRegion;
    expect(region.renderer).toBe("mermaid");
    // The fence spans Rust lines 4-7 in the original file.
    expect(region.sourceStartLine).toBe(4);
    expect(region.sourceEndLine).toBe(7);
    expect(region.displayText).toBe("flowchart TD\n  A --> B");
  });

  it("does not treat regular `//` comments or `/* */` blocks as doc comments", () => {
    // Four-slash `////` is a regular line comment per rustdoc rules;
    // it must NOT be parsed as a doc block. Same for `/*` without
    // the `!`/`*` suffix.
    const src = [
      "//// ```mermaid", // 1 — regular comment
      "//// flowchart", // 2 — regular comment
      "//// ```", // 3 — regular comment
      "/* ```mermaid", // 4 — regular block comment
      "   flow", // 5
      "   ``` */", // 6
    ].join("\n");
    expect(
      detectRenderableRegions({
        path: "src/lib.rs",
        language: "rust",
        content: src,
        mode: "source",
      }),
    ).toEqual([]);
  });

  it("returns nothing for Rust files whose doc comments are plain prose (no fenced diagrams)", () => {
    const src = [
      "/// Adds two integers together.", // 1
      "///", // 2
      "/// This is a simple helper; no diagrams needed.", // 3
      "pub fn add(a: i32, b: i32) -> i32 { a + b }", // 4
    ].join("\n");
    // Phase 5 only surfaces Mermaid/math regions inside doc blocks.
    // Plain prose doc comments do not produce renderable regions
    // today — they would render as ordinary Markdown in future
    // phases but are not included in the Phase 5 scope.
    expect(
      detectRenderableRegions({
        path: "src/lib.rs",
        language: "rust",
        content: src,
        mode: "source",
      }),
    ).toEqual([]);
  });

  it("handles multiple doc-comment blocks on separate items in the same file", () => {
    const src = [
      "/// Widget one.", // 1
      "/// ```mermaid", // 2
      "/// flowchart A", // 3
      "/// ```", // 4
      "pub struct One;", // 5
      "", // 6
      "/// Widget two.", // 7
      "/// ```mermaid", // 8
      "/// flowchart B", // 9
      "/// ```", // 10
      "pub struct Two;", // 11
    ].join("\n");
    const regions = detectRenderableRegions({
      path: "src/lib.rs",
      language: "rust",
      content: src,
      mode: "source",
    });
    expect(regions).toHaveLength(2);
    expect(regions[0]?.sourceStartLine).toBe(2);
    expect(regions[0]?.sourceEndLine).toBe(4);
    expect(regions[0]?.displayText).toBe("flowchart A");
    expect(regions[1]?.sourceStartLine).toBe(8);
    expect(regions[1]?.sourceEndLine).toBe(10);
    expect(regions[1]?.displayText).toBe("flowchart B");
  });

  it("recognizes `.rs` extension without requiring a language hint", () => {
    const src = [
      "/// ```mermaid",
      "/// flowchart X",
      "/// ```",
      "pub fn x() {}",
    ].join("\n");
    const regions = detectRenderableRegions({
      path: "lib.rs",
      language: null,
      content: src,
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    expect(regions[0]?.renderer).toBe("mermaid");
  });

  it("handles single-line `/** text */` doc comments", () => {
    const src = [
      "/** One-line doc. */", // 1 — plain prose, no diagrams
      "pub fn a() {}", // 2
    ].join("\n");
    // Prose-only block: no renderable region.
    expect(
      detectRenderableRegions({
        path: "src/lib.rs",
        language: "rust",
        content: src,
        mode: "source",
      }),
    ).toEqual([]);
  });

  it("finds Mermaid fences in the real `import_discovered_codex_threads` doc comment shape", () => {
    // End-to-end sanity check using a doc-comment shape that matches
    // the one we added to `src/state_boot.rs` — proves the parser
    // handles the common "prose before + blank `///` separator +
    // Mermaid fence + blank `///` + prose after" pattern without
    // accidentally splitting the block or losing the fence.
    const src = [
      "/// Merges Codex threads discovered on disk into self.sessions.", // 1
      "///", // 2
      "/// Per-thread flow:", // 3
      "///", // 4
      "/// ```mermaid", // 5
      "/// flowchart TD", // 6
      "///   Start([thread]) --> Check{within scope?}", // 7
      "///   Check -- no --> Skip([skip])", // 8
      "///   Check -- yes --> Import[import as session]", // 9
      "/// ```", // 10
      "///", // 11
      "/// The re-stamp step is load-bearing.", // 12
      "fn import_discovered_codex_threads(&mut self) {}", // 13
    ].join("\n");
    const regions = detectRenderableRegions({
      path: "src/state_boot.rs",
      language: "rust",
      content: src,
      mode: "source",
    });
    expect(regions).toHaveLength(1);
    const region = regions[0] as SourceRenderableRegion;
    expect(region.renderer).toBe("mermaid");
    expect(region.sourceStartLine).toBe(5);
    expect(region.sourceEndLine).toBe(10);
    expect(region.displayText).toContain("flowchart TD");
    expect(region.displayText).toContain("Check -- yes --> Import");
  });

  it("marks Rust-derived regions read-only in diff/markdown-diff mode", () => {
    const src = [
      "/// ```mermaid",
      "/// flow",
      "/// ```",
      "pub fn z() {}",
    ].join("\n");
    const diffRegions = detectRenderableRegions({
      path: "src/lib.rs",
      language: "rust",
      content: src,
      mode: "diff",
    });
    expect(diffRegions[0]?.editable).toBe(false);
  });
});

describe("hasRenderableRegions", () => {
  it("returns true for Markdown with a Mermaid fence", () => {
    expect(
      hasRenderableRegions(
        markdownContext("```mermaid\nflowchart\n```\n"),
      ),
    ).toBe(true);
  });

  it("returns true for Markdown with inline math", () => {
    expect(hasRenderableRegions(markdownContext("Inline $x$ only."))).toBe(
      true,
    );
  });

  it("returns false for prose-only Markdown", () => {
    expect(
      hasRenderableRegions(
        markdownContext("Just prose and *maybe* some **emphasis**."),
      ),
    ).toBe(false);
  });
});
