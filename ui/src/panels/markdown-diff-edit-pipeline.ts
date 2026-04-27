// Paste sanitization + Markdown re-serialization for the
// rendered-Markdown diff editor.
//
// What this file owns:
//   - The paste-sanitization pipeline:
//     `insertSanitizedMarkdownPaste` (replace the selection with
//     a sanitized HTML fragment + fall back to plain text),
//     `sanitizePastedMarkdownFragment` (strip non-HTML namespaces,
//     drop known-dangerous elements, unwrap unknown ones, clean
//     attributes on anchors and code blocks),
//     `normalizePastedMarkdownCodeClass` (keep only
//     `language-<name>` classes on `<code>`), and
//     `isSafePastedMarkdownHref` (allowlist anchors,
//     document-relative hrefs, `http(s)`, and `mailto`; local
//     absolute / network-looking hrefs and everything else are
//     rejected), and `isSerializableMarkdownHref` (serializer-side
//     href policy for links that already exist in trusted rendered
//     document content).
//   - The allow/deny sets
//     `PASTED_MARKDOWN_ALLOWED_ELEMENTS`,
//     `PASTED_MARKDOWN_DROPPED_ELEMENTS`, and the
//     `PASTED_MARKDOWN_HTML_NAMESPACE` guard value that decides
//     whether a pasted element is even HTML at all (rejects
//     SVG / MathML / foreign content wholesale).
//   - The DOM → Markdown serializer used both for live draft
//     commits and for the final disk-save round-trip:
//     `serializeEditableMarkdownSection`,
//     `serializeMarkdownBlockNode`,
//     `serializeMarkdownBlockChildren`,
//     `serializeMarkdownInlineChildren`,
//     `serializeMarkdownInlineNode`,
//     `serializeMarkdownList`, `serializeMarkdownTable`,
//     `wrapInlineMarkdownCode`.
//
// Serializer contract: blocks are joined by blank lines, inline
// content preserves whitespace, `<code>` is fenced with `` ` ``
// (or `` `` `` when the content itself contains a backtick),
// links round-trip via `[text](href)` after escaping label
// brackets/backslashes and destination parens/backslashes while
// rejecting destinations that cannot be safely represented inside
// Markdown's parenthesized link syntax; images / iframes / other
// disallowed inputs are already removed by the sanitizer (if they
// sneak through, the serializer emits the empty string).
//
// What this file does NOT own:
//   - The `MarkdownDiffDocument` component that runs the paste /
//     commit flow. That stays in `DiffPanel.tsx` and calls into
//     this module.
//   - The caret / focus / section-boundary navigation helpers
//     (`./editable-markdown-focus`).
//   - The `shouldSkipMarkdownEditableNode` predicate — it lives
//     in `./editable-markdown-focus` because the caret walker
//     also needs it. This module imports it.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same function
// bodies, same allowlists / denylists, same attribute names;
// consumers import from here directly.

import {
  MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE,
  safeDecodeMarkdownHref,
} from "../markdown-links";
import { shouldSkipMarkdownEditableNode } from "./editable-markdown-focus";

const MARKDOWN_HREF_POLICY_IGNORED_CHARACTERS =
  /[\u0000-\u001F\u007F\s]+|\p{Default_Ignorable_Code_Point}+/gu;
const UNSAFE_MARKDOWN_LINK_DESTINATION_CHARACTERS =
  /[\u0000-\u001F\u007F\s[\]<>]|\p{Default_Ignorable_Code_Point}/u;
const SAFE_EXTERNAL_MARKDOWN_PROTOCOLS = new Set(["http", "https", "mailto"]);

function normalizeMarkdownHrefForPolicy(href: string) {
  return href.trim().replace(MARKDOWN_HREF_POLICY_IGNORED_CHARACTERS, "");
}

function getMarkdownHrefProtocol(href: string) {
  const colonIndex = href.indexOf(":");
  return colonIndex === -1 ? null : href.slice(0, colonIndex).toLowerCase();
}

function looksLikeSerializableWindowsDriveHref(href: string) {
  return /^\/?[a-zA-Z]:[\\/]/.test(href);
}

