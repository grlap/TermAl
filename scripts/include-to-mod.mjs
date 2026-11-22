#!/usr/bin/env node
// Turns include!() fragments of src/main.rs into ordinary Rust modules, a
// chosen set at a time, and proves that a converted tree differs from its base
// only by module wiring and visibility.
//
//   node scripts/include-to-mod.mjs analyze ROOT
//   node scripts/include-to-mod.mjs convert ROOT UNIT[,UNIT...] [--write] [--expose NAME[,NAME...]]
//   node scripts/include-to-mod.mjs verify BASE_ROOT NEW_ROOT [--expose NAME[,NAME...]]
//
// A unit is a file that src/main.rs includes at its top level, together with
// every .rs file that file includes in turn. For each unit `convert` replaces
// `include!("x.rs");` in src/main.rs with `mod x;` and, when the module has a
// name to re-export, `use x::*;`, under #[cfg(test)] when only test code
// names its items. It adds one `use super::*;` to the file when
// the file, or a child module it mounts, names anything the crate root
// provides, and it inserts `pub(crate)` where the rule below asks for it. It
// refuses the whole request, before writing anything, when a unit needs a
// decision it cannot make (see `planUnit`'s hazards); those units are
// converted by hand. Its report names the units that hold
// platform-conditional code (`platformUnits`): a batch with any needs a
// compile on the other platform too, a batch with none does not.
//
// Where `pub(crate)` goes:
//   - every module-level `use` and every trait, because another file can
//     depend on them without naming them;
//   - a fn, const, static, struct, enum, type alias, inline mod, inherent
//     method or associated const whose name occurs outside the unit, as an
//     identifier or as a word in a string literal, or after `crate::`
//     anywhere. Every src file is read whatever its cfg, so the answer does
//     not depend on the platform or on cfg(test). A method or associated
//     const of a type the unit defines follows that type: while the type is
//     private, so are they;
//   - a named field of a visible struct by the same test; every field of a
//     tuple struct, and of a struct that is built with `..base` outside the
//     unit;
//   - a local type named in the signature of a visible item.
//
// `verify` checks a converted tree in two ways. It converts the base again
// and requires every changed file to equal that conversion, formatted with
// rustfmt; so nothing was inserted anywhere the plan does not put it, and no
// re-export the plan calls for is missing. And, without relying on the plan,
// it takes the inserted tokens and the wiring out of the candidate again and
// requires the result, formatted, to equal the base file, formatted; so the
// conversion added nothing else. Formatting is part of a conversion because
// cargo fmt skips include!() files and reaches them once they are modules.
// A fragment that a converted unit still includes stays out of cargo fmt's
// reach, so `verify` takes it formatted or exactly as converted.
// A unit converted by hand cannot pass `verify` and is reviewed as hand-
// written code.
//
// It never touches anything outside ROOT/src, never runs Cargo, never
// computes a hash or an identity of the tree, and never rewrites an
// expression, a comment or a name. It is a migration aid: remove it, with its
// test, in the change that converts the last fragment.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// ---------------------------------------------------------------- lexer
const isIdStart = (c) => /[A-Za-z_]/.test(c) || c > "\x7f";
const isIdCont = (c) => /[A-Za-z0-9_]/.test(c) || c > "\x7f";
// The name an identifier token stands for: `r#type` and `type` are one name.
const plain = (text) => (text.startsWith("r#") ? text.slice(2) : text);

