import { describe, expect, it } from "vitest";

import {
  getSelectionRangeInsideSection,
  serializeSelectedMarkdown,
} from "./markdown-diff-clipboard-pointer";

function replaceBodyWith(markup: string) {
  document.body.innerHTML = markup;
  const section = document.querySelector<HTMLElement>("[data-section]");
  if (!section) {
    throw new Error("test section missing");
  }
  return section;
}

describe("markdown-diff-clipboard-pointer", () => {
  it("rejects missing, collapsed, and out-of-section selections", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p>Inside text</p></div>
      </section>
      <p data-outside>Outside text</p>
    `);
    const selection = document.getSelection();
    selection?.removeAllRanges();

    expect(getSelectionRangeInsideSection(section)).toBeNull();

    const insideText = section.querySelector("p")?.firstChild;
    if (!insideText) {
      throw new Error("inside text missing");
    }
    const collapsedRange = document.createRange();
    collapsedRange.setStart(insideText, 0);
    collapsedRange.collapse(true);
    selection?.addRange(collapsedRange);

    expect(getSelectionRangeInsideSection(section)).toBeNull();

    selection?.removeAllRanges();
    const outsideText = document.querySelector("[data-outside]")?.firstChild;
    if (!outsideText) {
      throw new Error("outside text missing");
    }
    const escapingRange = document.createRange();
    escapingRange.setStart(insideText, 0);
    escapingRange.setEnd(outsideText, 7);
    selection?.addRange(escapingRange);

    expect(getSelectionRangeInsideSection(section)).toBeNull();
  });

  it("returns an in-section non-collapsed selection range", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p>Inside text</p></div>
      </section>
    `);
    const text = section.querySelector("p")?.firstChild;
    if (!text) {
      throw new Error("inside text missing");
    }
    const selection = document.getSelection();
    selection?.removeAllRanges();
    const range = document.createRange();
    range.setStart(text, 0);
    range.setEnd(text, 6);
    selection?.addRange(range);

    expect(getSelectionRangeInsideSection(section)).toBe(range);
  });

  it("serializes selected rendered Markdown when the round-trip produces content", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p>Hello <strong>world</strong></p></div>
      </section>
    `);
    const paragraph = section.querySelector("p");
    if (!paragraph) {
      throw new Error("paragraph missing");
    }
    const range = document.createRange();
    range.selectNode(paragraph);

    expect(serializeSelectedMarkdown(range, "fallback", section)).toBe(
      "Hello **world**",
    );
  });

  it("falls back to full Markdown when selection covers the editable body", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p><br></p></div>
      </section>
    `);
    const markdownRoot = section.querySelector<HTMLElement>(".markdown-copy");
    if (!markdownRoot) {
      throw new Error("markdown root missing");
    }
    const range = document.createRange();
    range.selectNodeContents(markdownRoot);

    expect(serializeSelectedMarkdown(range, "line\n\n", section)).toBe(
      "line\n\n",
    );
  });

  it("falls back to Range text when an empty partial selection cannot round-trip", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p><span>   </span><strong>body</strong></p></div>
      </section>
    `);
    const whitespace = section.querySelector("span")?.firstChild;
    if (!whitespace) {
      throw new Error("whitespace text missing");
    }
    const range = document.createRange();
    range.selectNodeContents(whitespace);

    expect(serializeSelectedMarkdown(range, "fallback", section)).toBe("   ");
  });
});