export function insertSanitizedMarkdownPaste(
  section: HTMLElement,
  html: string,
  fallbackText: string,
) {
  const template = document.createElement("template");
  template.innerHTML = html;
  sanitizePastedMarkdownFragment(template.content);

  let fragment = template.content;
  if (fragment.childNodes.length === 0 && fallbackText.length > 0) {
    fragment = document.createDocumentFragment();
    fragment.append(document.createTextNode(fallbackText));
  }

  const insertedNodes = Array.from(fragment.childNodes);
  if (insertedNodes.length === 0) {
    return;
  }

  const selection = window.getSelection();
  const range =
    selection && selection.rangeCount > 0 && section.contains(selection.anchorNode)
      ? selection.getRangeAt(0)
      : document.createRange();
  if (!selection || selection.rangeCount === 0 || !section.contains(selection.anchorNode)) {
    range.selectNodeContents(section);
    range.collapse(false);
  }

  range.deleteContents();
  range.insertNode(fragment);

  const lastInsertedNode = insertedNodes[insertedNodes.length - 1];
  if (lastInsertedNode?.parentNode) {
    const afterInsertRange = document.createRange();
    afterInsertRange.setStartAfter(lastInsertedNode);
    afterInsertRange.collapse(true);
    selection?.removeAllRanges();
    selection?.addRange(afterInsertRange);
  }
}

const PASTED_MARKDOWN_HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";

const PASTED_MARKDOWN_ALLOWED_ELEMENTS = new Set([
  "a",
  "b",
  "blockquote",
  "br",
  "code",
  "del",
  "div",
  "em",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "hr",
  "i",
  "li",
  "ol",
  "p",
  "pre",
  "s",
  "span",
  "strong",
  "table",
  "tbody",
  "td",
  "tfoot",
  "th",
  "thead",
  "tr",
  "ul",
]);

const PASTED_MARKDOWN_DROPPED_ELEMENTS = new Set([
  "area",
  "audio",
  "base",
  "button",
  "canvas",
  "embed",
  "form",
  "iframe",
  "img",
  "input",
  "link",
  "map",
  "math",
  "meta",
  "object",
  "option",
  "picture",
  "script",
  "select",
  "source",
  "style",
  "svg",
  "textarea",
  "video",
]);

export function sanitizePastedMarkdownFragment(root: ParentNode) {
  const rootNode = root as Node;
  const elements =
    root instanceof Element
      ? [root, ...Array.from(root.querySelectorAll<Element>("*"))]
      : Array.from(root.querySelectorAll<Element>("*"));

  for (const element of elements) {
    if (!rootNode.contains(element)) {
      continue;
    }

    const tagName = element.tagName.toLowerCase();
    if (
      element.namespaceURI !== PASTED_MARKDOWN_HTML_NAMESPACE ||
      PASTED_MARKDOWN_DROPPED_ELEMENTS.has(tagName)
    ) {
      element.remove();
      continue;
    }

    if (!PASTED_MARKDOWN_ALLOWED_ELEMENTS.has(tagName)) {
      element.replaceWith(...Array.from(element.childNodes));
      continue;
    }

    for (const attributeName of element.getAttributeNames()) {
      const normalizedAttributeName = attributeName.toLowerCase();
      if (tagName === "a" && normalizedAttributeName === "href") {
        const href = element.getAttribute(attributeName);
        if (href && isSafePastedMarkdownHref(href)) {
          continue;
        }
      } else if (tagName === "code" && normalizedAttributeName === "class") {
        const languageClass = normalizePastedMarkdownCodeClass(
          element.getAttribute(attributeName) ?? "",
        );
        if (languageClass) {
          element.setAttribute("class", languageClass);
          continue;
        }
      }

      if (element.hasAttribute(attributeName)) {
        element.removeAttribute(attributeName);
      }
    }
  }
}

export function normalizePastedMarkdownCodeClass(className: string) {
  const languageMatch = className.match(/(?:^|\s)language-([\w-]+)(?:\s|$)/);
  return languageMatch ? `language-${languageMatch[1]}` : "";
}