// Rust tokens with their offsets. Kinds: comment, str, char, lifetime, ident,
// num, punct. `->`, `=>` and `::` are single tokens; other punctuation is one
// character each, so `<` and `>` can be counted as generic brackets.
export function lex(src) {
  const toks = [];
  const n = src.length;
  let i = 0;
  const push = (k, s, e) => toks.push({ k, s, e, t: src.slice(s, e) });
  const scanString = (j) => {
    while (j < n && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
    return j + 1;
  };
  while (i < n) {
    const c = src[i];
    if (c === " " || c === "\t" || c === "\n" || c === "\r") { i++; continue; }
    if (c === "/" && src[i + 1] === "/") {
      let j = src.indexOf("\n", i);
      if (j < 0) j = n;
      push("comment", i, src[j - 1] === "\r" ? j - 1 : j);
      i = j;
      continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < n && depth) {
        if (src.startsWith("/*", j)) { depth++; j += 2; }
        else if (src.startsWith("*/", j)) { depth--; j += 2; }
        else j++;
      }
      push("comment", i, j);
      i = j;
      continue;
    }
    if (c === "r" || c === "b" || c === "c") {
      const raw = /^(?:br|cr|r)(#*)"/.exec(src.slice(i, i + 80));
      if (raw) {
        const close = '"' + raw[1];
        let j = src.indexOf(close, i + raw[0].length);
        if (j < 0) throw new Error(`unterminated raw string at offset ${i}`);
        j += close.length;
        push("str", i, j);
        i = j;
        continue;
      }
      if ((c === "b" || c === "c") && src[i + 1] === '"') {
        const j = scanString(i + 2);
        push("str", i, j);
        i = j;
        continue;
      }
      if (c === "b" && src[i + 1] === "'") {
        let j = i + 2;
        while (j < n && src[j] !== "'") j += src[j] === "\\" ? 2 : 1;
        push("char", i, j + 1);
        i = j + 1;
        continue;
      }
    }
    if (c === '"') {
      const j = scanString(i + 1);
      push("str", i, j);
      i = j;
      continue;
    }
    if (c === "'") {
      if (src[i + 1] === "\\") {
        let j = i + 3;
        while (j < n && src[j] !== "'") j++;
        push("char", i, j + 1);
        i = j + 1;
        continue;
      }
      const len = src.codePointAt(i + 1) > 0xffff ? 2 : 1;
      if (src[i + 1 + len] === "'") {
        push("char", i, i + 2 + len);
        i += 2 + len;
        continue;
      }
      let j = i + 1;
      while (j < n && isIdCont(src[j])) j++;
      push("lifetime", i, j);
      i = j;
      continue;
    }
    if (isIdStart(c)) {
      let j = i + 1;
      if (c === "r" && src[i + 1] === "#" && isIdStart(src[i + 2] ?? "")) j = i + 2;
      while (j < n && isIdCont(src[j])) j++;
      push("ident", i, j);
      i = j;
      continue;
    }
    if (/[0-9]/.test(c)) {
      let j = i + 1;
      while (j < n && /[0-9A-Za-z_]/.test(src[j])) j++;
      if (src[j] === "." && /[0-9]/.test(src[j + 1] ?? "")) {
        j++;
        while (j < n && /[0-9A-Za-z_]/.test(src[j])) j++;
      }
      push("num", i, j);
      i = j;
      continue;
    }
    const two = src.slice(i, i + 2);
    if (two === "->" || two === "=>" || two === "::") {
      push("punct", i, i + 2);
      i += 2;
      continue;
    }
    push("punct", i, i + 1);
    i++;
  }
  return toks;
}

const OPEN = { "(": ")", "[": "]", "{": "}" };
const CLOSE = new Set([")", "]", "}"]);
const isOpen = (t) => t.k === "punct" && Object.hasOwn(OPEN, t.t);
const isClose = (t) => t.k === "punct" && CLOSE.has(t.t);

// Index of the token that closes the group opened at T[p].
function matchGroup(T, p) {
  let depth = 0;
  for (let i = p; i < T.length; i++) {
    if (isOpen(T[i])) depth++;
    else if (isClose(T[i])) {
      depth--;
      if (depth === 0) return i;
    }
  }
  throw new Error(`unbalanced group at offset ${T[p].s}`);
}

// Index just past the `<...>` list that starts at T[p].
function skipAngles(T, p) {
  let depth = 0;
  for (let i = p; i < T.length; i++) {
    const t = T[i];
    if (isOpen(t)) { i = matchGroup(T, i); continue; }
    if (t.k !== "punct") continue;
    if (t.t === "<") depth++;
    else if (t.t === ">") {
      depth--;
      if (depth === 0) return i + 1;
    }
  }
  throw new Error(`unbalanced angle brackets at offset ${T[p].s}`);
}

// First index in [p, end) of a punctuation token in `set` outside any group.
function findOutsideGroups(T, p, end, set) {
  for (let i = p; i < end; i++) {
    const t = T[i];
    if (t.k !== "punct") continue;
    if (set.has(t.t)) return i;
    if (isOpen(t)) i = matchGroup(T, i);
  }
  return -1;
}

const MODIFIERS = new Set(["const", "async", "unsafe", "extern", "default", "safe"]);
const AFTER_CONST_MODIFIER = new Set(["fn", "async", "unsafe", "extern"]);

// The items between code-token indexes [p, end): a module body, an inherent
// impl body, a foreign block or the inside of thread_local!. Each item
// records where a visibility would go (`start`), the visibility it has
// (`vis`), its name and the token ranges later steps need.
export function parseItems(T, p, end, file) {
  const items = [];
  const fail = (i, message) => {
    const near = T.slice(i, i + 6).map((t) => t.t).join(" ");
    throw new Error(`${file}: ${message} near offset ${T[i]?.s} (${near})`);
  };
  while (p < end) {
    const item = { first: p, attrs: [] };
    while (T[p]?.t === "#" && (T[p + 1]?.t === "[" || (T[p + 1]?.t === "!" && T[p + 2]?.t === "["))) {
      const close = matchGroup(T, T[p + 1].t === "[" ? p + 1 : p + 2);
      item.attrs.push([p, close]);
      p = close + 1;
    }
    if (p >= end) break;
    if (T[p].t === ";") { p++; continue; }
    if (T[p].t === "pub" && T[p].k === "ident") {
      item.vis = [p, p];
      p++;
      if (T[p].t === "(") {
        const close = matchGroup(T, p);
        item.vis = [item.vis[0], close];
        p = close + 1;
      }
    }
    item.start = p;
    for (;;) {
      const t = T[p];
      if (!t || !MODIFIERS.has(t.t)) break;
      if (t.t === "const" && !AFTER_CONST_MODIFIER.has(T[p + 1]?.t)) break;
      if (t.t === "extern") {
        if (T[p + 1]?.t === "crate") break;
        if (T[p + 1]?.k === "str") p++;
        if (T[p + 1]?.t === "{") break;
      }
      p++;
    }
    const kw = T[p];
    if (!kw) fail(item.start, "unexpected end of items");
    const semicolon = () => {
      const e = findOutsideGroups(T, p, end, new Set([";"]));
      if (e < 0) fail(p, "no terminating semicolon");
      return e;
    };
    const braceOrSemicolon = () => {
      const o = findOutsideGroups(T, p, end, new Set(["{", ";"]));
      if (o < 0) fail(p, "no body");
      return o;
    };
    const named = (kind, at) => {
      item.kind = kind;
      item.name = plain(T[at].t);
      item.nameTok = at;
    };
    if (kw.t === "fn") {
      named("fn", p + 1);
      const o = braceOrSemicolon();
      item.sig = [item.start, o];
      p = T[o].t === "{" ? matchGroup(T, o) + 1 : o + 1;
    } else if (kw.t === "struct" || (kw.t === "union" && T[p + 1]?.k === "ident" && T[p + 2]?.t !== "!")) {
      named("struct", p + 1);
      let q = p + 2;
      if (T[q].t === "<") q = skipAngles(T, q);
      if (T[q].t === "(") {
        item.tuple = [q, matchGroup(T, q)];
        item.header = [item.start, q];
        p = findOutsideGroups(T, item.tuple[1] + 1, end, new Set([";"])) + 1;
      } else {
        const o = findOutsideGroups(T, q, end, new Set(["{", ";"]));
        item.header = [item.start, o];
        if (T[o].t === "{") {
          item.body = [o, matchGroup(T, o)];
          p = item.body[1] + 1;
        } else p = o + 1;
      }
    } else if (kw.t === "enum" || kw.t === "trait" || (kw.t === "auto" && T[p + 1]?.t === "trait")) {
      if (kw.t === "auto") p++;
      named(T[p].t, p + 1);
      const o = braceOrSemicolon();
      p = T[o].t === "{" ? matchGroup(T, o) + 1 : o + 1;
    } else if (kw.t === "type") {
      named("type", p + 1);
      const e = semicolon();
      item.sig = [item.start, e];
      p = e + 1;
    } else if (kw.t === "const" || kw.t === "static") {
      const q = T[p + 1].t === "mut" ? p + 2 : p + 1;
      named(kw.t, q);
      const e = semicolon();
      const eq = findOutsideGroups(T, q, e, new Set(["="]));
      item.sig = [item.start, eq < 0 ? e : eq];
      p = e + 1;
    } else if (kw.t === "mod") {
      named("mod", p + 1);
      if (T[p + 2].t === ";") {
        item.outOfLine = true;
        p += 3;
      } else {
        item.body = [p + 2, matchGroup(T, p + 2)];
        p = item.body[1] + 1;
      }
    } else if (kw.t === "use") {
      item.kind = "use";
      p = semicolon() + 1;
    } else if (kw.t === "impl") {
      item.kind = "impl";
      const o = braceOrSemicolon();
      item.header = [p, o];
      let q = p + 1;
      if (T[q].t === "<") q = skipAngles(T, q);
      item.isTrait = false;
      item.selfName = null; // last path segment of the type the impl is for
      for (let i = q; i < o; i++) {
        const t = T[i];
        if (t.t === "where") break;
        if (isOpen(t)) { i = matchGroup(T, i); continue; }
        if (t.t === "<") { i = skipAngles(T, i) - 1; continue; }
        // `for` introduces the self type of a trait impl; `for<'a>` is a bound
        if (t.t === "for" && T[i + 1]?.t !== "<") {
          item.isTrait = true;
          item.selfName = null;
        } else if (t.k === "ident" && t.t !== "dyn" && t.t !== "mut" && t.t !== "const") {
          item.selfName = plain(t.t);
        }
      }
      item.body = [o, matchGroup(T, o)];
      item.children = parseItems(T, o + 1, item.body[1], file);
      p = item.body[1] + 1;
    } else if (kw.t === "extern" && T[p + 1]?.t === "crate") {
      item.kind = "extern_crate";
      p = semicolon() + 1;
    } else if (kw.t === "extern" || (kw.k === "str" && T[p + 1]?.t === "{")) {
      item.kind = "foreign";
      const o = braceOrSemicolon();
      item.body = [o, matchGroup(T, o)];
      item.children = parseItems(T, o + 1, item.body[1], file);
      p = item.body[1] + 1;
    } else if (kw.k === "ident" && (T[p + 1]?.t === "!" || (T[p + 1]?.t === "::" && T[p + 2]?.k === "ident"))) {
      // a macro invocation in item position, possibly path-qualified
      while (T[p + 1]?.t === "::" && T[p + 2]?.k === "ident") p += 2;
      if (T[p + 1]?.t !== "!") fail(p, "unrecognised item");
      item.kind = "macro";
      item.macro = T[p].t;
      let q = p + 2;
      if (T[q].k === "ident") {
        item.name = plain(T[q].t);
        q++;
      }
      const close = matchGroup(T, q);
      p = close + 1;
      if (T[p]?.t === ";") p++;
      if (item.macro === "thread_local") item.children = parseItems(T, q + 1, close, file);
      if (item.macro === "include" && T[q + 1]?.k === "str") item.include = JSON.parse(T[q + 1].t);
    } else {
      fail(p, "unrecognised item");
    }
    item.last = p - 1;
    items.push(item);
  }
  return items;
}

// The named fields of a struct body, or the positional fields of a tuple
// struct, between the group tokens `open` and `close`.
function parseFields(T, open, close, tuple) {
  const fields = [];
  let p = open + 1;
  while (p < close) {
    while (T[p]?.t === "#" && T[p + 1]?.t === "[") p = matchGroup(T, p + 1) + 1;
    if (p >= close) break;
    const field = {};
    if (T[p].t === "pub" && T[p].k === "ident") {
      field.vis = true;
      p++;
      // `pub (A, B)` is a public tuple-typed field; a restriction holds one
      // of these words.
      if (T[p].t === "(" && ["crate", "super", "self", "in"].includes(T[p + 1]?.t)) p = matchGroup(T, p) + 1;
    }
    field.start = p;
    if (!tuple) {
      field.name = plain(T[p].t);
      p += 2;
    }
    field.typeStart = p;
    let angle = 0;
    for (; p < close; p++) {
      const t = T[p];
      if (isOpen(t)) { p = matchGroup(T, p); continue; }
      if (t.k !== "punct") continue;
      if (t.t === "<") angle++;
      else if (t.t === ">") angle = Math.max(0, angle - 1);
      else if (t.t === "," && angle === 0) break;
    }
    field.typeEnd = p;
    fields.push(field);
    p++;
  }
  return fields;
}

// ------------------------------------------------------------ source tree
function listRustFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listRustFiles(full));
    else if (entry.name.endsWith(".rs")) out.push(full);
  }
  return out;
}

