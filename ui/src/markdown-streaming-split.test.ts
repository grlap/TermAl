import { describe, expect, it } from "vitest";
import { splitStreamingMarkdownForRendering } from "./markdown-streaming-split";

describe("splitStreamingMarkdownForRendering", () => {
  it("returns empty halves for an empty input", () => {
    expect(splitStreamingMarkdownForRendering("")).toEqual({
      settled: "",
      pending: "",
    });
  });

  it("treats a plain paragraph (no blank line) as fully settled", () => {
    // Partial paragraphs render correctly through CommonMark — the
    // text just shows up. No need to defer.
    const text = "Hello world";
    expect(splitStreamingMarkdownForRendering(text)).toEqual({
      settled: text,
      pending: "",
    });
  });

  it("treats multiple settled paragraphs as fully settled", () => {
    const text = "First paragraph.\n\nSecond paragraph.";
    expect(splitStreamingMarkdownForRendering(text)).toEqual({
      settled: text,
      pending: "",
    });
  });

  describe("pipe tables", () => {
    it("defers a header-only partial table to pending", () => {
      const text = "| Col A | Col B |";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "",
        pending: text,
      });
    });

    it("defers header + partial separator to pending", () => {
      const text = "| Col A | Col B |\n| ---";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "",
        pending: text,
      });
    });

    it("defers header + complete separator + no body row to pending", () => {
      // Even with both header and separator, GFM's behavior here is
      // version-dependent (some emit empty <table>, others fall
      // through to text). Defer until the trailing blank line
      // confirms the block is closed.
      const text = "| Col A | Col B |\n| --- | --- |";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "",
        pending: text,
      });
    });

    it("defers header + separator + partial body row to pending", () => {
      const text = "| Col A | Col B |\n| --- | --- |\n| 42 |";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "",
        pending: text,
      });
    });

    it("settles a full table once a trailing blank line lands", () => {
      const text = "| Col A | Col B |\n| --- | --- |\n| 1 | 2 |\n";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });

    it("defers a partial table that follows a settled paragraph", () => {
      const text = "Some intro paragraph.\n\n| Col A | Col B |\n| --- | --- |\n| 1 | 2 |";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "Some intro paragraph.\n\n",
        pending: "| Col A | Col B |\n| --- | --- |\n| 1 | 2 |",
      });
    });

    it("settles a complete table that is followed by a partial paragraph", () => {
      const text = "| A | B |\n| --- | --- |\n| 1 | 2 |\n\nThat was the t";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });

    it("does not defer a paragraph that contains pipes mid-line", () => {
      // Pipes inside prose (e.g., "the | character") must not
      // trigger table deferral.
      const text = "Use the | character to separate fields.";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });

    it("recovers settled state when a non-pipe line breaks the table", () => {
      // If a stream produces a pipe header and then a non-pipe
      // continuation, the "table" was actually a stray pipe-line
      // followed by prose. Once the prose lands, we no longer
      // defer.
      const text = "| not a real header\nthen prose continues here.";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });
  });

  describe("fenced code blocks", () => {
    it("defers an unclosed fence and everything after it", () => {
      const text = "Intro text.\n\n```js\nconsole.log(1)";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "Intro text.\n\n",
        pending: "```js\nconsole.log(1)",
      });
    });

    it("settles a closed fence", () => {
      const text = "Intro.\n\n```js\nconsole.log(1)\n```\n";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });

    it("treats blank lines and pipes inside an unclosed fence as code body", () => {
      // The `| Col |` line inside the fence must NOT trigger
      // table-tracking. The whole fence-and-after is deferred
      // anyway because the fence is unclosed.
      const text = "```\n| Col |\n\n| 1 |";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "",
        pending: text,
      });
    });

    it("settles a closed fence even when its body contains pipes", () => {
      const text = "```\n| Col |\n| 1 |\n```\n";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });

    it("supports tilde fences", () => {
      const text = "~~~js\nfoo()";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "",
        pending: text,
      });
    });
  });

  describe("math display blocks", () => {
    it("defers an unclosed `$$` block", () => {
      const text = "Intro.\n\n$$\n\\sum_{i=1}^n i";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "Intro.\n\n",
        pending: "$$\n\\sum_{i=1}^n i",
      });
    });

    it("settles a closed `$$` block", () => {
      const text = "$$\nx^2 + y^2 = z^2\n$$\n";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: text,
        pending: "",
      });
    });
  });

  describe("interactions between block types", () => {
    it("cuts at the earliest open-block start when multiple are open", () => {
      // The fence opens at line 4 and is unclosed. The "table"
      // (line 7) is inside the fence body, so it's NOT really a
      // table — pipe-tracking is reset on fence open. The cut is
      // at the fence opener.
      const text = [
        "Intro line one.",
        "",
        "Intro line two.",
        "",
        "```js",
        "function f() {",
        "  return 1;",
        "  | x | y |",
      ].join("\n");
      const { settled, pending } = splitStreamingMarkdownForRendering(text);
      expect(settled).toBe("Intro line one.\n\nIntro line two.\n\n");
      expect(pending).toBe(
        "```js\nfunction f() {\n  return 1;\n  | x | y |",
      );
    });

    it("settles a closed fence followed by a partial table", () => {
      const text = "```\nclosed\n```\n| Col A | Col B |";
      expect(splitStreamingMarkdownForRendering(text)).toEqual({
        settled: "```\nclosed\n```\n",
        pending: "| Col A | Col B |",
      });
    });
  });

  describe("round-trip", () => {
    it("preserves the original input under plain settled + pending concatenation", () => {
      // The boundary newline between the last settled line and the
      // first pending line lives at the end of `settled`, so the
      // reconstruction is plain string concatenation regardless of
      // which half is empty. This matters for the rendering site:
      // it can splice the pending fragment as a sibling element
      // without re-deriving boundary semantics.
      const inputs = [
        "",
        "Hello",
        "Hello\n\n| A | B |",
        "```js\nconsole.log()",
        "Para 1.\n\nPara 2.\n\n| X |\n",
        // Edge: leading blank line then a partial table — verifies
        // the boundary newline is preserved.
        "\n| A |",
      ];
      for (const text of inputs) {
        const { settled, pending } = splitStreamingMarkdownForRendering(text);
        expect(settled + pending).toBe(text);
      }
    });
  });
});
