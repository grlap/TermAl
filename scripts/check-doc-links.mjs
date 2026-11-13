// Owns the docs link check: every relative Markdown link in docs/**/*.md must
// reach an existing file or directory, spelled with its exact case, and a
// #fragment on a Markdown target (or on the same file) must match a heading
// slug as GitHub renders it, or an HTML id/name anchor. Run it from the
// repository with no arguments: `node scripts/check-doc-links.mjs` (docs/test.md
// names this exact argv as a registered Lint check). Does not own external
// URLs, which it skips, link wording, or files outside docs/ other than as link
// targets. New file, not split from another.
import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

// GitHub's heading slug (github-slugger): lowercase, drop punctuation and
// symbols except `-` and `_`, then turn each space into `-`.
export function githubSlug(text) {
  return text
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc}\- ]/gu, "")
    .replace(/ /g, "-");
}

const asciiPunctuation = /\\([!-/:-@[-`{-~])/g;

// The text a heading renders to. Code span contents are kept exactly as
// written, so the markup removed around them (HTML tags, link syntax, emphasis
// markers and backslash escapes) never reaches into code.
export function renderedHeadingText(raw) {
  const codes = [];
  let text = raw.replace(/(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)/g, (_, _ticks, content) => {
    const trimmed = /^ .* $/s.test(content) && content.trim() !== "" ? content.slice(1, -1) : content;
    codes.push(trimmed);
    return `\u0000${codes.length - 1}\u0000`;
  });
  text = linkLabels(text.replace(/<[^>]+>/g, ""))
    .replace(/(^|[^\p{L}\p{N}])[*_]{1,3}([^*_]+?)[*_]{1,3}(?=$|[^\p{L}\p{N}])/gu, "$1$2")
    .replace(asciiPunctuation, "$1");
  return text.replace(/\u0000(\d+)\u0000/g, (_, index) => codes[Number(index)]).trim();
}

const fenceOpen = /^ {0,3}(`{3,}|~{3,})/;
const atxHeading = /^ {0,3}(#{1,6})[ \t]+(.*?)(?:[ \t]+#+)?[ \t]*$/;
const setextUnderline = /^ {0,3}(=+|-+)[ \t]*$/;
const htmlAnchor = /<a\s[^>]*\b(?:id|name)\s*=\s*["']([^"']+)["']/gi;
const referenceDefinition = /^ {0,3}\[([^\]\n]+)\]:[ \t]*(<[^>\n]*>|\S+)/gm;
const external = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

function blankOut(text) {
  return text.replace(/[^\n]/g, " ");
}

// The document with fenced code blocks, HTML comments and code spans replaced
// by spaces. Line breaks are kept, so every offset and line number still
// matches the original text.
export function maskCode(text) {
  const lines = text.split("\n");
  let fence = null;
  for (let index = 0; index < lines.length; index += 1) {
    const opener = fenceOpen.exec(lines[index]);
    if (fence) {
      if (opener && opener[1][0] === fence[0] && opener[1].length >= fence.length
        && lines[index].trim() === opener[1]) {
        fence = null;
      }
      lines[index] = blankOut(lines[index]);
    } else if (opener) {
      fence = opener[1];
      lines[index] = blankOut(lines[index]);
    }
  }
  let masked = lines.join("\n").replace(/<!--[\s\S]*?-->/g, blankOut);
  // A code span runs from a backtick string to the next backtick string of
  // the same length, across line breaks but never across a blank line. A
  // backtick string with no match is literal text.
  const out = masked.split("");
  let position = 0;
  while (position < masked.length) {
    if (masked[position] !== "`" || (position > 0 && masked[position - 1] === "\\")) {
      position += 1;
      continue;
    }
    let length = 0;
    while (masked[position + length] === "`") length += 1;
    let search = position + length;
    let close = -1;
    while (search < masked.length) {
      if (startsBlankLine(masked, search)) break;
      if (masked[search] === "`") {
        let run = 0;
        while (masked[search + run] === "`") run += 1;
        if (run === length) {
          close = search;
          break;
        }
        search += run;
        continue;
      }
      search += 1;
    }
    if (close === -1) {
      position += length;
      continue;
    }
    for (let index = position; index < close + length; index += 1) {
      if (out[index] !== "\n") out[index] = " ";
    }
    position = close + length;
  }
  return out.join("");
}

// A lookup from an offset to its 1-based line, built once per document.
function lineIndex(text) {
  const starts = [0];
  for (let index = 0; index < text.length; index += 1) if (text[index] === "\n") starts.push(index + 1);
  return (offset) => {
    let low = 0;
    let high = starts.length - 1;
    while (low < high) {
      const middle = (low + high + 1) >> 1;
      if (starts[middle] <= offset) low = middle;
      else high = middle - 1;
    }
    return low + 1;
  };
}

function withoutBom(text) {
  return text.replace(/^﻿/, "").replace(/\r\n/g, "\n");
}

// Whether the line break at `index` is followed by a blank line, which ends a
// paragraph and so any code span, label or title still open.
function startsBlankLine(text, index) {
  if (text[index] !== "\n") return false;
  let position = index + 1;
  while (text[position] === " " || text[position] === "\t") position += 1;
  return position >= text.length || text[position] === "\n";
}

// A link destination starting at `index` (just after `(`): `<...>`, or a run
// with balanced parentheses and backslash escapes, then an optional title and
// the closing `)`. Returns the destination and the offset after `)`.
function parseDestination(text, index) {
  let position = index;
  const skipSpace = () => {
    while (text[position] === " " || text[position] === "\t") position += 1;
    if (text[position] === "\n" && !startsBlankLine(text, position)) {
      position += 1;
      while (text[position] === " " || text[position] === "\t") position += 1;
    }
  };
  skipSpace();
  let target = "";
  if (text[position] === "<") {
    const close = text.indexOf(">", position);
    const newline = text.indexOf("\n", position);
    if (close === -1 || (newline !== -1 && newline < close)) return null;
    target = text.slice(position + 1, close);
    position = close + 1;
  } else {
    let depth = 0;
    while (position < text.length) {
      const character = text[position];
      if (character === "\\" && position + 1 < text.length) {
        target += character + text[position + 1];
        position += 2;
        continue;
      }
      if (/\s/.test(character)) break;
      if (character === "(") depth += 1;
      if (character === ")") {
        if (depth === 0) break;
        depth -= 1;
      }
      target += character;
      position += 1;
    }
    if (depth !== 0) return null;
  }
  skipSpace();
  const opener = text[position];
  if (opener === '"' || opener === "'" || opener === "(") {
    const closer = opener === "(" ? ")" : opener;
    position += 1;
    while (position < text.length && text[position] !== closer) {
      if (startsBlankLine(text, position)) return null;
      position += text[position] === "\\" ? 2 : 1;
    }
    if (position >= text.length) return null;
    position += 1;
    skipSpace();
  }
  if (text[position] !== ")") return null;
  return { target: target.replace(asciiPunctuation, "$1"), end: position + 1 };
}

// Each inline link or image in `text`: where its `[` stands, where its label
// ends, its destination, and the offset after its closing `)`.
function inlineLinks(text) {
  const links = [];
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] !== "[" || (index > 0 && text[index - 1] === "\\")) continue;
    let depth = 1;
    let position = index + 1;
    while (position < text.length && depth > 0) {
      const character = text[position];
      if (character === "\\") {
        position += 2;
        continue;
      }
      if (startsBlankLine(text, position)) break;
      if (character === "[") depth += 1;
      if (character === "]") depth -= 1;
      if (depth > 0) position += 1;
    }
    if (depth !== 0 || text[position + 1] !== "(") continue;
    const destination = parseDestination(text, position + 2);
    if (destination) links.push({ start: index, labelEnd: position, ...destination });
  }
  return links;
}