const relativeName = (srcDir, full) => path.relative(srcDir, full).split(path.sep).join("/");

const ITEM_START = new Set([
  "fn", "struct", "enum", "union", "impl", "mod", "use", "const", "static", "type", "trait", "pub", "async", "unsafe", "extern", "let",
]);

// Index of the last token of what the attributes ending before T[p] apply
// to: an item or statement runs to its `;` or to the close of its first brace
// group; a field, a variant or a match arm also ends at a comma, and at the
// end of the group that holds it.
function attributedEnd(T, p) {
  const item = ITEM_START.has(T[p]?.t);
  for (let i = p; i < T.length; i++) {
    const t = T[i];
    if (t.k !== "punct") continue;
    if (t.t === ";" || (!item && t.t === ",")) return i;
    if (t.t === "{") return matchGroup(T, i);
    if (isClose(t)) return i - 1;
    if (isOpen(t)) i = matchGroup(T, i);
  }
  return T.length - 1;
}

// For each code token, 1 when it sits in something that carries #[cfg(test)]
// or #[cfg(all(test, ...))], from the attribute to the end of what it applies
// to. `#![cfg(test)]` covers the file.
function testCode(T) {
  const marks = new Uint8Array(T.length);
  for (let i = 0; i < T.length; i++) {
    if (T[i].t !== "#") continue;
    const inner = T[i + 1]?.t === "!";
    const open = inner ? i + 2 : i + 1;
    if (T[open]?.t !== "[" || T[open + 1]?.t !== "cfg" || T[open + 2]?.t !== "(") continue;
    const close = matchGroup(T, open + 2);
    let predicate = T.slice(open + 3, close);
    // the direct members of all(...)
    if (predicate[0]?.t === "all" && predicate[1]?.t === "(") {
      const members = [];
      for (let k = 2; k < predicate.length - 1; k++) {
        if (isOpen(predicate[k])) { k = matchGroup(predicate, k); members.push(null); continue; }
        if (predicate[k].t !== ",") members.push(predicate[k]);
      }
      // `not(test)` leaves `not` and a null behind, never a lone `test`
      predicate = members.some((t, k) => t?.t === "test" && members[k + 1] !== null) ? [{ t: "test" }] : [];
    }
    if (predicate.length !== 1 || predicate[0].t !== "test") continue;
    if (inner) return marks.fill(1);
    let p = close + 2; // past `)` and `]`
    while (T[p]?.t === "#" && T[p + 1]?.t === "[") p = matchGroup(T, p + 1) + 1;
    const end = attributedEnd(T, p);
    marks.fill(1, i, end + 1);
    i = end;
  }
  return marks;
}

// Every .rs file under ROOT/src, lexed, with an index of where each
// identifier is used.
export class SourceTree {
  constructor(root) {
    this.src = path.join(root, "src");
    this.files = new Map();
    for (const full of listRustFiles(this.src)) {
      const rel = relativeName(this.src, full);
      const text = fs.readFileSync(full, "utf8");
      const all = lex(text);
      this.files.set(rel, { rel, full, text, all, T: all.filter((t) => t.k !== "comment") });
    }
    for (const file of this.files.values()) file.testCode = testCode(file.T);
    this.testOnly = this.#testOnlyFiles();
    this.uses = new Map(); // identifier -> files that mention it
    this.plainUses = new Map(); // identifier -> files that mention it outside test code
    this.cratePath = new Set(); // NAME in `crate::NAME`
    this.plainCratePath = new Set(); // the same, outside test code
    this.structUpdates = new Map(); // struct name, or "*" when unknown -> files using `..base`
    for (const file of this.files.values()) this.#index(file);
  }

  // Whether token `index` of a file is compiled only for tests.
  inTestCode(rel, index) {
    return this.testOnly.has(rel) || this.files.get(rel).testCode[index] === 1;
  }

