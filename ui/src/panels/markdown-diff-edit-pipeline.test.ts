// Unit coverage for the paste-sanitization security boundary in
// `markdown-diff-edit-pipeline.ts`. Two guards matter:
//
//   - `isSafePastedMarkdownHref` decides whether an `<a href="...">`
//     from a pasted fragment keeps its link. The allowlist is
//     http / https / mailto plus Windows drive-letter paths
//     (the drive-letter exception is a known minor inconsistency
//     with `transformMarkdownLinkUri`'s tighter allowlist — see
//     `docs/bugs.md` → "isSafePastedMarkdownHref Windows drive-letter
//     exception inconsistent with protocol allowlist").
//   - `sanitizePastedMarkdownFragment` walks a `<template>` content
//     fragment and applies three gates: HTML-namespace only,
//     drop the 24-element block set, unwrap anything not in the
//     31-element allow set, strip every attribute except safe
//     `href` on `<a>` and normalized `language-*` `class` on
//     `<code>`.
//
// Before this file, neither guard had direct Vitest coverage, so a
// set-membership regression (e.g. accidentally removing `button`
// from the drop set, or adding `svg` to the allow set) would ship
// silently. These tests pin the current contract; a later pass will
// tighten the drive-letter exception (which is a documented
// follow-up, not a fix landing here).

import { afterEach, describe, expect, it } from "vitest";

import {
  insertSanitizedMarkdownPaste,
  isSafePastedMarkdownHref,
  normalizePastedMarkdownCodeClass,
  sanitizePastedMarkdownFragment,
} from "./markdown-diff-edit-pipeline";

function buildPasteFragment(html: string): DocumentFragment {
  // Mirrors `insertSanitizedMarkdownPaste`'s use of a `<template>`:
  // `<template>.innerHTML = html` parses the fragment into an inert
  // tree (browsers do not fetch resources or run scripts inside
  // template content). We return the content fragment so tests can
  // hand it to `sanitizePastedMarkdownFragment` directly and
  // inspect the post-sanitize shape.
  const template = document.createElement("template");
  template.innerHTML = html;
  return template.content;
}