// `text` with each inline link or image replaced by its label, using the same
// destination rules as link checking.
function linkLabels(text) {
  let result = "";
  let cursor = 0;
  for (const link of inlineLinks(text)) {
    if (link.start < cursor) continue;
    const imageMark = link.start > 0 && text[link.start - 1] === "!" ? 1 : 0;
    result += text.slice(cursor, link.start - imageMark) + text.slice(link.start + 1, link.labelEnd);
    cursor = link.end;
  }
  return result + text.slice(cursor);
}

// Every inline link or image destination and every reference definition in a
// Markdown file, outside code, with the line where its `[` stands. Labels may
// wrap across line breaks inside one paragraph.
export function markdownLinks(text) {
  const masked = maskCode(withoutBom(text));
  const lineOf = lineIndex(masked);
  const links = [];
  for (const link of inlineLinks(masked)) {
    if (link.target !== "") links.push({ line: lineOf(link.start), target: link.target });
  }
  for (const match of masked.matchAll(referenceDefinition)) {
    if (match[1].startsWith("^")) continue;
    const target = match[2].startsWith("<") ? match[2].slice(1, -1) : match[2];
    links.push({ line: lineOf(match.index), target });
  }
  return links.sort((left, right) => left.line - right.line);
}

// Every anchor a Markdown file offers: heading slugs, with repeats resolved as
// github-slugger does (a slug already taken gets the next free -N suffix), and
// HTML id/name attributes.
export function markdownAnchors(text) {
  const anchors = new Set();
  const occurrences = new Map();
  const addSlug = (heading) => {
    const original = githubSlug(renderedHeadingText(heading));
    let slug = original;
    while (occurrences.has(slug)) {
      const next = occurrences.get(original) + 1;
      occurrences.set(original, next);
      slug = `${original}-${next}`;
    }
    occurrences.set(slug, 0);
    anchors.add(slug);
  };
  const original = withoutBom(text).split("\n");
  const masked = maskCode(original.join("\n")).split("\n");
  // Heading structure is read from the masked lines, so nothing in fenced
  // code, comments or code spans counts; masking keeps every line's length,
  // so the heading text is then taken from the original line with its code.
  for (let index = 0; index < original.length; index += 1) {
    const line = masked[index];
    for (const match of line.matchAll(htmlAnchor)) anchors.add(match[1]);
    const atx = atxHeading.exec(original[index]);
    if (atx && atxHeading.test(line)) {
      addSlug(atx[2]);
      continue;
    }
    const next = masked[index + 1];
    const previous = masked[index - 1];
    if (next !== undefined && setextUnderline.test(next)
      && line.trim() !== "" && !/^ {0,3}([-*+>|]|\d+[.)])\s/.test(line)
      && (previous === undefined || previous.trim() === "")) {
      addSlug(original[index].trim());
    }
  }
  return anchors;
}