  // The files only a test build compiles: one that test code includes or
  // mounts as a module, with the files under that module's directory, and
  // whatever such a file mounts in turn.
  #testOnlyFiles() {
    const only = new Set();
    const mark = (rel) => {
      const base = rel.replace(/(\/mod)?\.rs$/, "");
      let grew = false;
      for (const other of this.files.keys()) {
        if ((other === rel || other.startsWith(`${base}/`)) && !only.has(other)) {
          only.add(other);
          grew = true;
        }
      }
      return grew;
    };
    for (let grew = true; grew; ) {
      grew = false;
      for (const file of this.files.values()) {
        const T = file.T;
        const dir = path.posix.dirname(file.rel);
        const whole = only.has(file.rel);
        let pathed = -1; // the `mod` a #[path] attribute belongs to
        for (let i = 0; i < T.length; i++) {
          if (T[i].t === "#" && T[i + 1]?.t === "[" && T[i + 2]?.t === "path" && T[i + 4]?.k === "str") {
            pathed = T.findIndex((t, k) => k > i && t.t === "mod");
            if ((whole || file.testCode[i]) && mark(path.posix.join(dir, JSON.parse(T[i + 4].t)))) grew = true;
          }
          if (!whole && !file.testCode[i]) continue;
          if (T[i].t === "include" && T[i + 1]?.t === "!" && T[i + 3]?.k === "str") {
            if (mark(path.posix.join(dir, JSON.parse(T[i + 3].t)))) grew = true;
          } else if (T[i].t === "mod" && T[i + 1]?.k === "ident" && T[i + 2]?.t === ";" && i !== pathed) {
            const own = /(^|\/)(main|lib|mod)\.rs$/.test(file.rel) ? dir : file.rel.replace(/\.rs$/, "");
            if (mark(`${path.posix.join(own, plain(T[i + 1].t))}.rs`)) grew = true;
          }
        }
      }
    }
    return only;
  }

  #index(file) {
    const note = (map, key) => {
      if (!map.has(key)) map.set(key, new Set());
      map.get(key).add(file.rel);
    };
    const T = file.T;
    for (let i = 0; i < T.length; i++) {
      const t = T[i];
      const plainCode = !this.inTestCode(file.rel, i);
      if (t.k === "ident") {
        note(this.uses, plain(t.t));
        if (plainCode) note(this.plainUses, plain(t.t));
        if (t.t === "crate" && T[i + 1]?.t === "::" && T[i + 2]?.k === "ident") {
          this.cratePath.add(plain(T[i + 2].t));
          if (plainCode) this.plainCratePath.add(plain(T[i + 2].t));
        }
      } else if (t.k === "str") {
        for (const word of t.t.matchAll(/[A-Za-z_][A-Za-z0-9_]*/g)) {
          note(this.uses, word[0]);
          if (plainCode) note(this.plainUses, word[0]);
        }
      } else if (t.t === "." && T[i + 1]?.t === "." && T[i + 2]?.t !== "." && T[i - 1]?.t !== ".") {
        // `..expr` right after `{` or `,`: a struct built from another value
        const prev = T[i - 1]?.t;
        const next = T[i + 2];
        if ((prev !== "," && prev !== "{") || !next || isClose(next) || next.t === "=") continue;
        let depth = 0;
        let j = i - 1;
        for (; j >= 0; j--) {
          if (isClose(T[j])) depth++;
          else if (isOpen(T[j])) {
            if (depth === 0) break;
            depth--;
          }
        }
        const name = j > 0 && T[j - 1].k === "ident" ? plain(T[j - 1].t) : "*";
        note(this.structUpdates, name === "Self" ? "*" : name);
      }
    }
  }

  usedOutside(name, inside) {
    for (const rel of this.uses.get(name) ?? []) if (!inside.has(rel)) return true;
    return false;
  }

  // The names a module gets from the crate root through `use super::*;`,
  // each with the files that provide it: what src/main.rs defines or
  // imports, what the fragments it includes define or import at module
  // level, and what the local modules it glob-imports make visible.
  rootNames() {
    if (this.rootNameIndex) return this.rootNameIndex;
    const names = new Map();
    const provide = (name, rel) => {
      if (name === "_") return; // `use Trait as _;` gives no name
      if (!names.has(name)) names.set(name, new Set());
      names.get(name).add(rel);
    };
    const seen = new Set();
    const collect = (rel, onlyVisible) => {
      const file = this.files.get(rel);
      if (!file || seen.has(rel)) return;
      seen.add(rel);
      const T = file.T;
      const visit = (items) => {
        for (const item of items) {
          if (item.kind === "macro" && item.macro === "include" && item.include?.endsWith(".rs")) {
            collect(path.posix.join(path.posix.dirname(rel), item.include), onlyVisible);
          } else if (item.kind === "foreign" || (item.kind === "macro" && item.macro === "thread_local")) {
            visit(item.children);
          } else if (item.kind === "use") {
            const glob = T[item.last - 1]?.t === "*" && T[item.start + 2]?.t === "::" && item.last - item.start === 4;
            const local = glob ? `${plain(T[item.start + 1].t)}.rs` : null;
            if (local && rel === "main.rs" && this.files.has(local)) collect(local, true);
            else if (!onlyVisible || item.vis) for (const leaf of useLeaves(T, item)) provide(leaf, rel);
          } else if (item.name && item.kind !== "impl" && item.kind !== "macro" && (!onlyVisible || item.vis)) {
            provide(item.name, rel);
          }
        }
      };
      visit(parseItems(T, 0, T.length, rel));
    };
    collect("main.rs", false);
    this.rootNameIndex = names;
    return names;
  }

  // The files src/main.rs includes at its top level, in order.
  mainUnits() {
    const main = this.files.get("main.rs");
    if (!main) throw new Error("src/main.rs not found");
    const items = parseItems(main.T, 0, main.T.length, "main.rs");
    return items
      .map((item, index) => ({ item, previous: items[index - 1], next: items[index + 1] }))
      .filter(({ item }) => item.kind === "macro" && item.macro === "include" && item.include?.endsWith(".rs"))
      .map(({ item, previous, next }) => ({
        file: item.include,
        cfgTest: item.attrs.some(([a, b]) => main.T.slice(a, b + 1).map((t) => t.t).join("") === "#[cfg(test)]"),
        otherAttrs: item.attrs.some(([a, b]) => main.T.slice(a, b + 1).map((t) => t.t).join("") !== "#[cfg(test)]"),
        // the text of the invocation itself, from `include` to its `;`
        range: [main.T[item.start].s, main.T[item.last].e],
        beforeUse: next?.kind === "use",
        // where an existing `mod x;` right above it ends, if there is one
        afterMod: previous?.kind === "mod" && previous.outOfLine ? main.T[previous.last].e : null,
      }));
  }
}

// The names a `use` item brings in: the last segment of each path, or its
// `as` alias; `self` in a braced list stands for the segment before the list.
function useLeaves(T, item) {
  const leaves = [];
  const lists = [];
  for (let i = item.start + 1; i < item.last; i++) {
    const t = T[i];
    if (t.t === "{") lists.push(T[i - 2]?.k === "ident" ? plain(T[i - 2].t) : null);
    else if (t.t === "}") lists.pop();
    else if (t.k === "ident" && [",", "}", ";"].includes(T[i + 1]?.t)) {
      const name = t.t === "self" ? lists[lists.length - 1] : plain(t.t);
      if (name) leaves.push(name);
    }
  }
  return leaves;
}

// `pub(super)` and `pub(in ...)` whose target would move when the file stops
// being pasted into the crate root. `modDepth` counts the inline modules
// around a token; a restriction that climbs to the file's top level, or names
// a path from the crate root, meant the crate root and would mean the new
// module.
function movedRestrictions(file, baseDepth) {
  const T = file.T;
  const found = [];
  const groups = [];
  let modDepth = baseDepth;
  for (let i = 0; i < T.length; i++) {
    const t = T[i];
    if (isOpen(t)) {
      const inlineMod = t.t === "{" && T[i - 1]?.k === "ident" && T[i - 2]?.t === "mod";
      groups.push(inlineMod);
      if (inlineMod) modDepth++;
    } else if (isClose(t)) {
      if (groups.pop()) modDepth--;
    } else if (t.t === "pub" && t.k === "ident" && T[i + 1]?.t === "(" && (T[i + 2]?.t === "super" || T[i + 2]?.t === "in")) {
      const inner = T.slice(i + 2, matchGroup(T, i + 1)).map((x) => x.t);
      const climbs = inner.filter((x) => x === "super").length;
      const fromCrate = inner.includes("crate") && inner.length > 2;
      if (fromCrate || climbs >= modDepth) found.push(`${file.rel}: pub(${inner.join("")}) at offset ${t.s} would change meaning`);
    }
  }
  return found;
}

const PLATFORM_PREDICATES = new Set(["windows", "unix", "target_os", "target_family", "target_arch"]);

// How many cfg conditions in a file depend on the platform. A unit with any
// holds code that one host's compiler never type-checks, so its batch needs a
// compile on the other platform as well.
function platformConditionals(file) {
  const T = file.T;
  let count = 0;
  for (let i = 0; i < T.length; i++) {
    if ((T[i].t !== "cfg" && T[i].t !== "cfg_attr") || T[i].k !== "ident") continue;
    const open = T[i + 1]?.t === "!" ? i + 2 : i + 1;
    if (T[open]?.t !== "(") continue;
    const close = matchGroup(T, open);
    if (T.slice(open + 1, close).some((t) => t.k === "ident" && PLATFORM_PREDICATES.has(t.t))) count++;
    i = close;
  }
  return count;
}