export function isSafePastedMarkdownHref(href: string) {
  // Paste-time allowlist for `<a href="...">` in the rendered-
  // Markdown diff editor. Dangerous protocols are rejected; safe
  // external protocols (http/https/mailto) round-trip; no-colon
  // hrefs are accepted only when they are anchors or document-
  // relative paths. Network-looking paths (`//host/path`,
  // `\\host\share`) and rooted local paths (`/etc/passwd`,
  // `\Windows`) are stripped because this paste boundary has no
  // workspace context to prove they are project-scoped.
  //
  // Previously this function also short-circuited on
  // `/^[a-zA-Z]:[\\/]/` and returned true for Windows drive-
  // letter paths (`C:\foo`, `c:/bar`). That branch is removed:
  // such pastes survive sanitization with their `href` stripped
  // (the `<a>` element itself stays in ALLOWED; the serializer
  // then emits the anchor's text without a link target). The
  // drive-letter exception was inert under the current browser
  // deployment — `file://` hrefs are blocked from an http origin
  // — but would become an arbitrary-local-file hazard if TermAl
  // ever ships a Tauri/Electron wrapper or a native link opener,
  // and it disagreed with the rest of the project's allowlist
  // policy (see `docs/bugs.md` → "isSafePastedMarkdownHref
  // Windows drive-letter exception inconsistent with protocol
  // allowlist"). Local-path Markdown links that the user types
  // or authors continue to work through the rendered-Markdown
  // pathway in `markdown-links.ts::resolveMarkdownFileLinkTarget`;
  // only the paste-sanitize entry point is tightened here.
  const normalized = normalizeMarkdownHrefForPolicy(href);
  if (!normalized) {
    return false;
  }

  const decoded = normalizeMarkdownHrefForPolicy(safeDecodeMarkdownHref(normalized));
  const protocol =
    getMarkdownHrefProtocol(normalized) ??
    (decoded !== normalized ? getMarkdownHrefProtocol(decoded) : null);
  if (!protocol) {
    return (
      !normalized.startsWith("/") &&
      !normalized.startsWith("\\") &&
      !decoded.startsWith("/") &&
      !decoded.startsWith("\\")
    );
  }

  return SAFE_EXTERNAL_MARKDOWN_PROTOCOLS.has(protocol);
}

export function serializeEditableMarkdownSection(section: HTMLElement) {
  const markdownRoot = section.querySelector<HTMLElement>(".markdown-copy") ?? section;
  const blocks = Array.from(markdownRoot.childNodes)
    .map((node) => serializeMarkdownBlockNode(node))
    .filter((markdown) => markdown.trim().length > 0);

  return blocks.join("\n\n");
}

export function serializeMarkdownBlockNode(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) {
    return node.textContent?.trim() ?? "";
  }

  if (!(node instanceof HTMLElement) || shouldSkipMarkdownEditableNode(node)) {
    return "";
  }

  const tagName = node.tagName.toLowerCase();
  if (/^h[1-6]$/.test(tagName)) {
    const level = Number(tagName.slice(1));
    return `${"#".repeat(level)} ${serializeMarkdownInlineChildren(node).trim()}`;
  }
  if (tagName === "p") {
    return serializeMarkdownInlineChildren(node).trim();
  }
  if (tagName === "ul" || tagName === "ol") {
    return serializeMarkdownList(node, tagName === "ol");
  }
  if (tagName === "blockquote") {
    return serializeMarkdownBlockChildren(node)
      .split("\n")
      .map((line) => (line.length > 0 ? `> ${line}` : ">"))
      .join("\n");
  }
  if (tagName === "pre") {
    const codeElement = node.querySelector("code");
    const language = codeElement?.className.match(/language-([\w-]+)/)?.[1] ?? "";
    const code = codeElement?.textContent ?? node.textContent ?? "";
    const fence = buildMarkdownBacktickFence(code, 3);
    return `${fence}${language}\n${code.replace(/\n$/, "")}\n${fence}`;
  }
  if (tagName === "table") {
    return serializeMarkdownTable(node);
  }
  if (tagName === "div" && node.classList.contains("markdown-table-scroll")) {
    const table = node.querySelector("table");
    return table ? serializeMarkdownTable(table) : "";
  }
  if (tagName === "hr") {
    return "---";
  }
  if (tagName === "br") {
    return "\n";
  }

  return serializeMarkdownBlockChildren(node);
}

export function serializeMarkdownBlockChildren(element: HTMLElement) {
  return Array.from(element.childNodes)
    .map((node) => serializeMarkdownBlockNode(node))
    .filter((markdown) => markdown.trim().length > 0)
    .join("\n\n");
}

export function serializeMarkdownInlineChildren(element: HTMLElement) {
  return Array.from(element.childNodes).map((node) => serializeMarkdownInlineNode(node)).join("");
}

