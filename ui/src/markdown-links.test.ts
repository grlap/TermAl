// Regression guard for `transformMarkdownLinkUri`'s dangerous-protocol
// handling. `react-markdown`'s built-in `uriTransformer` neutralizes
// hrefs whose protocol is not in its allowlist (http, https, mailto,
// tel) by returning the literal string `"javascript:void(0)"`. React
// ≥ 18.3 logs a console warning every time that string reaches the
// DOM and is slated to block it outright in a future release. On top
// of that, an `<a href="javascript:void(0)">` that leaks through is
// an inert same-page-navigate anchor — visually harmless but still
// triggers history/popstate in some browsers, and breaks the
// "clicking a dangerous link does nothing" intuition.
//
// `transformMarkdownLinkUri` wraps `uriTransformer` and substitutes
// the `"javascript:void(0)"` placeholder with an empty string so the
// dangerous URL never reaches React. `MarkdownContent`'s `a`
// renderer then treats `!href` as "render a plain `<span>`" — the
// integration test for that path lives in `MarkdownContent.test.tsx`.
//
// This file pins the pure-function contract: every dangerous protocol
// neutralizes to `""`; every whitelisted/local/non-external href
// passes through unchanged.

import { describe, expect, it } from "vitest";

import {
  shouldScrubMarkdownDomHref,
  transformMarkdownLinkUri,
} from "./markdown-links";

describe("transformMarkdownLinkUri", () => {
  describe("dangerous protocols", () => {
    // `uriTransformer` returns `"javascript:void(0)"` for any protocol
    // not in its allowlist. Our wrapper must replace that sentinel
    // with `""` so React never sees a `javascript:` URL.
    it.each([
      ["javascript:alert(1)"],
      ["JavaScript:alert(1)"],
      ["JAVASCRIPT:alert(1)"],
      ["vbscript:msgbox('pwn')"],
      ["VBScript:msgbox('pwn')"],
      ["data:text/html,<script>alert(1)</script>"],
      ["data:application/javascript,alert(1)"],
      ["%6A%61%76%61%73%63%72%69%70%74%3Aalert(1)"],
      ["%76%62%73%63%72%69%70%74%3Amsgbox('pwn')"],
      ["%64%61%74%61%3Atext/html,<script>alert(1)</script>"],
      // `data:image/*` is NOT on `uriTransformer`'s allowlist
      // (only http/https/mailto/tel + `#`/`/` early returns), so
      // even image data URIs get neutralized. Pinning this here so a
      // future react-markdown upgrade that adds `data:image` support
      // is noticed — if that change is desirable it needs an explicit
      // test update, not a silent pass.
      ["data:image/png;base64,iVBORw0KGgo="],
    ])("neutralizes %s to an empty string", (href) => {
      expect(transformMarkdownLinkUri(href)).toBe("");
    });

    it("neutralizes dangerous URIs with surrounding whitespace", () => {
      // `uriTransformer` trims before matching; the sentinel we
      // compare against is emitted for the trimmed URL, so our
      // substitution covers padded inputs as well.
      expect(transformMarkdownLinkUri("  javascript:alert(1)  ")).toBe("");
    });

    it("never returns the `javascript:void(0)` sentinel directly", () => {
      // Load-bearing invariant: no matter what `uriTransformer` does
      // internally, this wrapper must not let the literal placeholder
      // string escape to the DOM. A regression that removed the
      // ternary would fail this check.
      const dangerousInputs = [
        "javascript:alert(1)",
        "vbscript:foo()",
        "data:text/html,<b>x</b>",
        "madeupprotocol:whatever",
      ];
      for (const input of dangerousInputs) {
        expect(transformMarkdownLinkUri(input)).not.toBe("javascript:void(0)");
      }
    });
  });

  describe("safe external protocols", () => {
    // These are on `uriTransformer`'s allowlist and must round-trip
    // through `transformMarkdownLinkUri` unchanged (modulo whatever
    // trimming `uriTransformer` itself does — we test the typical
    // well-formed shape).
    it.each([
      ["https://example.com/foo?bar=baz#frag", "https://example.com/foo?bar=baz#frag"],
      ["http://example.com", "http://example.com"],
      ["mailto:alice@example.com", "mailto:alice@example.com"],
      ["tel:+1234567890", "tel:+1234567890"],
    ])("passes %s through unchanged", (href, expected) => {
      expect(transformMarkdownLinkUri(href)).toBe(expected);
    });
  });

  describe("local / relative hrefs", () => {
    // `transformMarkdownLinkUri` short-circuits non-external hrefs
    // via the `isExternalMarkdownHref` early return, never calling
    // `uriTransformer`. This keeps workspace file links, anchors,
    // and relative paths unchanged regardless of how uriTransformer
    // evolves upstream.
    it.each([
      ["#anchor", "#anchor"],
      ["#section-2", "#section-2"],
      ["./relative/file.md", "./relative/file.md"],
      ["../sibling.md#L10", "../sibling.md#L10"],
      ["docs/architecture.md", "docs/architecture.md"],
      ["foo/bar.md#L42C3", "foo/bar.md#L42C3"],
      ["C:%5Crepo%5Cdocs%5CREADME.md", "C:%5Crepo%5Cdocs%5CREADME.md"],
    ])("passes %s through unchanged", (href, expected) => {
      expect(transformMarkdownLinkUri(href)).toBe(expected);
    });

    it("passes an empty href through unchanged", () => {
      // The `MarkdownContent` `a` renderer uses `!href` to detect the
      // empty-string sentinel we return for dangerous URIs. For
      // already-empty input we preserve the emptiness so the renderer
      // takes the plain-`<span>` branch — there's no link target to
      // navigate to in either case.
      expect(transformMarkdownLinkUri("")).toBe("");
    });
  });
});

describe("shouldScrubMarkdownDomHref", () => {
  it.each([
    ["file:///C:/repo/docs/README.md"],
    ["C:/repo/docs/README.md"],
    ["C:\\repo\\docs\\README.md"],
    ["/C:/repo/docs/README.md"],
    ["/C:%5Crepo%5Cdocs%5CREADME.md"],
    ["C:%5Crepo%5Cdocs%5CREADME.md"],
    ["%5C%5Cserver%5Cshare%5CREADME.md"],
    ["\\\\server\\share\\README.md"],
  ])("scrubs local absolute href %s", (href) => {
    expect(shouldScrubMarkdownDomHref(href)).toBe(true);
  });

  it.each([
    ["https://example.com/docs"],
    ["mailto:alice@example.com"],
    ["#anchor"],
    ["docs/README.md"],
    ["../docs/README.md"],
  ])("does not scrub non-local href %s", (href) => {
    expect(shouldScrubMarkdownDomHref(href)).toBe(false);
  });
});