// ------------------------------------------------------------- planning
const TYPE_KINDS = new Set(["struct", "enum", "type", "trait"]);

// The edit plan for one unit: the files it spans, where a visibility token
// goes, and the hazards that make it a hand conversion.
export function planUnit(tree, unitFile, expose = new Set()) {
  const unitFiles = new Set();
  const parsed = new Map(); // rel -> { file, items, context }
  const hazards = [];
  const childModules = [];
  const stem = unitFile.replace(/\.rs$/, "");
  const load = (rel, context) => {
    const file = tree.files.get(rel);
    if (!file) { hazards.push(`${unitFile}: include target ${rel} not found`); return; }
    if (unitFiles.has(rel)) { hazards.push(`${rel}: included twice`); return; }
    unitFiles.add(rel);
    const items = parseItems(file.T, 0, file.T.length, rel);
    parsed.set(rel, { file, items, context });
    hazards.push(...movedRestrictions(file, context === "nested" ? 1 : 0));
    const walk = (list, ctx) => {
      for (const item of list) {
        if (item.kind === "macro" && item.macro === "include" && item.include?.endsWith(".rs")) {
          load(path.posix.join(path.posix.dirname(rel), item.include), ctx);
        } else if (item.kind === "impl" && !item.isTrait) {
          walk(item.children, "impl");
        } else if (item.kind === "mod" && item.body) {
          if (ctx === "mod" && item.name === stem) hazards.push(`${rel}: inline mod ${item.name} has the unit's own name`);
          item.inner = parseItems(file.T, item.body[0] + 1, item.body[1], rel);
          walk(item.inner, "nested");
        } else if (item.kind === "mod" && item.outOfLine) {
          const attr = item.attrs.map(([a, b]) => file.T.slice(a, b + 1)).find((ts) => ts[2]?.t === "path");
          if (!attr) hazards.push(`${rel}: mod ${item.name}; has no #[path] and would resolve elsewhere`);
          else if (ctx !== "mod") hazards.push(`${rel}: mod ${item.name}; sits inside an inline module, so its #[path] would resolve elsewhere`);
          else childModules.push(path.posix.join(path.posix.dirname(rel), JSON.parse(attr[4].t)));
        }
      }
    };
    walk(items, context);
  };
  load(unitFile, "mod");

  // Child module files are descendants of the new module: they keep reading
  // its private items, and their own restrictions move with them.
  const inside = new Set(unitFiles);
  const addChild = (rel) => {
    if (inside.has(rel)) return;
    const file = tree.files.get(rel);
    if (!file) return;
    inside.add(rel);
    hazards.push(...movedRestrictions(file, 1));
    const own = rel.replace(/\.rs$/, "") + "/";
    for (const other of tree.files.keys()) if (other.startsWith(own)) inside.add(other);
    for (const item of parseItems(file.T, 0, file.T.length, rel)) {
      if (item.kind === "macro" && item.macro === "include" && item.include?.endsWith(".rs")) {
        addChild(path.posix.join(path.posix.dirname(rel), item.include));
      }
    }
  };
  for (const rel of childModules) addChild(rel);

  const external = (name) => expose.has(name) || tree.cratePath.has(name) || tree.usedOutside(name, inside);

  const defs = []; // { item, file, context, kind, name, owner }
  const traitImpls = [];
  for (const { file, items, context } of parsed.values()) {
    const visit = (list, ctx, owner) => {
      for (const item of list) {
        if (item.kind === "impl") {
          if (!item.isTrait) visit(item.children, "impl", item);
          else traitImpls.push({ item, file });
        } else if (item.kind === "foreign") {
          visit(item.children, "foreign", item);
        } else if (item.kind === "macro") {
          if (item.macro === "thread_local") visit(item.children, ctx, item);
          else if (item.macro === "macro_rules") {
            if (external(item.name)) hazards.push(`${file.rel}: macro_rules! ${item.name} is used outside the unit and needs #[macro_use]`);
          } else if (item.macro !== "include") {
            hazards.push(`${file.rel}: ${item.macro}! at offset ${file.T[item.first].s} may define items this tool cannot see`);
          }
        } else if (item.kind !== "extern_crate") {
          defs.push({ item, file, context: ctx, kind: item.kind, name: item.name, owner });
        }
      }
    };
    visit(items, context, null);
  }

  const localTypes = new Map();
  for (const def of defs) if (def.context === "mod" && TYPE_KINDS.has(def.kind)) localTypes.set(def.name, def);

  const promote = new Set();
  const visible = (def) => Boolean(def.item.vis) || promote.has(def);
  // A member of an inherent impl can only be reached from outside when its
  // type can: a private local type keeps all its members private, whatever
  // they are called.
  const ownerType = (def) => (def.owner?.kind === "impl" ? localTypes.get(def.owner.selfName) : undefined);
  const members = [];
  for (const def of defs) {
    if (def.item.vis || def.context === "nested" || def.name === "_") continue;
    if (def.kind === "use" || def.kind === "trait") {
      if (def.context === "mod") promote.add(def);
    } else if (external(def.name)) {
      if (ownerType(def)) members.push(def);
      else promote.add(def);
    }
  }

  // Fields of visible structs, then the types that visible signatures name,
  // until nothing changes.
  const fieldChoices = new Map();
  const identifiers = (file, a, b) => file.T.slice(a, b).filter((t) => t.k === "ident").map((t) => plain(t.t));
  const updatedOutside = (name) => {
    const exact = tree.structUpdates.get(name) ?? new Set();
    const unknown = tree.structUpdates.get("*") ?? new Set();
    for (const rel of exact) if (!inside.has(rel)) return true;
    for (const rel of unknown) if (!inside.has(rel) && tree.uses.get(name)?.has(rel)) return true;
    return false;
  };
  // A type that a trait impl gives as an associated type is part of that
  // impl's interface: the compiler refuses a private one when the impl is
  // for a visible type.
  for (const { item, file } of traitImpls) {
    for (const child of item.children) {
      if (child.kind !== "type" || !child.sig) continue;
      for (const name of identifiers(file, child.sig[0], child.sig[1])) {
        const type = localTypes.get(name);
        if (type && !type.item.vis) promote.add(type);
      }
    }
  }
  let changed = true;
  while (changed) {
    changed = false;
    for (const def of members) {
      if (!promote.has(def) && visible(ownerType(def))) {
        promote.add(def);
        changed = true;
      }
    }
    for (const def of defs) {
      if (def.context === "nested" || !visible(def)) continue;
      const T = def.file.T;
      const names = [];
      if (def.kind === "struct") {
        const range = def.item.body ?? def.item.tuple;
        if (range) {
          const fields = parseFields(T, range[0], range[1], Boolean(def.item.tuple));
          const all = Boolean(def.item.tuple) || updatedOutside(def.name);
          const chosen = fields.filter((field) => !field.vis && (all || external(field.name)));
          if (fieldChoices.get(def)?.length !== chosen.length) {
            fieldChoices.set(def, chosen);
            changed = true;
          }
          for (const field of fields) {
            if (field.vis || chosen.includes(field)) names.push(...identifiers(def.file, field.typeStart, field.typeEnd));
          }
        }
        names.push(...identifiers(def.file, def.item.header[0], def.item.header[1]));
      } else if (def.kind === "enum" || def.kind === "trait") {
        names.push(...identifiers(def.file, def.item.start, def.item.last + 1));
      } else if (def.item.sig) {
        names.push(...identifiers(def.file, def.item.sig[0], def.item.sig[1]));
      }
      if (def.owner?.kind === "impl") names.push(...identifiers(def.file, def.owner.header[0], def.owner.header[1]));
      for (const name of names) {
        const type = localTypes.get(name);
        if (type && !visible(type)) {
          promote.add(type);
          changed = true;
        }
      }
    }
  }

  const edits = new Map(); // rel -> offsets where `pub(crate) ` is inserted
  const insertAt = (file, offset) => {
    if (!edits.has(file.rel)) edits.set(file.rel, []);
    edits.get(file.rel).push(offset);
  };
  for (const def of promote) insertAt(def.file, def.file.T[def.item.start].s);
  let fields = 0;
  for (const [def, chosen] of fieldChoices) {
    if (!visible(def)) continue;
    fields += chosen.length;
    for (const field of chosen) insertAt(def.file, def.file.T[field.start].s);
  }
  const counted = defs.filter((def) => def.context !== "nested");
  const alreadyVisible = counted.filter((def) => def.item.vis).length;
  let platform = 0;
  for (const rel of inside) platform += platformConditionals(tree.files.get(rel));
  // Whether the module has a name for the crate root to import. A unit of
  // nothing but impl blocks, or with every item private, has none, and a
  // glob import of it would be an unused import.
  const nameable = defs.filter((def) => (def.context === "mod" || def.context === "foreign") && visible(def));
  const reexport = nameable.length > 0;
  // Whether only test code needs the re-export: wherever another file names
  // one of the module's items, it does so in code compiled only for tests.
  // In another build nothing would read the glob import, an unused import.
  // An item that can be depended on without being named (a `use`, a trait),
  // one that was visible already, and one exposed by hand keep the re-export
  // in every build.
  const namedOutside = (index, def) => [...(index.get(def.name) ?? [])].some((rel) => !inside.has(rel));
  const reexportForTests =
    nameable.some((def) => namedOutside(tree.uses, def)) &&
    nameable.every(
      (def) =>
        def.kind !== "use" &&
        def.kind !== "trait" &&
        !def.item.vis &&
        !expose.has(def.name) &&
        !tree.plainCratePath.has(def.name) &&
        !namedOutside(tree.plainUses, def),
    );
  // Whether the unit names anything the crate root provides. A unit that
  // names nothing of the kind, a schema constant for example, would get an
  // unused `use super::*;`. A name after `.` is a field or a method and a
  // name after `::` belongs to its path, except after `super::` or `self::`,
  // which reach the module's own imports. Child module files count: they
  // read the root through this module. When every such name sits in code
  // compiled only for tests, the import is needed only there.
  const rootNames = tree.rootNames();
  // a name the unit defines or imports itself is its own, whatever the root has
  const ownNames = new Set();
  for (const def of defs) {
    if (def.context !== "mod" && def.context !== "foreign") continue;
    if (def.kind === "use") for (const leaf of useLeaves(def.file.T, def.item)) ownNames.add(leaf);
    else if (def.name) ownNames.add(def.name);
  }
  const fromRoot = (name) => !ownNames.has(name) && [...(rootNames.get(name) ?? [])].some((rel) => !inside.has(rel));
  let importsRoot = false;
  let importsRootForTests = true;
  for (const rel of inside) {
    const T = tree.files.get(rel)?.T ?? [];
    for (let i = 0; i < T.length && importsRootForTests; i++) {
      if (T[i].k !== "ident") continue;
      const prev = T[i - 1]?.t;
      if (prev === ".") continue;
      if (prev === "::" && T[i - 2]?.t !== "super" && T[i - 2]?.t !== "self") continue;
      if (!fromRoot(plain(T[i].t))) continue;
      importsRoot = true;
      if (!tree.inTestCode(rel, i)) importsRootForTests = false;
    }
  }
  importsRootForTests &&= importsRoot;
  return {
    unitFile,
    unitFiles,
    edits,
    hazards,
    reexport,
    reexportForTests,
    importsRoot,
    importsRootForTests,
    stats: {
      files: unitFiles.size,
      definitions: counted.length,
      alreadyVisible,
      madeVisible: promote.size,
      private: counted.length - alreadyVisible - promote.size,
      fieldsMadeVisible: fields,
      platformConditionals: platform,
    },
  };
}