describe("isSafePastedMarkdownHref", () => {
  describe("rejects empty input", () => {
    it.each([
      ["", "empty string"],
      ["   ", "whitespace only"],
      ["\t\n\r", "control whitespace only"],
    ])("returns false for %s (%s)", (href) => {
      expect(isSafePastedMarkdownHref(href)).toBe(false);
    });
  });

  describe("rejects dangerous protocols", () => {
    // Every one of these has a colon AND falls outside the
    // http/https/mailto allowlist, so `isSafePastedMarkdownHref`
    // must reject. Control-char / whitespace obfuscation is stripped
    // before the protocol comparison, so `java\u0000script:` and
    // `  javascript:` are still recognized as the dangerous
    // `javascript:` prefix.
    it.each([
      ["javascript:alert(1)"],
      ["JavaScript:alert(1)"],
      ["JAVASCRIPT:alert(1)"],
      ["vbscript:msgbox('x')"],
      ["data:text/html,<script>alert(1)</script>"],
      ["data:application/javascript,alert(1)"],
      ["data:image/png;base64,iVBORw0KGgo="],
      ["file:///etc/passwd"],
      ["ftp://ftp.example.com/"],
      ["ws://example.com/socket"],
      ["blob:http://example.com/abcdef"],
      // Leading whitespace is stripped by trim() before the
      // protocol check.
      ["  javascript:alert(1)"],
      ["\tjavascript:alert(1)"],
      // Internal control-byte / whitespace obfuscation: the
      // normalizer strips `\u0000` through `\u001F`, `\u007F`, and
      // `\s`, so `java\u0000script:` collapses to `javascript:`
      // before the allowlist check.
      ["java\u0000script:alert(1)"],
      ["jav\nascript:alert(1)"],
      ["\u0001javascript:alert(1)"],
    ])("returns false for %s", (href) => {
      expect(isSafePastedMarkdownHref(href)).toBe(false);
    });
  });

  describe("allows http / https / mailto", () => {
    it.each([
      ["http://example.com"],
      ["http://example.com/path?query=1#frag"],
      ["HTTP://example.com"],
      ["https://example.com"],
      ["HTTPS://example.com"],
      ["HtTpS://example.com"],
      ["mailto:alice@example.com"],
      ["MAILTO:alice@example.com"],
    ])("returns true for %s", (href) => {
      expect(isSafePastedMarkdownHref(href)).toBe(true);
    });
  });

  describe("allows no-protocol hrefs", () => {
    // The guard treats any href with no colon (after normalization)
    // as a relative path or anchor and returns true. This keeps
    // pasted `[text](./file.md)` and `[text](#anchor)` fragments
    // working through the rendered-Markdown edit path.
    it.each([
      ["./relative/file.md"],
      ["../sibling.md"],
      ["../sibling.md#L10"],
      ["docs/architecture.md"],
      ["foo.txt"],
      ["#anchor"],
      ["#section-2"],
      ["/absolute/path"],
    ])("returns true for %s", (href) => {
      expect(isSafePastedMarkdownHref(href)).toBe(true);
    });
  });

  describe("rejects Windows drive-letter paths", () => {
    // The guard previously short-circuited on `/^[a-zA-Z]:[\\/]/`
    // and returned true for Windows drive-letter paths. That
    // branch was removed so paste-sanitize treats drive-letter
    // hrefs the same as any other unknown protocol:
    // `C:\foo` has its colon at index 1, the one-letter "c"
    // protocol is not in the http/https/mailto allowlist, so
    // the href is rejected — and the paste-sanitizer's
    // attribute scrubber then drops the `href` attribute while
    // keeping the `<a>` element and its inner text.
    //
    // Local-path file links that the user AUTHORS (typed into
    // the rendered-Markdown editor or in a committed file) still
    // work through
    // `markdown-links.ts::resolveMarkdownFileLinkTarget`, which
    // has its own allowlist that recognizes drive-letter paths.
    // Only the paste-sanitize entry point is tightened here; the
    // rendered/open-in-source-panel path is unchanged.
    it.each([
      ["C:\\repo\\docs\\api.md"],
      ["c:/repo/docs/api.md"],
      ["Z:\\path\\file.txt"],
      ["d:/x"],
      ["A:\\"],
      ["z:/"],
      // Bare drive letter without separator — also rejected.
      ["C:"],
      // Two-letter prefix that's definitely not a drive letter.
      ["CC:\\foo"],
    ])("returns false for %s", (href) => {
      expect(isSafePastedMarkdownHref(href)).toBe(false);
    });

    it("strips href from a pasted <a href=\"C:\\...\"> anchor", () => {
      // End-to-end proof: the sanitizer reads
      // `isSafePastedMarkdownHref` for the `<a>` attribute
      // scrubber, and the new contract is "drive-letter paths
      // lose their href". The `<a>` element itself is in the
      // ALLOWED set, so the element stays; its link text
      // survives as plain content.
      const fragment = buildPasteFragment(
        '<a href="C:\\Windows\\System32\\cmd.exe">open cmd</a>',
      );
      sanitizePastedMarkdownFragment(fragment);
      const anchor = fragment.querySelector("a");
      expect(anchor).not.toBeNull();
      expect(anchor?.hasAttribute("href")).toBe(false);
      expect(anchor?.textContent).toBe("open cmd");
    });
  });
});

describe("normalizePastedMarkdownCodeClass", () => {
  // Helper shared by the sanitizer's attribute scrubbing path.
  // Returns the first `language-*` token (stripped to just that
  // token) or an empty string when no language class is present.
  it("extracts a single language class", () => {
    expect(normalizePastedMarkdownCodeClass("language-python")).toBe("language-python");
  });

  it("extracts a language class when other classes are present", () => {
    expect(normalizePastedMarkdownCodeClass("foo language-rust bar")).toBe("language-rust");
  });

  it("extracts the FIRST language class when multiple are present", () => {
    expect(normalizePastedMarkdownCodeClass("language-py language-ts")).toBe("language-py");
  });

  it("returns an empty string for classes with no language-*", () => {
    expect(normalizePastedMarkdownCodeClass("foo bar")).toBe("");
  });

  it("returns an empty string for an empty class list", () => {
    expect(normalizePastedMarkdownCodeClass("")).toBe("");
  });

  it("accepts hyphenated language names", () => {
    expect(normalizePastedMarkdownCodeClass("language-objective-c")).toBe(
      "language-objective-c",
    );
  });

  it("rejects `language-` prefixes with non-word characters in the name", () => {
    // The full regex is `(?:^|\s)language-([\w-]+)(?:\s|$)` — the
    // trailing `(?:\s|$)` anchor requires whitespace or
    // end-of-string after the language name. A dot, slash, or
    // other non-word character following the capture group means
    // the whole regex fails to match and the helper returns "".
    expect(normalizePastedMarkdownCodeClass("language-py.thon")).toBe("");
    expect(normalizePastedMarkdownCodeClass("language-")).toBe("");
  });
});

