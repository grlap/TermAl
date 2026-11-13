// Exercises the docs link check on fixture trees it writes itself; it never
// reads the repository's own docs or agent instruction files.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  checkDocLinks,
  githubSlug,
  markdownAnchors,
  renderedHeadingText,
  runCheckDocLinks,
} from "./check-doc-links.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const scriptFile = fileURLToPath(new URL("./check-doc-links.mjs", import.meta.url));

function scratch(t) {
  const root = mkdtempSync(join(testTempDirectory(), "doc-links-fixture-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function write(root, files) {
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
  return root;
}

function fixture(t, files) {
  return write(scratch(t), files);
}

function reasons(result) {
  return result.problems.map((problem) => `${problem.file}:${problem.line} ${problem.target} ${problem.reason}`);
}

test("links that reach existing files and headings resolve, including underscore and punctuation slugs", (t) => {
  const root = fixture(t, {
    "docs/a.md": [
      "# A",
      "See [b](b.md), [section](b.md#canonical-native-source_id-and-credit),",
      "[code heading](b.md#the-run-function-runs), [here](#local-part) and [src](../src/main.rs).",
      "## Local part",
    ].join("\n"),
    "docs/b.md": "# B\n\n### Canonical native source_id and credit\n\n## The `run()` function: runs!\n",
    "src/main.rs": "fn main() {}\n",
  });
  const result = checkDocLinks(root);
  assert.deepEqual(reasons(result), []);
  assert.equal(result.files, 2);
  assert.equal(result.links, 5);
});

test("a link to a missing file or a missing anchor is reported with its file and line", (t) => {
  const root = fixture(t, {
    "docs/a.md": "# A\n\n[gone](missing.md)\n[wrong](b.md#canonical-native-source-id-and-credit)\n",
    "docs/b.md": "### Canonical native source_id and credit\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), [
    "docs/a.md:3 missing.md missing file",
    "docs/a.md:4 b.md#canonical-native-source-id-and-credit missing anchor #canonical-native-source-id-and-credit in docs/b.md",
  ]);
});

test("repeated headings take the next free suffix, as github-slugger assigns them", () => {
  assert.deepEqual([...markdownAnchors("# Foo\n\n# Foo\n\n# Foo-1\n\n# Foo\n")], ["foo", "foo-1", "foo-1-1", "foo-2"]);
});

test("code inside a heading keeps its text when the slug is made", () => {
  assert.equal(githubSlug(renderedHeadingText("`_private_` helpers")), "_private_-helpers");
  assert.equal(githubSlug(renderedHeadingText("The `Vec<T>` type")), "the-vect-type");
  assert.equal(githubSlug(renderedHeadingText("`__init__` and *emphasis*")), "__init__-and-emphasis");
  assert.equal(githubSlug(renderedHeadingText("Canonical `source_id` and EO credit")), "canonical-source_id-and-eo-credit");
  assert.equal(githubSlug(renderedHeadingText("Ordinary committed-input runs (`--input commit`)")), "ordinary-committed-input-runs---input-commit");
  assert.equal(githubSlug(renderedHeadingText("See [the guide](x.md) *now*")), "see-the-guide-now");
});

test("link destinations keep balanced parentheses and escapes", (t) => {
  const root = fixture(t, {
    "docs/a.md": "# A\n\n[guide](guide(v2).md) [gone](other(v2).md) [esc](guide\\(v2\\).md)\n",
    "docs/guide(v2).md": "# Guide\n",
    "docs/other(v2": "not the target\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/a.md:3 other(v2).md missing file"]);
});

test("links inside fenced code, comments and code spans, even spans that wrap lines, are not checked", (t) => {
  const root = fixture(t, {
    "docs/a.md": [
      "# A",
      "```md",
      "[fenced](nowhere.md)",
      "```",
      "Inline `[code](nowhere.md)` and a span `that wraps",
      "[example](missing.md) across lines` and <!-- [comment](nowhere.md) -->.",
      "External [web](https://example.com/x.md) and [mail](mailto:a@b.c).",
    ].join("\n"),
  });
  const result = checkDocLinks(root);
  assert.deepEqual(reasons(result), []);
  assert.equal(result.links, 0);
});

test("a link whose label wraps across a line break is still checked, at the line where it starts", (t) => {
  const root = fixture(t, {
    "docs/a.md": "# A\n\nSee [the wrapped\nlabel](gone.md) here.\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/a.md:3 gone.md missing file"]);
});

test("a target must match the file's case exactly, and a backslash in a path is reported", (t) => {
  const root = fixture(t, {
    "docs/a.md": "# A\n\n[case](B.md) [ok](b.md) [slash](sub\\b.md)\n",
    "docs/b.md": "# B\n",
    "docs/sub/b.md": "# B\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), [
    "docs/a.md:3 B.md missing file",
    "docs/a.md:3 sub\\b.md backslash in path (GitHub reads it literally; use /)",
  ]);
});

test("HTML id anchors, setext headings and reference definitions are understood, and footnotes are not links", (t) => {
  const root = fixture(t, {
    "docs/a.md": [
      'Intro <a id="custom-spot"></a>',
      "",
      "Setext title",
      "============",
      "",
      "[spot](#custom-spot) [title][ref] and a footnote[^n].",
      "",
      "[ref]: #setext-title",
      "[bad]: <b.md#nope>",
      "[^n]: not a link target",
    ].join("\n"),
    "docs/b.md": "# B\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/a.md:9 b.md#nope missing anchor #nope in docs/b.md"]);
});

test("a link in a heading becomes its label even when its target has parentheses", (t) => {
  assert.equal(githubSlug(renderedHeadingText("See [guide](guide(v2).md)")), "see-guide");
  const root = fixture(t, {
    "docs/a.md": "# See [guide](guide(v2).md)\n\n[ok](#see-guide) [bad](#see-guidemd)\n",
    "docs/guide(v2).md": "# Guide\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/a.md:3 #see-guidemd missing anchor #see-guidemd in docs/a.md"]);
});

test("a title with escaped quotes does not hide its link", (t) => {
  const root = fixture(t, {
    "docs/a.md": '# A\n\n[gone](missing.md "a \\"quoted\\" title") [ok](#a \'it\\\'s\')\n',
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/a.md:3 missing.md missing file"]);
});

test("a byte order mark does not hide a file's first heading or its links", (t) => {
  const root = fixture(t, {
    "docs/a.md": "﻿# First heading\n[self](#first-heading)\n",
    "docs/b.md": "﻿[gone](missing.md)\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/b.md:1 missing.md missing file"]);
});

test("root-relative, angle-bracket, directory and percent-encoded targets resolve; escaping the repository does not", (t) => {
  const root = fixture(t, {
    "docs/a.md": [
      "# A",
      "[root](/src/main.rs) [angle](<b c.md#b>) [dir](sub) [dir-anchor](sub#x) [encoded](b%20c.md)",
      "[up](../../outside.md) [dotted](../..cache/x.md)",
    ].join("\n"),
    "docs/b c.md": "# B\n",
    "docs/sub/x.md": "# X\n",
    "src/main.rs": "fn main() {}\n",
    "..cache/x.md": "# X\n",
  });
  assert.deepEqual(reasons(checkDocLinks(root)), ["docs/a.md:3 ../../outside.md outside the repository"]);
});

test("the command exits 0 when every link resolves, 1 when one does not, and 2 when given arguments", (t) => {
  const clean = fixture(t, { "docs/a.md": "# A\n[self](#a)\n" });
  const broken = fixture(t, { "docs/a.md": "# A\n[gone](missing.md)\n" });
  const output = [];
  assert.equal(runCheckDocLinks(clean, [], (line) => output.push(line)), 0);
  assert.equal(output.at(-1), "check-doc-links: 1 files, 1 relative links, 0 unresolved");
  output.length = 0;
  assert.equal(runCheckDocLinks(broken, [], (line) => output.push(line)), 1);
  assert.deepEqual(output, ["docs/a.md:2: missing.md: missing file", "check-doc-links: 1 files, 1 relative links, 1 unresolved"]);
  assert.equal(runCheckDocLinks(clean, ["--fix"], () => {}), 2);
});

test("the script checks its own repository when started directly or through a linked directory", (t) => {
  // Node resolves a module's own URL through links while argv keeps the
  // caller's spelling; a Windows junction is the same kind of alias.
  const base = scratch(t);
  const real = write(join(base, "real"), { "docs/a.md": "# A\n[gone](missing.md)\n" });
  mkdirSync(join(real, "scripts"), { recursive: true });
  copyFileSync(scriptFile, join(real, "scripts", "check-doc-links.mjs"));
  const linked = join(base, "linked");
  symlinkSync(real, linked, "junction");
  for (const start of [real, linked]) {
    const child = spawnSync(process.execPath, [join(start, "scripts", "check-doc-links.mjs")], { encoding: "utf8" });
    assert.equal(child.status, 1, `${start}: ${child.stderr}`);
    assert.equal(child.stdout, "docs/a.md:2: missing.md: missing file\ncheck-doc-links: 1 files, 1 relative links, 1 unresolved\n");
  }
});