const VISIBILITY = "pub(crate) ";

function insertVisibility(text, offsets) {
  let out = text;
  for (const offset of [...offsets].sort((a, b) => b - a)) out = out.slice(0, offset) + VISIBILITY + out.slice(offset);
  return out;
}

const plainComment = (token) => token.k === "comment" && !/^\/\/[/!]|^\/\*[*!]/.test(token.t);

// Where `use super::*;` goes: after the file's leading plain-comment header,
// if it has one, otherwise at the top.
function headerEnd(file) {
  if (!file.all[0] || !plainComment(file.all[0])) return 0;
  let i = 0;
  while (file.all[i + 1] && plainComment(file.all[i + 1]) && !/\n\s*\n/.test(file.text.slice(file.all[i].e, file.all[i + 1].s))) i++;
  const newline = file.text.indexOf("\n", file.all[i].e);
  return newline < 0 ? file.text.length : newline + 1;
}

// Plans every requested unit and, only when none has a hazard, returns the
// new text of each changed file. A refusal changes nothing.
export function convertUnits(root, unitFiles, { expose = [] } = {}) {
  const tree = new SourceTree(root);
  const units = tree.mainUnits();
  const exposed = new Set(expose.map(plain));
  const plans = [];
  const refused = [];
  for (const unitFile of unitFiles) {
    const unit = units.find((u) => u.file === unitFile);
    if (!unit) { refused.push({ unit: unitFile, hazards: [`${unitFile} is not included at the top level of src/main.rs`] }); continue; }
    const hazards = [];
    if (unit.otherAttrs) hazards.push(`${unitFile}: its include!() carries an attribute other than #[cfg(test)]`);
    let plan;
    try {
      plan = planUnit(tree, unitFile, exposed);
      hazards.push(...plan.hazards);
    } catch (error) {
      hazards.push(error.message);
    }
    if (hazards.length) refused.push({ unit: unitFile, hazards });
    else plans.push({ unit, plan });
  }
  const seen = new Map();
  for (const { plan } of plans) {
    for (const rel of plan.unitFiles) {
      if (seen.has(rel)) refused.push({ unit: plan.unitFile, hazards: [`${rel} also belongs to ${seen.get(rel)}`] });
      seen.set(rel, plan.unitFile);
    }
  }
  if (refused.length) return { refused, changes: new Map(), converted: [] };

  const changes = new Map(); // absolute path -> new text
  const main = tree.files.get("main.rs");
  const mainEol = main.text.includes("\r\n") ? "\r\n" : "\n";
  const replacements = []; // [from, to, text] in src/main.rs
  const converted = [];
  for (const { unit, plan } of plans) {
    const stem = unit.file.replace(/\.rs$/, "");
    for (const rel of plan.unitFiles) {
      const file = tree.files.get(rel);
      const offsets = plan.edits.get(rel) ?? [];
      let text = insertVisibility(file.text, offsets);
      if (rel === unit.file && plan.importsRoot) {
        const eol = file.text.includes("\r\n") ? "\r\n" : "\n";
        const at = headerEnd(file);
        const shifted = at + offsets.filter((offset) => offset < at).length * VISIBILITY.length;
        // a file that is itself included only for tests needs no second cfg
        const glob = `${plan.importsRootForTests && !unit.cfgTest ? `#[cfg(test)]${eol}` : ""}use super::*;`;
        const line = at === 0 ? `${glob}${eol}${eol}` : `${eol}${glob}${eol}`;
        text = text.slice(0, shifted) + line + text.slice(shifted);
      }
      if (text !== file.text) changes.set(file.full, text);
    }
    const cfg = unit.cfgTest || plan.reexportForTests ? `#[cfg(test)]${mainEol}` : "";
    const glob = plan.reexport ? `${mainEol}${cfg}use ${stem}::*;` : "";
    // rustfmt sorts adjacent `mod` lines and adjacent `use` lines. A blank
    // line keeps the original order where the new line would otherwise join
    // such a run: a `mod` without a re-export, whose next line may be another
    // `mod`, and a re-export that an import follows.
    const joinsRun = !plan.reexport || unit.beforeUse;
    const blankFollows = /^[ \t]*\r?\n[ \t]*\r?\n/.test(main.text.slice(unit.range[1]));
    replacements.push([...unit.range, `mod ${stem};${glob}${joinsRun && !blankFollows ? mainEol : ""}`]);
    // The same above it: after an existing `mod` line the new one would be
    // sorted into its run, so a blank line ends that line first.
    if (unit.afterMod !== null && !/^[^\n]*\r?\n[ \t]*\r?\n/.test(main.text.slice(unit.afterMod))) {
      const lineEnd = main.text.indexOf(mainEol, unit.afterMod);
      replacements.push([lineEnd, lineEnd, mainEol]);
    }
    converted.push({
      unit: unit.file,
      ...plan.stats,
      reexported: plan.reexport,
      reexportedForTests: plan.reexport && plan.reexportForTests && !unit.cfgTest,
      importsRoot: plan.importsRoot,
      importsRootForTests: plan.importsRootForTests && !unit.cfgTest,
    });
  }
  // The parsed range of each invocation is replaced, last one first, so text
  // that merely looks like an include line, in a string for example, stays.
  let mainText = main.text;
  for (const [from, to, text] of replacements.sort((a, b) => b[0] - a[0])) mainText = mainText.slice(0, from) + text + mainText.slice(to);
  changes.set(main.full, mainText);
  return { refused, changes, converted };
}