export function serializeMarkdownInlineNode(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) {
    return node.textContent ?? "";
  }

  if (!(node instanceof HTMLElement) || shouldSkipMarkdownEditableNode(node)) {
    return "";
  }

  const tagName = node.tagName.toLowerCase();
  if (tagName === "ul" || tagName === "ol") {
    return "";
  }
  if (tagName === "br") {
    return "\n";
  }

  const content = serializeMarkdownInlineChildren(node);
  if (!content) {
    return "";
  }

  if (tagName === "strong" || tagName === "b") {
    return `**${content}**`;
  }
  if (tagName === "em" || tagName === "i") {
    return `*${content}*`;
  }
  if (tagName === "del" || tagName === "s") {
    return `~~${content}~~`;
  }
  if (tagName === "code") {
    return wrapInlineMarkdownCode(content);
  }
  if (tagName === "a") {
    if (node.classList.contains("inline-code-link")) {
      const code = node.querySelector("code")?.textContent ?? content;
      return wrapInlineMarkdownCode(code);
    }

    const href =
      node.getAttribute(MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE) ??
      node.getAttribute("href");
    const destination =
      href && isSerializableMarkdownHref(href)
        ? formatSafeMarkdownLinkDestination(href)
        : null;
    return destination
      ? `[${escapeMarkdownLinkLabel(content)}](${destination})`
      : content;
  }

  return content;
}

function escapeMarkdownLinkLabel(label: string) {
  return label.replace(/[\\[\]]/g, "\\$&");
}

// Serializer-side href policy for links that already exist in the
// trusted document. This intentionally shares the paste-time
// protocol surface with `isSafePastedMarkdownHref`, but keeps
// Windows drive-letter paths round-trippable because the link is
// authored document content rather than untrusted pasted input.
function isSerializableMarkdownHref(href: string) {
  const trimmed = href.trim();
  if (!trimmed || formatSafeMarkdownLinkDestination(trimmed) === null) {
    return false;
  }

  const decodedHref = safeDecodeMarkdownHref(trimmed);
  if (
    looksLikeSerializableWindowsDriveHref(trimmed) ||
    looksLikeSerializableWindowsDriveHref(decodedHref)
  ) {
    return true;
  }

  const protocol =
    getMarkdownHrefProtocol(trimmed) ??
    (decodedHref !== trimmed ? getMarkdownHrefProtocol(decodedHref) : null);
  if (!protocol) {
    return true;
  }

  return SAFE_EXTERNAL_MARKDOWN_PROTOCOLS.has(protocol);
}

function formatSafeMarkdownLinkDestination(href: string) {
  const destination = href.trim();
  if (
    destination.length === 0 ||
    UNSAFE_MARKDOWN_LINK_DESTINATION_CHARACTERS.test(destination)
  ) {
    return null;
  }
  return destination.replace(/[\\()]/g, "\\$&");
}

export function serializeMarkdownList(list: HTMLElement, ordered: boolean) {
  return Array.from(list.children)
    .filter((child): child is HTMLElement => child instanceof HTMLElement && child.tagName.toLowerCase() === "li")
    .map((item, index) => {
      const marker = ordered ? `${index + 1}.` : "-";
      const itemText = serializeMarkdownInlineChildren(item).trim();
      const nestedBlocks = Array.from(item.children)
        .filter((child) => child instanceof HTMLElement && ["ul", "ol"].includes(child.tagName.toLowerCase()))
        .map((child) =>
          serializeMarkdownBlockNode(child).split("\n").map((line) => `  ${line}`).join("\n"),
        )
        .filter((markdown) => markdown.trim().length > 0);
      return [`${marker} ${itemText}`, ...nestedBlocks].join("\n");
    })
    .join("\n");
}

export function serializeMarkdownTable(table: HTMLElement) {
  const rows = Array.from(table.querySelectorAll("tr")).map((row) =>
    Array.from(row.children).map((cell) => serializeMarkdownInlineChildren(cell as HTMLElement).trim()),
  );
  if (rows.length === 0) {
    return "";
  }

  const header = rows[0];
  const separator = header.map(() => "---");
  const bodyRows = rows.slice(1);
  return [header, separator, ...bodyRows]
    .map((row) => `| ${row.join(" | ")} |`)
    .join("\n");
}

export function wrapInlineMarkdownCode(content: string) {
  const fence = buildMarkdownBacktickFence(content, 1);
  if (content.startsWith("`") || content.endsWith("`")) {
    // CommonMark code spans need a single inner space when content touches a
    // fence backtick; otherwise the parser treats one fence backtick as content.
    return `${fence} ${content} ${fence}`;
  }
  return `${fence}${content}${fence}`;
}

// CommonMark fenced code and code spans need a delimiter longer than any
// backtick run inside the content, so the serializer uses longest run + 1.
function buildMarkdownBacktickFence(content: string, minimumLength: number) {
  let longestRun = 0;
  for (const match of content.matchAll(/`+/g)) {
    longestRun = Math.max(longestRun, match[0].length);
  }
  return "`".repeat(Math.max(minimumLength, longestRun + 1));
}
