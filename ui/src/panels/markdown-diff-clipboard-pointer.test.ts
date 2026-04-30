import { describe, expect, it, vi } from "vitest";

import {
  getSelectionRangeInsideSection,
  serializeSelectedMarkdown,
  setDropCaretFromPoint,
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
  it("sets the drop caret from a point inside the section", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p>Inside text</p></div>
      </section>
    `);
    const text = section.querySelector("p")?.firstChild;
    if (!text) {
      throw new Error("inside text missing");
    }
    const documentWithCaret = document as unknown as {
      caretPositionFromPoint?: unknown;
    };
    const originalCaretPositionFromPoint =
      documentWithCaret.caretPositionFromPoint;
    const caretPositionFromPoint = vi.fn(() => ({
      offsetNode: text,
      offset: 3,
    }));
    documentWithCaret.caretPositionFromPoint = caretPositionFromPoint;

    try {
      setDropCaretFromPoint(section, 10, 20);

      expect(caretPositionFromPoint).toHaveBeenCalledWith(10, 20);
      const selection = document.getSelection();
      expect(selection?.rangeCount).toBe(1);
      const range = selection?.getRangeAt(0);
      expect(range?.startContainer).toBe(text);
      expect(range?.startOffset).toBe(3);
      expect(range?.collapsed).toBe(true);
    } finally {
      if (originalCaretPositionFromPoint === undefined) {
        delete documentWithCaret.caretPositionFromPoint;
      } else {
        documentWithCaret.caretPositionFromPoint =
          originalCaretPositionFromPoint;
      }
      document.getSelection()?.removeAllRanges();
    }
  });

  it("sets the drop caret with the legacy caretRangeFromPoint fallback", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p>Inside text</p></div>
      </section>
    `);
    const text = section.querySelector("p")?.firstChild;
    if (!text) {
      throw new Error("inside text missing");
    }
    const range = document.createRange();
    range.setStart(text, 4);
    range.collapse(true);
    const documentWithCaret = document as unknown as {
      caretPositionFromPoint?: unknown;
      caretRangeFromPoint?: unknown;
    };
    const originalCaretPositionFromPoint =
      documentWithCaret.caretPositionFromPoint;
    const originalCaretRangeFromPoint = documentWithCaret.caretRangeFromPoint;
    delete documentWithCaret.caretPositionFromPoint;
    const caretRangeFromPoint = vi.fn(() => range);
    documentWithCaret.caretRangeFromPoint = caretRangeFromPoint;

    try {
      setDropCaretFromPoint(section, 11, 21);

      expect(caretRangeFromPoint).toHaveBeenCalledWith(11, 21);
      const selection = document.getSelection();
      expect(selection?.rangeCount).toBe(1);
      const selectedRange = selection?.getRangeAt(0);
      expect(selectedRange?.startContainer).toBe(text);
      expect(selectedRange?.startOffset).toBe(4);
      expect(selectedRange?.collapsed).toBe(true);
    } finally {
      if (originalCaretPositionFromPoint === undefined) {
        delete documentWithCaret.caretPositionFromPoint;
      } else {
        documentWithCaret.caretPositionFromPoint =
          originalCaretPositionFromPoint;
      }
      if (originalCaretRangeFromPoint === undefined) {
        delete documentWithCaret.caretRangeFromPoint;
      } else {
        documentWithCaret.caretRangeFromPoint = originalCaretRangeFromPoint;
      }
      document.getSelection()?.removeAllRanges();
    }
  });

  it("rejects a drop caret point outside the section", () => {
    const section = replaceBodyWith(`
      <section data-section>
        <div class="markdown-copy"><p>Inside text</p></div>
      </section>
      <p data-outside>Outside text</p>
    `);
    const outsideText = document.querySelector("[data-outside]")?.firstChild;
    if (!outsideText) {
      throw new Error("outside text missing");
    }
    const documentWithCaret = document as unknown as {
      caretPositionFromPoint?: unknown;
    };
    const originalCaretPositionFromPoint =
      documentWithCaret.caretPositionFromPoint;
    document.getSelection()?.removeAllRanges();
    const caretPositionFromPoint = vi.fn(() => ({
      offsetNode: outsideText,
      offset: 0,
    }));
    documentWithCaret.caretPositionFromPoint = caretPositionFromPoint;

    try {
      setDropCaretFromPoint(section, 10, 20);

      expect(caretPositionFromPoint).toHaveBeenCalledWith(10, 20);
      expect(document.getSelection()?.rangeCount).toBe(0);
    } finally {
      if (originalCaretPositionFromPoint === undefined) {
        delete documentWithCaret.caretPositionFromPoint;
      } else {
        documentWithCaret.caretPositionFromPoint =
          originalCaretPositionFromPoint;
      }
    }
  });

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