// ------------------------------------------------------------------ verify
// rustfmt until its output no longer changes. One pass is not always enough:
// a line it leaves exactly at the width limit can be wrapped by the next
// pass, and `cargo fmt --check` accepts only the text a further pass keeps.
function rustfmt(text) {
  let current = text.replace(/\r\n/g, "\n");
  for (let pass = 0; pass < 4; pass++) {
    const next = execFileSync("rustfmt", ["--edition", "2024", "--emit", "stdout"], {
      input: current,
      encoding: "utf8",
      maxBuffer: 1 << 28,
    }).replace(/\r\n/g, "\n");
    if (next === current) break;
    current = next;
  }
  return current;
}

// Two checks on every file that differs between the two trees.
//   1. Reproduction: the candidate file equals format(what `convertUnits`
//      makes of the base for the units the candidate converted). This pins
//      each insertion to the place the plan puts it and requires every
//      re-export the plan calls for. A unit the tool refuses, or one that
//      was converted by hand, cannot pass it.
//   2. Content: format(candidate with inserted visibility and wiring
//      removed) == format(base). This does not depend on the plan; it shows
//      that the conversion added nothing but visibility and wiring.
// `format` is rustfmt unless a caller supplies its own; `expose` is the list
// the conversion was run with.
export function verifyTrees(baseRoot, newRoot, { format = rustfmt, expose = [] } = {}) {
  const filesOf = (root) => new Map(listRustFiles(path.join(root, "src")).map((full) => [relativeName(path.join(root, "src"), full), full]));
  const base = filesOf(baseRoot);
  const next = filesOf(newRoot);
  const problems = [];
  const changedFiles = [];
  let insertedVisibility = 0;
  // Git stores LF for both; a CRLF working copy is a checkout artefact.
  const read = (full) => fs.readFileSync(full, "utf8").replace(/\r\n/g, "\n");
  for (const name of base.keys()) if (!next.has(name)) problems.push(`${name}: removed`);

  // What the tool makes of the base, for the units the candidate converted.
  const stillIncluded = new Set(new SourceTree(newRoot).mainUnits().map((unit) => unit.file));
  const convertedUnits = new SourceTree(baseRoot).mainUnits().map((unit) => unit.file).filter((unit) => !stillIncluded.has(unit));
  const expected = convertUnits(baseRoot, convertedUnits, { expose });
  for (const { unit, hazards } of expected.refused) {
    problems.push(`${unit}: the tool refuses to convert this unit (${hazards[0]}), so verify cannot pass it`);
  }
  const reproduced = new Map();
  for (const [full, text] of expected.changes) {
    const name = relativeName(path.join(baseRoot, "src"), full);
    const converted = text.replace(/\r\n/g, "\n");
    if (converted !== read(full)) reproduced.set(name, converted);
  }
  for (const [name, full] of next) {
    if (!base.has(name)) { problems.push(`${name}: new file`); continue; }
    const before = read(base.get(name));
    let after = read(full);
    if (before === after) continue;
    changedFiles.push(name);
    if (!expected.refused.length) {
      if (!reproduced.has(name)) {
        // A file the conversion leaves alone may still change: rustfmt
        // reaches a child module once the file that mounts it is a module.
        if (format(before) !== after) problems.push(`${name}: changed by more than formatting, and converting the base leaves it as it is`);
        continue;
      }
      const made = format(reproduced.get(name));
      // rustfmt follows `mod` lines, not include!(): a fragment that a
      // converted unit still includes may be left exactly as converted.
      const stillFragment = name !== "main.rs" && !convertedUnits.includes(name);
      // The content check below reads formatted text on both sides.
      if (stillFragment && reproduced.get(name) === after) after = made;
      if (made !== after) {
        const want = made.split("\n");
        const got = after.split("\n");
        let line = 0;
        while (line < want.length && got[line] === want[line]) line++;
        problems.push(`${name}: differs from the tool's own conversion of the base at line ${line + 1}: ${JSON.stringify(want[line])} (conversion) and ${JSON.stringify(got[line])} (candidate)`);
        continue;
      }
    }
    const wanted = format(before);
    // The walk finds what was inserted. It skips the comma before a closing
    // delimiter or the `>` of a generic list, which rustfmt adds or drops
    // when it re-wraps a list; the comparison of formatted text below skips
    // nothing.
    const listEnd = (t) => isClose(t) || (t.k === "punct" && t.t === ">");
    const significant = (tokens) => tokens.filter((t, k) => !(t.t === "," && tokens[k + 1] && listEnd(tokens[k + 1])));
    const A = significant(lex(wanted));
    const B = significant(lex(after));
    const at = (X, k, ...texts) => texts.every((text, d) => X[k + d]?.t === text);
    const cuts = []; // [from, to, replacement] in `after`
    let useCut = null;
    let wiredThrough = 0; // where the last restored include's cut ends
    let i = 0;
    let j = 0;
    let walked = true;
    const superGlob = ["use", "super", "::", "*", ";"];
    const forTests = ["#", "[", "cfg", "(", "test", ")", "]"];
    // how many tokens of X at k are `use super::*;`, with its #[cfg(test)] if it has one
    const globAt = (X, k) => (at(X, k, ...superGlob) ? 5 : at(X, k, ...forTests, ...superGlob) ? 12 : 0);
    while (i < A.length || j < B.length) {
      // before the plain comparison: the base's first item may itself be a `use`
      const globTokens = name !== "main.rs" && !useCut ? globAt(B, j) : 0;
      if (globTokens && globAt(A, i) !== globTokens) {
        const newline = after.indexOf("\n", B[j + globTokens - 1].e);
        useCut = [B[j].s, newline < 0 ? after.length : newline + 1, ""];
        cuts.push(useCut);
        j += globTokens;
        continue;
      }
      if (A[i] && B[j] && A[i].t === B[j].t) { i++; j++; continue; }
      if (at(B, j, "pub", "(", "crate", ")")) {
        let to = B[j + 3].e;
        while (after[to] === " ") to++;
        cuts.push([B[j].s, to, ""]);
        insertedVisibility++;
        j += 4;
        continue;
      }
      if (name === "main.rs" && at(A, i, "include", "!", "(") && A[i + 3]?.k === "str" && at(B, j, "mod")) {
        const file = JSON.parse(A[i + 3].t);
        const stem = file.replace(/\.rs$/, "");
        const from = B[j].s;
        if (!at(B, j, "mod", stem, ";")) { problems.push(`main.rs: include of ${file} replaced by something else`); walked = false; break; }
        // a blank line the conversion put above its lines, after an existing
        // `mod` line, goes too
        const lead = at(A, i - 7, "#", "[", "cfg", "(", "test", ")", "]") ? 7 : 0;
        const aboveBase = A[i - lead - 1];
        const above = B[j - lead - 1];
        const blankBelow = (text, token) => /^[^\n]*\n[ \t]*\n/.test(text.slice(token.e));
        if (above && aboveBase && wiredThrough <= above.e && blankBelow(after, above) && !blankBelow(wanted, aboveBase)) {
          const lineEnd = after.indexOf("\n", above.e);
          cuts.push([lineEnd, lineEnd + 1, ""]);
        }
        i += 6;
        j += 3;
        // `use x::*;` follows unless the module has nothing to re-export
        const hadCfg = at(A, i - 13, "#", "[", "cfg", "(", "test", ")", "]");
        const cfgHere = at(B, j, "#", "[", "cfg", "(", "test", ")", "]");
        const useAt = cfgHere ? j + 7 : j;
        // a blank line the conversion put after its lines, to keep rustfmt
        // from sorting them into the run that follows, goes too
        const baseEnd = A[i - 1].e;
        const baseBlank = wanted[baseEnd] === "\n" && wanted[baseEnd + 1] === "\n";
        const through = (end) => (!baseBlank && after[end] === "\n" && after[end + 1] === "\n" ? end + 1 : end);
        if (!at(B, useAt, "use", stem, "::", "*", ";")) {
          wiredThrough = through(B[j - 1].e);
          cuts.push([from, wiredThrough, `include!("${file}");`]);
          continue;
        }
        // a re-export may be narrowed to test builds, never widened from them
        if (hadCfg && !cfgHere) {
          problems.push(`main.rs: use ${stem}::* lost #[cfg(test)]`);
          walked = false;
          break;
        }
        wiredThrough = through(B[useAt + 4].e);
        cuts.push([from, wiredThrough, `include!("${file}");`]);
        j = useAt + 5;
        continue;
      }
      const left = A.slice(i, i + 5).map((t) => t.t).join(" ");
      const right = B.slice(j, j + 5).map((t) => t.t).join(" ");
      problems.push(`${name}: content differs near "${left}" (base) and "${right}" (candidate, offset ${B[j]?.s})`);
      walked = false;
      break;
    }
    if (!walked) continue;
    const without = (list) => {
      let out = after;
      for (const [from, to, text] of [...list].sort((x, y) => y[0] - x[0])) out = out.slice(0, from) + text + out.slice(to);
      return out;
    };
    // The inserted `use super::*;` brought a blank line with it on one side or
    // the other; which one depends on what the base had there.
    const variants = [cuts];
    if (useCut) {
      const widen = (from, to) => cuts.map((cut) => (cut === useCut ? [from, to, ""] : cut));
      const blankAfter = after[useCut[1]] === "\n";
      const previousLine = after.lastIndexOf("\n", useCut[0] - 2) + 1;
      const blankBefore = previousLine > 0 && after.slice(previousLine, useCut[0]).trim() === "";
      if (blankAfter) variants.push(widen(useCut[0], useCut[1] + 1));
      if (blankBefore) variants.push(widen(previousLine, useCut[1]));
      if (blankBefore && blankAfter) variants.push(widen(previousLine, useCut[1] + 1));
    }
    if (variants.some((list) => format(without(list)) === wanted)) continue;
    const got = format(without(cuts)).split("\n");
    const want = wanted.split("\n");
    let line = 0;
    while (line < want.length && got[line] === want[line]) line++;
    problems.push(`${name}: with the insertions removed it formats differently from the base at line ${line + 1}: ${JSON.stringify(want[line])} (base) and ${JSON.stringify(got[line])} (candidate)`);
  }
  for (const name of reproduced.keys()) {
    if (!changedFiles.includes(name)) problems.push(`${name}: converting the base changes it, but the candidate left it as it was`);
  }
  return { changedFiles: changedFiles.length, convertedUnits: convertedUnits.length, insertedVisibility, problems };
}