function decode(part) {
  try {
    return decodeURIComponent(part);
  } catch {
    return part;
  }
}

function markdownFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...markdownFiles(path));
    else if (entry.isFile() && entry.name.toLowerCase().endsWith(".md")) files.push(path);
  }
  return files.sort();
}

// Whether `destination` exists inside `root` with every path segment spelled
// in its exact case, so case-insensitive file systems give the same verdict
// as GitHub.
function existsExactly(root, destination, listings) {
  const inside = relative(root, destination);
  if (inside === "") return true;
  if (inside === ".." || inside.startsWith(`..${sep}`) || isAbsolute(inside)) return null;
  let current = root;
  for (const segment of inside.split(sep).filter(Boolean)) {
    if (!listings.has(current)) {
      try {
        listings.set(current, new Set(readdirSync(current)));
      } catch {
        listings.set(current, new Set());
      }
    }
    if (!listings.get(current).has(segment)) return false;
    current = join(current, segment);
  }
  return true;
}

// Checks every Markdown file under `root/docsDirectory` and returns the counts
// and each unresolved link.
export function checkDocLinks(root, docsDirectory = "docs") {
  const docs = join(root, docsDirectory);
  if (!existsSync(docs) || !statSync(docs).isDirectory()) {
    throw new Error(`no ${docsDirectory} directory under ${root}`);
  }
  const anchorCache = new Map();
  const anchorsOf = (path) => {
    if (!anchorCache.has(path)) anchorCache.set(path, markdownAnchors(readFileSync(path, "utf8")));
    return anchorCache.get(path);
  };
  const listings = new Map();
  const problems = [];
  let linkCount = 0;
  const files = markdownFiles(docs);
  for (const file of files) {
    const shown = relative(root, file).split(sep).join("/");
    for (const { line, target } of markdownLinks(readFileSync(file, "utf8"))) {
      if (external.test(target)) continue;
      linkCount += 1;
      const hash = target.indexOf("#");
      const pathPart = decode(hash === -1 ? target : target.slice(0, hash));
      const fragment = hash === -1 ? null : decode(target.slice(hash + 1));
      if (pathPart.includes("\\")) {
        problems.push({ file: shown, line, target, reason: "backslash in path (GitHub reads it literally; use /)" });
        continue;
      }
      const destination = pathPart === ""
        ? file
        : pathPart.startsWith("/") ? join(root, pathPart) : resolve(dirname(file), pathPart);
      const exists = existsExactly(root, destination, listings);
      if (exists === null) {
        problems.push({ file: shown, line, target, reason: "outside the repository" });
        continue;
      }
      if (!exists) {
        problems.push({ file: shown, line, target, reason: "missing file" });
        continue;
      }
      if (fragment === null || fragment === "") continue;
      if (!statSync(destination).isFile() || !destination.toLowerCase().endsWith(".md")) continue;
      if (!anchorsOf(destination).has(fragment)) {
        const where = relative(root, destination).split(sep).join("/");
        problems.push({ file: shown, line, target, reason: `missing anchor #${fragment} in ${where}` });
      }
    }
  }
  return { files: files.length, links: linkCount, problems };
}

// The command: no arguments, the repository root taken from this script's
// location. Exit 0 when every link resolves, 1 when any does not, 2 on misuse.
export function runCheckDocLinks(root, args, write = (line) => process.stdout.write(`${line}\n`)) {
  if (args.length > 0) {
    write("usage: node scripts/check-doc-links.mjs (no arguments)");
    return 2;
  }
  let result;
  try {
    result = checkDocLinks(root);
  } catch (error) {
    write(`check-doc-links: ${error.message}`);
    return 2;
  }
  for (const problem of result.problems) {
    write(`${problem.file}:${problem.line}: ${problem.target}: ${problem.reason}`);
  }
  write(`check-doc-links: ${result.files} files, ${result.links} relative links, ${result.problems.length} unresolved`);
  return result.problems.length === 0 ? 0 : 1;
}

// Node resolves a module's own URL through links while argv keeps the caller's
// spelling, so both are compared as canonical paths.
function invokedDirectly() {
  try {
    return Boolean(process.argv[1])
      && realpathSync.native(resolve(process.argv[1])) === realpathSync.native(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedDirectly()) {
  const script = realpathSync.native(fileURLToPath(import.meta.url));
  process.exitCode = runCheckDocLinks(resolve(dirname(script), ".."), process.argv.slice(2));
}