describe("sanitizePastedMarkdownFragment", () => {
  describe("drops disallowed elements (DROPPED set)", () => {
    // Every tag in the DROPPED set must be removed — the entire
    // subtree, including children, disappears. A regression that
    // dropped one of these from the set (allowing it through as
    // "unknown → unwrap") would be a real security problem for
    // tags like `<script>` and `<iframe>`.
    it.each([
      ["script", "<script>alert(1)</script>"],
      ["iframe", '<iframe src="https://evil.example.com"></iframe>'],
      ["svg", "<svg><circle r='10'/></svg>"],
      ["button", "<button onclick='x'>Click</button>"],
      ["img", "<img src='x' onerror='alert(1)'>"],
      ["form", "<form action='x'><input></form>"],
      ["input", "<input type='text'>"],
      ["style", "<style>body{background:red}</style>"],
      ["link", "<link rel='stylesheet' href='x.css'>"],
      ["audio", "<audio src='x'></audio>"],
      ["video", "<video src='x'></video>"],
      ["canvas", "<canvas></canvas>"],
      ["object", "<object data='x'></object>"],
      ["embed", "<embed src='x'>"],
      ["meta", "<meta http-equiv='refresh' content='0'>"],
      ["base", "<base href='http://evil.example.com'>"],
      ["textarea", "<textarea>x</textarea>"],
      ["select", "<select><option>x</option></select>"],
      ["picture", "<picture></picture>"],
      ["source", "<source src='x'>"],
      ["map", "<map></map>"],
      ["area", "<area>"],
      // `<math>` is the MathML root — its namespace is
      // `http://www.w3.org/1998/Math/MathML`, so it's caught by
      // both the namespace gate and the drop set. The
      // namespace-gate-isolation test below exercises a case where
      // only the namespace gate fires.
      ["math", "<math><mi>x</mi></math>"],
      // `<option>` is in DROPPED too; the sanitizer removes it
      // even when it appears inside a `<select>` (which is itself
      // dropped). We build a fragment where `<option>` is the
      // element actually under query to confirm its individual
      // drop, not the enclosing select's drop.
      ["option", "<select><option>x</option></select>"],
    ])("drops <%s>", (tagName, html) => {
      const fragment = buildPasteFragment(html);
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector(tagName)).toBeNull();
    });

    it("drops a dropped element's children along with it", () => {
      // Intentionally NOT using `<iframe>` as the parent: the HTML
      // parser treats `<iframe>` content as raw text, so children
      // like `<p>` and `<a>` never materialise as DOM elements —
      // the test would pass trivially before sanitize even runs.
      // `<form>` IS in the DROPPED set AND accepts structured
      // children, so we can assert "children existed before
      // sanitize ran, and were gone after" — the real
      // descendants-also-removed contract.
      const fragment = buildPasteFragment(
        "<form><p>inner paragraph</p><a href='x'>link</a></form>",
      );
      expect(fragment.querySelector("form")).not.toBeNull();
      expect(fragment.querySelector("p")).not.toBeNull();
      expect(fragment.querySelector("a")).not.toBeNull();
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("form")).toBeNull();
      expect(fragment.querySelector("p")).toBeNull();
      expect(fragment.querySelector("a")).toBeNull();
    });

    it("drops foreign-namespace elements via the namespace gate", () => {
      // Pin the `namespaceURI !== PASTED_MARKDOWN_HTML_NAMESPACE`
      // check in isolation. MathML children like `<mi>` / `<mo>`
      // are NOT in either the drop set or the allow set; the ONLY
      // defence that removes them is the namespace gate. If that
      // gate were deleted, `<mi>` would fall through to the
      // "unknown element → unwrap" branch and its text content
      // would survive in the editable buffer — a silent
      // MathML-injection bypass.
      //
      // `<math>` itself is in DROPPED so `fragment.querySelector
      // ("math")` being null doesn't prove the namespace gate
      // fired. We assert `<mi>` is removed, which is only
      // reachable through the namespace path.
      const fragment = buildPasteFragment("<math><mi>x</mi></math>");
      const miBeforeSanitize = fragment.querySelector("mi");
      expect(miBeforeSanitize).not.toBeNull();
      expect(miBeforeSanitize?.namespaceURI).toBe(
        "http://www.w3.org/1998/Math/MathML",
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("mi")).toBeNull();
      expect(fragment.querySelector("math")).toBeNull();
    });
  });

  describe("unwraps unknown elements (not in allow set, not in drop set)", () => {
    // Unknown tags are "unwrapped" — the element itself is removed
    // but its children survive in its place. This preserves
    // pasted text content that happens to sit inside exotic
    // containers (`<article>`, `<section>`, `<mark>`, etc.) without
    // bringing the unknown element through to the editable DOM.
    it.each([
      ["article"],
      ["section"],
      ["aside"],
      ["header"],
      ["footer"],
      ["nav"],
      ["main"],
      ["details"],
      ["summary"],
      ["mark"],
      ["u"],
      ["sub"],
      ["sup"],
      ["font"],
      ["center"],
    ])("unwraps <%s> but keeps its text", (tagName) => {
      const fragment = buildPasteFragment(`<${tagName}>keep me</${tagName}>`);
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector(tagName)).toBeNull();
      expect(fragment.textContent).toBe("keep me");
    });

    it("unwraps nested unknown elements inside allowed ones", () => {
      const fragment = buildPasteFragment(
        "<p>alpha <mark>highlighted</mark> omega</p>",
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("mark")).toBeNull();
      expect(fragment.querySelector("p")?.textContent).toBe("alpha highlighted omega");
    });
  });

  describe("keeps allowed elements (ALLOWED set)", () => {
    // Every tag in the ALLOWED set must survive the sanitize
    // pass. Regression guard for accidentally dropping one from the
    // set (e.g. `<li>` or `<strong>`), which would silently corrupt
    // rendered-Markdown paste.
    it.each([
      ["p", "<p>text</p>"],
      ["h1", "<h1>heading</h1>"],
      ["h2", "<h2>heading</h2>"],
      ["h3", "<h3>heading</h3>"],
      ["h4", "<h4>heading</h4>"],
      ["h5", "<h5>heading</h5>"],
      ["h6", "<h6>heading</h6>"],
      ["strong", "<strong>bold</strong>"],
      ["em", "<em>italic</em>"],
      ["b", "<b>bold</b>"],
      ["i", "<i>italic</i>"],
      ["del", "<del>strike</del>"],
      ["s", "<s>strike</s>"],
      ["code", "<code>x</code>"],
      ["pre", "<pre>x</pre>"],
      ["blockquote", "<blockquote>q</blockquote>"],
      ["ul", "<ul><li>a</li></ul>"],
      ["ol", "<ol><li>a</li></ol>"],
      ["li", "<ol><li>a</li></ol>"],
      ["table", "<table><tr><td>a</td></tr></table>"],
      ["thead", "<table><thead><tr><th>a</th></tr></thead></table>"],
      ["tbody", "<table><tbody><tr><td>a</td></tr></tbody></table>"],
      ["tfoot", "<table><tfoot><tr><td>a</td></tr></tfoot></table>"],
      ["tr", "<table><tr><td>a</td></tr></table>"],
      ["td", "<table><tr><td>a</td></tr></table>"],
      ["th", "<table><tr><th>a</th></tr></table>"],
      ["br", "<p>a<br>b</p>"],
      ["hr", "<hr>"],
      ["span", "<span>s</span>"],
      ["div", "<div>d</div>"],
    ])("keeps <%s>", (tagName, html) => {
      const fragment = buildPasteFragment(html);
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector(tagName)).not.toBeNull();
    });

    it("keeps <a> but strips href when unsafe", () => {
      const fragment = buildPasteFragment(
        '<a href="javascript:alert(1)">click me</a>',
      );
      sanitizePastedMarkdownFragment(fragment);
      const anchor = fragment.querySelector("a");
      expect(anchor).not.toBeNull();
      expect(anchor?.hasAttribute("href")).toBe(false);
      expect(anchor?.textContent).toBe("click me");
    });
  });

  describe("attribute scrubbing", () => {
    it("keeps safe href on <a>", () => {
      const fragment = buildPasteFragment(
        '<a href="https://example.com">link</a>',
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("a")?.getAttribute("href")).toBe(
        "https://example.com",
      );
    });

    it("drops dangerous href on <a>", () => {
      const fragment = buildPasteFragment(
        '<a href="javascript:alert(1)">xss</a>',
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("a")?.hasAttribute("href")).toBe(false);
    });

    it("strips onclick / onmouseover / on* event-handler attributes on <a>", () => {
      const fragment = buildPasteFragment(
        '<a href="https://example.com" onclick="alert(1)" onmouseover="alert(2)" title="Tip">link</a>',
      );
      sanitizePastedMarkdownFragment(fragment);
      const anchor = fragment.querySelector("a");
      expect(anchor?.getAttribute("href")).toBe("https://example.com");
      expect(anchor?.hasAttribute("onclick")).toBe(false);
      expect(anchor?.hasAttribute("onmouseover")).toBe(false);
      expect(anchor?.hasAttribute("title")).toBe(false);
    });

    it("strips href on non-<a> elements", () => {
      // The attribute scrubber only keeps `href` on `<a>`. If
      // someone pastes `<p href="x">`, the href must be removed
      // regardless of its content.
      const fragment = buildPasteFragment('<p href="https://example.com">x</p>');
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("p")?.hasAttribute("href")).toBe(false);
    });

    it("normalizes and keeps a `language-*` class on <code>", () => {
      const fragment = buildPasteFragment(
        '<code class="language-python extra">print(1)</code>',
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("code")?.getAttribute("class")).toBe(
        "language-python",
      );
    });

    it("strips class on <code> when there is no language-* token", () => {
      const fragment = buildPasteFragment(
        '<code class="foo bar">inline</code>',
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("code")?.hasAttribute("class")).toBe(false);
    });

    it("strips class on non-<code> allowed elements", () => {
      // Only `<code>` gets the class-preservation pass. A
      // `<strong class="foo">` must have its class stripped.
      const fragment = buildPasteFragment(
        '<strong class="language-python">bold</strong>',
      );
      sanitizePastedMarkdownFragment(fragment);
      expect(fragment.querySelector("strong")?.hasAttribute("class")).toBe(false);
    });

    it("strips style / data-* / id attributes everywhere", () => {
      const fragment = buildPasteFragment(
        '<p style="color:red" data-x="1" id="p1">x</p>',
      );
      sanitizePastedMarkdownFragment(fragment);
      const p = fragment.querySelector("p");
      expect(p?.hasAttribute("style")).toBe(false);
      expect(p?.hasAttribute("data-x")).toBe(false);
      expect(p?.hasAttribute("id")).toBe(false);
    });

    it("strips style / data-* / id on <a> even though href is preserved", () => {
      // The attribute scrubber has a special branch for `<a href>`
      // that `continue`s past the removal. That branch applies
      // ONLY to href — every other attribute on `<a>` (style,
      // data-*, id, event handlers) still falls through the
      // default removal path. This pins that behaviour so a
      // future refactor that widens the `<a>` branch doesn't
      // silently leak attributes.
      const fragment = buildPasteFragment(
        '<a href="https://example.com" style="color:red" data-track="click" id="link1">x</a>',
      );
      sanitizePastedMarkdownFragment(fragment);
      const anchor = fragment.querySelector("a");
      expect(anchor?.getAttribute("href")).toBe("https://example.com");
      expect(anchor?.hasAttribute("style")).toBe(false);
      expect(anchor?.hasAttribute("data-track")).toBe(false);
      expect(anchor?.hasAttribute("id")).toBe(false);
    });

    it("strips style / data-* / id on <code> even though language-* class is preserved", () => {
      // Same pin as above but for the `<code class>` branch. The
      // scrubber's `continue` applies only to the normalised
      // class attribute; every other attribute still falls
      // through to removal.
      const fragment = buildPasteFragment(
        '<code class="language-rust extra" style="color:red" data-lang="rust" id="c1">fn main(){}</code>',
      );
      sanitizePastedMarkdownFragment(fragment);
      const code = fragment.querySelector("code");
      expect(code?.getAttribute("class")).toBe("language-rust");
      expect(code?.hasAttribute("style")).toBe(false);
      expect(code?.hasAttribute("data-lang")).toBe(false);
      expect(code?.hasAttribute("id")).toBe(false);
    });
  });

  describe("realistic paste fragments", () => {
    // End-to-end shape assertions on realistic fragments that
    // combine several of the above contracts — a useful smoke
    // check that the three gates cooperate.
    it("sanitizes a mixed fragment into the expected post-sanitize shape", () => {
      const fragment = buildPasteFragment([
        "<h2>Heading</h2>",
        "<p>Paragraph with <strong class=\"foo\">bold</strong> and ",
        "<a href=\"https://example.com\" onclick=\"alert(1)\">a link</a>.</p>",
        "<iframe src=\"https://evil.example.com\"></iframe>",
        "<section>Unknown wrapper with <em>emphasis</em>.</section>",
        "<pre><code class=\"language-rust extra\">fn main() {}</code></pre>",
        "<script>alert('xss')</script>",
      ].join(""));
      sanitizePastedMarkdownFragment(fragment);

      // Dropped tags gone entirely.
      expect(fragment.querySelector("iframe")).toBeNull();
      expect(fragment.querySelector("script")).toBeNull();

      // Allowed tags survive; the unknown <section> is unwrapped
      // so its <em> child remains.
      expect(fragment.querySelector("h2")).not.toBeNull();
      expect(fragment.querySelector("p")).not.toBeNull();
      expect(fragment.querySelector("section")).toBeNull();
      expect(fragment.querySelector("em")).not.toBeNull();

      // Attribute scrubbing hit the anchor correctly.
      const anchor = fragment.querySelector("a");
      expect(anchor?.getAttribute("href")).toBe("https://example.com");
      expect(anchor?.hasAttribute("onclick")).toBe(false);

      // Attribute scrubbing stripped `class="foo"` from <strong>
      // but normalized `language-rust` on <code>.
      expect(fragment.querySelector("strong")?.hasAttribute("class")).toBe(false);
      expect(fragment.querySelector("code")?.getAttribute("class")).toBe(
        "language-rust",
      );
    });
  });
});