// --------------------------------------------------------------------- cli
function analyze(root) {
  const tree = new SourceTree(root);
  const totals = { units: 0, files: 0, definitions: 0, alreadyVisible: 0, madeVisible: 0, private: 0, fieldsMadeVisible: 0, platformConditionals: 0 };
  const units = [];
  for (const unit of tree.mainUnits()) {
    totals.units++;
    const row = { unit: unit.file, cfgTest: unit.cfgTest, hazards: [] };
    try {
      const plan = planUnit(tree, unit.file);
      Object.assign(row, plan.stats);
      row.hazards = plan.hazards;
      for (const key of Object.keys(plan.stats)) totals[key] += plan.stats[key];
    } catch (error) {
      row.hazards = [error.message];
    }
    units.push(row);
  }
  return { totals, units };
}

function main(argv) {
  const [command, ...rest] = argv;
  const print = (value) => process.stdout.write(`${JSON.stringify(value, null, 1)}\n`);
  const exposed = () => {
    const at = rest.indexOf("--expose");
    return at >= 0 ? (rest[at + 1] ?? "").split(",").filter(Boolean) : [];
  };
  if (command === "analyze" && rest.length === 1) {
    print(analyze(rest[0]));
    return 0;
  }
  if (command === "convert" && rest.length >= 2) {
    const result = convertUnits(rest[0], rest[1].split(",").filter(Boolean), { expose: exposed() });
    if (result.refused.length) {
      print({ refused: result.refused, written: 0 });
      return 1;
    }
    const write = rest.includes("--write");
    if (write) for (const [full, text] of result.changes) fs.writeFileSync(full, text);
    // units whose batch also needs a compile on the other platform
    const platformUnits = result.converted.filter((unit) => unit.platformConditionals > 0).map((unit) => unit.unit);
    print({ converted: result.converted, platformUnits, written: write ? result.changes.size : 0 });
    return 0;
  }
  if (command === "verify" && rest.length >= 2) {
    const result = verifyTrees(rest[0], rest[1], { expose: exposed() });
    print(result);
    return result.problems.length ? 1 : 0;
  }
  process.stderr.write("usage: include-to-mod.mjs analyze ROOT | convert ROOT UNIT[,UNIT...] [--write] [--expose NAME[,NAME...]] | verify BASE_ROOT NEW_ROOT [--expose NAME[,NAME...]]\n");
  return 2;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