describe("insertSanitizedMarkdownPaste", () => {
  // Smoke coverage for the full paste handler. Heavier DOM /
  // Selection interactions come from downstream edit flows;
  // these tests pin the core contract — pasting a fragment with
  // disallowed tags must not leave those tags inside the
  // section, the fallback-text branch works when sanitize
  // empties the fragment, and the early-return on empty input
  // leaves both the section and the selection untouched.
  function setSelectionTo(node: Node) {
    const range = document.createRange();
    range.selectNodeContents(node);
    range.collapse(false);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  }

  afterEach(() => {
    // Clear any lingering selection between tests. The insertion
    // flow moves the caret after the last inserted node and that
    // node gets orphaned when its host `section` is removed.
    // Without an explicit clear, a later test that forgot to
    // call `setSelectionTo` would observe stale selection state.
    window.getSelection()?.removeAllRanges();
  });

  it("drops disallowed tags before inserting", () => {
    const section = document.createElement("div");
    document.body.appendChild(section);
    setSelectionTo(section);

    try {
      insertSanitizedMarkdownPaste(
        section,
        "<iframe src='x'></iframe><script>alert(1)</script><p>keep me</p>",
        "keep me",
      );

      expect(section.querySelector("iframe")).toBeNull();
      expect(section.querySelector("script")).toBeNull();
      expect(section.querySelector("p")).not.toBeNull();
      expect(section.textContent).toContain("keep me");
    } finally {
      section.remove();
    }
  });

  it("falls back to the plain-text fallback when the sanitized fragment is empty", () => {
    // If the pasted HTML contained only dropped elements, the
    // sanitized fragment is empty and the fallback text is
    // inserted in its place. Covers the `childNodes.length === 0
    // && fallbackText.length > 0` branch.
    const section = document.createElement("div");
    document.body.appendChild(section);
    setSelectionTo(section);

    try {
      insertSanitizedMarkdownPaste(
        section,
        "<iframe></iframe><script></script>",
        "plain text fallback",
      );

      expect(section.textContent).toBe("plain text fallback");
      expect(section.querySelector("iframe")).toBeNull();
    } finally {
      section.remove();
    }
  });

  it("inserts safe content verbatim when everything is allowed", () => {
    const section = document.createElement("div");
    document.body.appendChild(section);
    setSelectionTo(section);

    try {
      insertSanitizedMarkdownPaste(
        section,
        '<p>Hello <strong>world</strong> <a href="https://example.com">link</a>.</p>',
        "Hello world link.",
      );

      const paragraph = section.querySelector("p");
      expect(paragraph).not.toBeNull();
      expect(paragraph?.textContent).toBe("Hello world link.");
      expect(section.querySelector("a")?.getAttribute("href")).toBe(
        "https://example.com",
      );
    } finally {
      section.remove();
    }
  });

  it("does nothing when both the sanitized fragment and the fallback are empty", () => {
    // Empty pasted HTML + empty fallback → nothing happens. The
    // function returns early at `insertedNodes.length === 0`
    // before touching the selection. Pin that the section's
    // child list AND the selection state are both unchanged,
    // not just "some <p> with the original text is still
    // somewhere in the section".
    const section = document.createElement("div");
    section.innerHTML = "<p>original</p>";
    document.body.appendChild(section);
    setSelectionTo(section);

    const selection = window.getSelection();
    const beforeAnchor = selection?.anchorNode;
    const beforeOffset = selection?.anchorOffset;
    const beforeRangeCount = selection?.rangeCount;

    try {
      insertSanitizedMarkdownPaste(section, "", "");
      expect(section.innerHTML).toBe("<p>original</p>");
      expect(section.childNodes.length).toBe(1);
      expect(selection?.anchorNode).toBe(beforeAnchor);
      expect(selection?.anchorOffset).toBe(beforeOffset);
      expect(selection?.rangeCount).toBe(beforeRangeCount);
    } finally {
      section.remove();
    }
  });

  it("replaces the current selection with the sanitized fragment", () => {
    // `insertSanitizedMarkdownPaste` calls `range.deleteContents()`
    // before inserting the sanitized fragment, which means a
    // non-collapsed selection is REPLACED, not merely inserted
    // alongside. Cover that path: put "WORLD" under selection
    // inside an existing `<p>`, paste `<strong>NEW</strong>`,
    // and assert the post-paste text is `"HELLO NEW"` (not
    // `"HELLO WORLDNEW"` or similar).
    const section = document.createElement("div");
    section.innerHTML = "<p>HELLO WORLD</p>";
    document.body.appendChild(section);

    try {
      const paragraph = section.querySelector("p");
      expect(paragraph).not.toBeNull();
      const textNode = paragraph!.firstChild as Text;
      expect(textNode.nodeType).toBe(Node.TEXT_NODE);

      // Select "WORLD" (offset 6 .. 11 in "HELLO WORLD").
      const range = document.createRange();
      range.setStart(textNode, 6);
      range.setEnd(textNode, 11);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);

      insertSanitizedMarkdownPaste(
        section,
        "<strong>NEW</strong>",
        "NEW",
      );

      expect(section.querySelector("strong")?.textContent).toBe("NEW");
      expect(section.textContent).toBe("HELLO NEW");
      expect(section.textContent).not.toContain("WORLD");
    } finally {
      section.remove();
    }
  });
});
