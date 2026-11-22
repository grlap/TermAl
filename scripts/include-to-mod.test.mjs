// Exercises the include!() to module conversion on small Rust trees it writes
// itself; it never reads the repository's own sources. The verify cases need
// rustfmt on PATH, as the gate's format stage does.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { convertUnits, verifyTrees } from "./include-to-mod.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const scriptFile = fileURLToPath(new URL("./include-to-mod.mjs", import.meta.url));

const MAIN = `use std::collections::HashMap;

fn main() {
    run_alpha();
    let _ = HashMap::<u8, u8>::new();
}

include!("alpha.rs");
include!("beta.rs");
#[cfg(test)]
include!("probe.rs");

#[cfg(test)]
mod tests;
`;

const ALPHA = `// Alpha owns the counter.
// It does not own what reads it.

use std::fmt::Write;

const LIMIT: usize = 3;

struct Counter {
    total: usize,
    hidden: usize,
}

struct Wrapped(usize);

impl Counter {
    fn bump(&mut self) {
        self.total += helper(self.hidden);
    }

    fn snapshot(&self) -> Wrapped {
        Wrapped(self.total)
    }
}

fn helper(value: usize) -> usize {
    value + LIMIT
}

fn run_alpha() {
    let mut counter = Counter {
        total: 0,
        hidden: 1,
    };
    counter.bump();
    let _seen: HashMap<u8, u8> = HashMap::new();
    let _shape = stringify!(fn);
}
`;

const BETA = `fn read_total(counter: &Counter) -> usize {
    let _ = counter.snapshot();
    counter.total
}
`;

const PROBE = `fn probe_value() -> usize {
    7
}
`;

const TESTS = `use super::*;

#[test]
fn reads() {
    assert_eq!(probe_value(), 7);
}
`;

function fixture(t, overrides = {}) {
  const root = mkdtempSync(join(testTempDirectory(), "include-to-mod-fixture-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const files = {
    "src/main.rs": MAIN,
    "src/alpha.rs": ALPHA,
    "src/beta.rs": BETA,
    "src/probe.rs": PROBE,
    "src/tests.rs": TESTS,
    ...overrides,
  };
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
  return root;
}

function textOf(result, root, path) {
  return result.changes.get(join(root, ...path.split("/")));
}

function snapshot(root, paths) {
  return Object.fromEntries(paths.map((path) => [path, readFileSync(join(root, path), "utf8")]));
}

// A copy of `root` with the conversion written into it.
function converted(t, root, units) {
  const copy = mkdtempSync(join(testTempDirectory(), "include-to-mod-converted-"));
  t.after(() => rmSync(copy, { recursive: true, force: true }));
  cpSync(root, copy, { recursive: true });
  const result = convertUnits(copy, units);
  assert.deepEqual(result.refused, []);
  for (const [full, text] of result.changes) writeFileSync(full, text);
  return copy;
}

test("a unit becomes a module: names used from other files turn crate-visible and the rest stay private", (t) => {
  const root = fixture(t);
  const result = convertUnits(root, ["alpha.rs"]);
  assert.deepEqual(result.refused, []);

  const main = textOf(result, root, "src/main.rs");
  assert.match(main, /^mod alpha;\nuse alpha::\*;\ninclude!\("beta\.rs"\);$/m);

  const alpha = textOf(result, root, "src/alpha.rs");
  assert.ok(alpha.startsWith("// Alpha owns the counter.\n// It does not own what reads it.\n\nuse super::*;\n"));
  // an import is shared through the crate root today, whoever names it
  assert.match(alpha, /^pub\(crate\) use std::fmt::Write;$/m);
  // used by beta.rs or main.rs
  assert.match(alpha, /^pub\(crate\) struct Counter \{$/m);
  assert.match(alpha, /^    pub\(crate\) total: usize,$/m);
  assert.match(alpha, /^    pub\(crate\) fn snapshot\(&self\) -> Wrapped \{$/m);
  assert.match(alpha, /^pub\(crate\) fn run_alpha\(\) \{$/m);
  // named only in the signature of a method that became visible
  assert.match(alpha, /^pub\(crate\) struct Wrapped\(pub\(crate\) usize\);$/m);
  // used only inside alpha.rs
  assert.match(alpha, /^const LIMIT: usize = 3;$/m);
  assert.match(alpha, /^    hidden: usize,$/m);
  assert.match(alpha, /^    fn bump\(&mut self\) \{$/m);
  assert.match(alpha, /^fn helper\(value: usize\) -> usize \{$/m);

  assert.equal(textOf(result, root, "src/beta.rs"), undefined);
});

test("a test-only include keeps its cfg on both the module and its re-export", (t) => {
  const root = fixture(t);
  const result = convertUnits(root, ["probe.rs"]);
  assert.deepEqual(result.refused, []);
  assert.match(textOf(result, root, "src/main.rs"), /^#\[cfg\(test\)\]\nmod probe;\n#\[cfg\(test\)\]\nuse probe::\*;$/m);
  assert.match(textOf(result, root, "src/probe.rs"), /^pub\(crate\) fn probe_value\(\) -> usize \{$/m);
});

test("a module whose items only test files name is re-exported for test builds only", (t) => {
  // src/tests.rs is mounted under #[cfg(test)]; in another build nothing would read the glob
  const files = {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("zeta.rs");'),
    "src/zeta.rs": "fn zeta_value() -> usize {\n    1\n}\n",
    "src/tests.rs": `${TESTS}\n#[test]\nfn reads_zeta() {\n    assert_eq!(zeta_value(), 1);\n}\n`,
  };
  const base = fixture(t, files);
  const result = convertUnits(base, ["zeta.rs"]);
  assert.equal(result.converted[0].reexportedForTests, true);
  assert.match(textOf(result, base, "src/main.rs"), /^mod zeta;\n#\[cfg\(test\)\]\nuse zeta::\*;\n#\[cfg\(test\)\]\ninclude!\("probe\.rs"\);$/m);
  assert.deepEqual(verifyTrees(base, converted(t, base, ["zeta.rs"])).problems, []);

  // one caller outside the tests, and every build needs it
  const shared = fixture(t, { ...files, "src/beta.rs": `${BETA}\nfn calls() -> usize {\n    zeta_value()\n}\n` });
  const everywhere = convertUnits(shared, ["zeta.rs"]);
  assert.equal(everywhere.converted[0].reexportedForTests, false);
  assert.match(textOf(everywhere, shared, "src/main.rs"), /^mod zeta;\nuse zeta::\*;\n#\[cfg\(test\)\]\ninclude!\("probe\.rs"\);$/m);
});

test("a #[cfg(test)] item in an ordinary file counts as test code, for the re-export and for use super::*", (t) => {
  // beta.rs calls zeta_value only from a test-only function, and zeta.rs
  // names the root's HashMap only in a test-only function of its own
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("zeta.rs");'),
    "src/zeta.rs": "fn zeta_value() -> usize {\n    1\n}\n\n#[cfg(test)]\nfn zeta_seen() -> HashMap<u8, u8> {\n    HashMap::new()\n}\n",
    "src/beta.rs": `${BETA}\n#[cfg(test)]\nfn checks() -> usize {\n    zeta_value()\n}\n`,
  });
  const result = convertUnits(base, ["zeta.rs"]);
  assert.equal(result.converted[0].reexportedForTests, true);
  assert.equal(result.converted[0].importsRootForTests, true);
  assert.ok(textOf(result, base, "src/zeta.rs").startsWith("#[cfg(test)]\nuse super::*;\n\npub(crate) fn zeta_value() -> usize {\n"));
  assert.match(textOf(result, base, "src/main.rs"), /^mod zeta;\n#\[cfg\(test\)\]\nuse zeta::\*;$/m);
  assert.deepEqual(verifyTrees(base, converted(t, base, ["zeta.rs"])).problems, []);
});

test("a test-only field and a #[cfg(all(test, ...))] item are test code, and what follows them is not", (t) => {
  // beta.rs names ZetaGate in a test-only field and in a test-and-unix item,
  // and EtaPlain in the ordinary field right after the test-only one
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("zeta.rs");\ninclude!("eta.rs");'),
    "src/zeta.rs": "struct ZetaGate;\n",
    "src/eta.rs": "struct EtaPlain;\n",
    "src/beta.rs": `${BETA}\nstruct Holder {\n    count: usize,\n    #[cfg(test)]\n    gate: Option<ZetaGate>,\n    after: EtaPlain,\n}\n\n#[cfg(all(test, unix))]\nstruct Other(ZetaGate);\n`,
  });
  const result = convertUnits(base, ["zeta.rs", "eta.rs"]);
  assert.deepEqual(result.converted.map((unit) => unit.reexportedForTests), [true, false]);
});

test("a type a trait impl gives as an associated type becomes visible with it", (t) => {
  // a private Inner behind a visible Outer is an error: private type in public interface
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("shape.rs");'),
    "src/shape.rs": "struct Inner;\n\nstruct Outer;\n\ntrait Shape {\n    type Part;\n}\n\nimpl Shape for Outer {\n    type Part = Inner;\n}\n",
    "src/beta.rs": `${BETA}\nfn outer() -> Outer {\n    Outer\n}\n`,
  });
  const shape = textOf(convertUnits(base, ["shape.rs"]), base, "src/shape.rs");
  assert.match(shape, /^pub\(crate\) struct Outer;$/m);
  assert.match(shape, /^pub\(crate\) struct Inner;$/m);
});

test("a unit that names nothing from the crate root gets no use super::*", (t) => {
  // probe.rs is one function over a literal; alpha.rs uses HashMap, which src/main.rs imports
  const root = fixture(t);
  const result = convertUnits(root, ["alpha.rs", "probe.rs"]);
  assert.deepEqual(result.converted.map((unit) => unit.importsRoot), [true, false]);
  assert.equal(textOf(result, root, "src/probe.rs"), "pub(crate) fn probe_value() -> usize {\n    7\n}\n");
  assert.match(textOf(result, root, "src/alpha.rs"), /^use super::\*;$/m);
});

test("a unit whose child module reads the root keeps use super::*", (t) => {
  // probe.rs itself names nothing from the root, but its test module does, through it
  const root = fixture(t, {
    "src/probe.rs": `${PROBE}\n#[cfg(test)]\n#[path = "probe_checks.rs"]\nmod probe_checks;\n`,
    "src/probe_checks.rs": "use super::*;\n\n#[test]\nfn sees_the_root() {\n    let _ = read_total;\n}\n",
  });
  const result = convertUnits(root, ["probe.rs"]);
  assert.deepEqual(result.refused, []);
  assert.equal(result.converted[0].importsRoot, true);
  assert.ok(textOf(result, root, "src/probe.rs").startsWith("use super::*;\n"));
});

test("a unit of nothing but impl blocks becomes a module without a re-export", (t) => {
  const root = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("gamma.rs");'),
    "src/gamma.rs": "impl Counter {\n    fn doubled(&self) -> usize {\n        self.total * 2\n    }\n}\n",
    "src/beta.rs": `${BETA}\nfn twice(counter: &Counter) -> usize {\n    counter.doubled()\n}\n`,
  });
  const result = convertUnits(root, ["gamma.rs"]);
  assert.deepEqual(result.refused, []);
  assert.equal(result.converted[0].reexported, false);
  // a glob import of a module with no nameable item would be an unused import
  // and a blank line follows, so that rustfmt does not sort it with a mod line below
  assert.match(textOf(result, root, "src/main.rs"), /^include!\("beta\.rs"\);\nmod gamma;\n\n#\[cfg\(test\)\]\ninclude!\("probe\.rs"\);$/m);
  // the method is still reachable: the type lives elsewhere and beta.rs calls it
  assert.match(textOf(result, root, "src/gamma.rs"), /^    pub\(crate\) fn doubled\(&self\) -> usize \{$/m);
});

test("the members of a private local type stay private, whatever they are called", (t) => {
  const root = fixture(t, {
    "src/alpha.rs": `${ALPHA}\nstruct Scratch;\n\nimpl Scratch {\n    fn snapshot(&self) {}\n}\n`,
  });
  const alpha = textOf(convertUnits(root, ["alpha.rs"]), root, "src/alpha.rs");
  // beta.rs calls Counter::snapshot, never Scratch::snapshot
  assert.match(alpha, /^struct Scratch;$/m);
  assert.match(alpha, /^    fn snapshot\(&self\) \{\}$/m);
  assert.match(alpha, /^    pub\(crate\) fn snapshot\(&self\) -> Wrapped \{$/m);
});

test("verify accepts a module that was given no re-export", (t) => {
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("gamma.rs");'),
    "src/gamma.rs": "impl Counter {\n    fn doubled(&self) -> usize {\n        self.total * 2\n    }\n}\n",
    "src/beta.rs": `${BETA}\nfn twice(counter: &Counter) -> usize {\n    counter.doubled()\n}\n`,
  });
  // probe.rs follows gamma.rs in main.rs, so two mod lines would be adjacent
  const next = converted(t, base, ["alpha.rs", "gamma.rs", "probe.rs"]);
  const main = readFileSync(join(next, "src/main.rs"), "utf8");
  assert.ok(main.indexOf("mod gamma;") < main.indexOf("mod probe;"));
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

test("convert reports which units hold platform-conditional code", (t) => {
  const root = fixture(t, {
    "src/beta.rs": `${BETA}\n#[cfg(windows)]\nfn only_windows() {}\n\n#[cfg(not(unix))]\nfn not_unix() {}\n\nfn anywhere() -> bool {\n    cfg!(target_os = "macos")\n}\n`,
    "src/probe.rs": `${PROBE}\n#[cfg(feature = "extra")]\nfn extra() {}\n`,
  });
  const run = spawnSync(process.execPath, [scriptFile, "convert", root, "alpha.rs,beta.rs,probe.rs"], { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  const report = JSON.parse(run.stdout);
  assert.deepEqual(report.platformUnits, ["beta.rs"]);
  assert.deepEqual(report.converted.map((unit) => unit.platformConditionals), [0, 3, 0]);
  // without --write nothing is written
  assert.equal(report.written, 0);
  assert.equal(readFileSync(join(root, "src/beta.rs"), "utf8").includes("use super::*;"), false);
});

test("a raw identifier and its plain spelling are one name", (t) => {
  // the field is declared as r#type and read as r#type from another file
  const root = fixture(t, {
    "src/alpha.rs": ALPHA.replaceAll("total", "r#type"),
    "src/beta.rs": BETA.replaceAll("total", "r#type"),
  });
  const alpha = textOf(convertUnits(root, ["alpha.rs"]), root, "src/alpha.rs");
  assert.match(alpha, /^    pub\(crate\) r#type: usize,$/m);
  assert.match(alpha, /^    hidden: usize,$/m);
});

test("a name the token rule cannot see can be exposed by hand", (t) => {
  const root = fixture(t);
  const result = convertUnits(root, ["alpha.rs"], { expose: ["helper"] });
  assert.match(textOf(result, root, "src/alpha.rs"), /^pub\(crate\) fn helper\(value: usize\) -> usize \{$/m);
});

test("a name is exposed by either spelling when the source writes it as a raw identifier", (t) => {
  const root = fixture(t, { "src/alpha.rs": ALPHA.replaceAll("helper", "r#helper") });
  for (const spelling of ["helper", "r#helper"]) {
    const result = convertUnits(root, ["alpha.rs"], { expose: [spelling] });
    assert.match(textOf(result, root, "src/alpha.rs"), /^pub\(crate\) fn r#helper\(value: usize\) -> usize \{$/m, spelling);
  }
});

test("text in src/main.rs that only looks like an include line stays as it is", (t) => {
  const note = 'const NOTE: &str = r#"\ninclude!("alpha.rs");\n"#;\n';
  const root = fixture(t, { "src/main.rs": MAIN.replace('include!("alpha.rs");', `${note}include!("alpha.rs");`) });
  const result = convertUnits(root, ["alpha.rs"]);
  assert.deepEqual(result.refused, []);
  assert.ok(textOf(result, root, "src/main.rs").includes(`${note}mod alpha;\nuse alpha::*;\ninclude!("beta.rs");`));
});

test("an import that follows a converted include is not sorted together with the re-export", (t) => {
  // rustfmt orders adjacent imports, and `std` sorts before `zeta`
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("zeta.rs");\nuse std::fmt::Debug;'),
    "src/zeta.rs": "fn zeta_value() -> usize {\n    1\n}\n",
    "src/beta.rs": `${BETA}\nfn calls() -> usize {\n    zeta_value()\n}\n`,
  });
  const next = converted(t, base, ["zeta.rs"]);
  const formatted = spawnSync("rustfmt", ["--edition", "2024", join(next, "src/main.rs")], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
  assert.match(readFileSync(join(next, "src/main.rs"), "utf8"), /^mod zeta;\nuse zeta::\*;\n\nuse std::fmt::Debug;$/m);
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

test("a blank line that already follows an include is not doubled", (t) => {
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("probe.rs");', 'include!("probe.rs");\ninclude!("gamma.rs");'),
    "src/gamma.rs": "impl Counter {\n    fn doubled(&self) -> usize {\n        self.total * 2\n    }\n}\n",
    "src/beta.rs": `${BETA}\nfn twice(counter: &Counter) -> usize {\n    counter.doubled()\n}\n`,
  });
  const next = converted(t, base, ["gamma.rs"]);
  assert.match(readFileSync(join(next, "src/main.rs"), "utf8"), /^mod gamma;\n\n#\[cfg\(test\)\]\nmod tests;$/m);
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

const refusals = [
  {
    name: "a macro_rules! that another file uses",
    files: {
      "src/alpha.rs": `${ALPHA}\nmacro_rules! shout {\n    () => {\n        run_alpha()\n    };\n}\n`,
      "src/beta.rs": `${BETA}\nfn loud() {\n    shout!();\n}\n`,
    },
    hazard: /macro_rules! shout/,
  },
  {
    name: "pub(super) in an inline module, which meant the crate root",
    files: { "src/alpha.rs": `${ALPHA}\nmod inner {\n    pub(super) fn reach() {}\n}\n` },
    hazard: /pub\(super\)/,
  },
  {
    name: "pub(in crate::...) naming a path from the crate root",
    files: { "src/alpha.rs": `${ALPHA}\nmod inner {\n    pub(in crate::inner) fn reach() {}\n}\n` },
    hazard: /pub\(incrate::inner\)/,
  },
  {
    name: "a child module without #[path], which would resolve to another file",
    files: { "src/alpha.rs": `${ALPHA}\n#[cfg(test)]\nmod alpha_checks;\n`, "src/alpha_checks.rs": "use super::*;\n" },
    hazard: /mod alpha_checks; has no #\[path\]/,
  },
  {
    name: "pub(super) at the top of a child module file",
    files: {
      "src/alpha.rs": `${ALPHA}\n#[cfg(test)]\n#[path = "alpha_checks.rs"]\nmod alpha_checks;\n`,
      "src/alpha_checks.rs": "use super::*;\n\npub(super) fn shared_fixture() {}\n",
    },
    hazard: /alpha_checks\.rs: pub\(super\)/,
  },
  {
    name: "a child module declared inside an inline module, whose #[path] base would move",
    files: {
      "src/alpha.rs": `${ALPHA}\nmod outer {\n    #[path = "alpha_checks.rs"]\n    mod alpha_checks;\n}\n`,
      "src/outer/alpha_checks.rs": "pub(crate) fn inner() {}\n",
    },
    hazard: /mod alpha_checks; sits inside an inline module/,
  },
  {
    name: "an item macro whose expansion it cannot read",
    files: { "src/alpha.rs": `${ALPHA}\ndefine_ids! {\n    AlphaId\n}\n` },
    hazard: /define_ids!/,
  },
  {
    name: "an inline module with the unit's own name",
    files: { "src/alpha.rs": `${ALPHA}\nmod alpha {\n    pub(crate) fn inner() {}\n}\n` },
    hazard: /inline mod alpha has the unit's own name/,
  },
  {
    name: "a file src/main.rs does not include",
    files: {},
    units: ["tests.rs"],
    hazard: /not included at the top level/,
  },
];

for (const refusal of refusals) {
  test(`convert refuses ${refusal.name}, and changes nothing`, (t) => {
    const root = fixture(t, refusal.files);
    const result = convertUnits(root, refusal.units ?? ["alpha.rs"]);
    assert.equal(result.changes.size, 0);
    assert.deepEqual(result.converted, []);
    assert.equal(result.refused.length, 1);
    assert.match(result.refused[0].hazards.join("\n"), refusal.hazard);
  });
}

test("pub(super) that stays inside the file is not a hazard", (t) => {
  const root = fixture(t, {
    "src/alpha.rs": `${ALPHA}\nmod outer {\n    mod inner {\n        pub(super) fn reach() {}\n    }\n}\n`,
  });
  assert.deepEqual(convertUnits(root, ["alpha.rs"]).refused, []);
});

test("one hazardous unit refuses the whole request and --write leaves every file as it was", (t) => {
  const root = fixture(t, { "src/beta.rs": `${BETA}\nmod inner {\n    pub(super) fn reach() {}\n}\n` });
  const paths = ["src/main.rs", "src/alpha.rs", "src/beta.rs"];
  const before = snapshot(root, paths);
  const run = spawnSync(process.execPath, [scriptFile, "convert", root, "alpha.rs,beta.rs", "--write"], { encoding: "utf8" });
  assert.equal(run.status, 1);
  const report = JSON.parse(run.stdout);
  assert.equal(report.written, 0);
  assert.deepEqual(report.refused.map((entry) => entry.unit), ["beta.rs"]);
  assert.deepEqual(snapshot(root, paths), before);
});

test("--write converts the requested units on disk and verify accepts the result", (t) => {
  const base = fixture(t);
  const next = mkdtempSync(join(testTempDirectory(), "include-to-mod-written-"));
  t.after(() => rmSync(next, { recursive: true, force: true }));
  cpSync(base, next, { recursive: true });
  const run = spawnSync(process.execPath, [scriptFile, "convert", next, "alpha.rs,probe.rs", "--write"], { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  assert.equal(JSON.parse(run.stdout).written, 3);
  const verify = spawnSync(process.execPath, [scriptFile, "verify", base, next], { encoding: "utf8" });
  assert.equal(verify.status, 0, verify.stdout + verify.stderr);
  assert.deepEqual(JSON.parse(verify.stdout).problems, []);
});

test("verify accepts a generic list that rustfmt re-wraps once the visibility is added", (t) => {
  // a signature that fills its line exactly: eleven more characters push the
  // generic list onto lines of its own, where rustfmt gives it a trailing comma
  const name = "m".repeat(76);
  const signature = `    async fn ${name}<T: Clone>(`;
  assert.equal(signature.length, 100);
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("gamma.rs");'),
    "src/gamma.rs": `impl Counter {\n${signature}\n        &self,\n        value: T,\n    ) -> T {\n        value\n    }\n}\n`,
    "src/beta.rs": `${BETA}\nasync fn relay(counter: &Counter) -> usize {\n    counter.${name}(1usize).await\n}\n`,
  });
  const next = converted(t, base, ["gamma.rs"]);
  const formatted = spawnSync("rustfmt", ["--edition", "2024", join(next, "src/gamma.rs")], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
  assert.match(readFileSync(join(next, "src/gamma.rs"), "utf8"), /<\n        T: Clone,\n    >\(/);
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

test("verify accepts a conversion whose layout rustfmt changed", (t) => {
  // an unformatted base, as include!() files are today
  const base = fixture(t, { "src/beta.rs": "fn read_total(counter:&Counter)->usize{let _=counter.snapshot();counter.total}\n" });
  const next = converted(t, base, ["alpha.rs", "beta.rs"]);
  const formatted = spawnSync("rustfmt", ["--edition", "2024", join(next, "src/beta.rs")], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
  assert.match(readFileSync(join(next, "src/beta.rs"), "utf8"), /^fn read_total\(counter: &Counter\) -> usize \{\n    let _ = counter\.snapshot\(\);$/m);
  const result = verifyTrees(base, next);
  assert.deepEqual(result.problems, []);
  assert.equal(result.changedFiles, 3);
});

test("verify takes a fragment that a converted unit still includes as converted or formatted", (t) => {
  // rustfmt reaches alpha.rs through `mod alpha;` but not the file alpha.rs includes
  const base = fixture(t, {
    "src/alpha.rs": `${ALPHA}\ninclude!("alpha_parts.rs");\n`,
    // rustfmt drops the braces of this closure, so the two forms differ in tokens too
    "src/alpha_parts.rs": "fn part_value()->usize{let two=||{2};two()}\n",
    "src/beta.rs": `${BETA}\nfn calls() -> usize {\n    part_value()\n}\n`,
  });
  const next = converted(t, base, ["alpha.rs"]);
  const parts = join(next, "src/alpha_parts.rs");
  assert.equal(readFileSync(parts, "utf8"), "pub(crate) fn part_value()->usize{let two=||{2};two()}\n");
  assert.deepEqual(verifyTrees(base, next).problems, []);
  const formatted = spawnSync("rustfmt", ["--edition", "2024", parts], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
  assert.equal(readFileSync(parts, "utf8"), "pub(crate) fn part_value() -> usize {\n    let two = || 2;\n    two()\n}\n");
  assert.deepEqual(verifyTrees(base, next).problems, []);
  // but not with anything else in it
  writeFileSync(parts, "pub(crate) fn part_value() -> usize {\n    let two = || 3;\n    two()\n}\n");
  assert.equal(verifyTrees(base, next).problems.length, 1);
});

test("verify takes a child module that rustfmt reaches once its parent is a module, formatted and nothing else", (t) => {
  // probe_checks.rs is not converted, but `mod probe;` leads rustfmt to it
  const base = fixture(t, {
    "src/probe.rs": `${PROBE}\n#[cfg(test)]\n#[path = "probe_checks.rs"]\nmod probe_checks;\n`,
    "src/probe_checks.rs": "use super::*;\n#[test]\nfn sees_it(){assert_eq!(probe_value(),7);}\n",
  });
  const next = converted(t, base, ["probe.rs"]);
  const formatted = spawnSync("rustfmt", ["--edition", "2024", join(next, "src/main.rs")], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
  const checks = join(next, "src/probe_checks.rs");
  assert.equal(readFileSync(checks, "utf8"), "use super::*;\n#[test]\nfn sees_it() {\n    assert_eq!(probe_value(), 7);\n}\n");
  assert.deepEqual(verifyTrees(base, next).problems, []);
  writeFileSync(checks, "use super::*;\n#[test]\nfn sees_it() {\n    assert_eq!(probe_value(), 8);\n}\n");
  assert.deepEqual(verifyTrees(base, next).problems, ["probe_checks.rs: changed by more than formatting, and converting the base leaves it as it is"]);
});

test("verify formats to the text a further rustfmt pass keeps", (t) => {
  // one pass leaves the return line exactly at the width limit and the next pass wraps it
  const arm = '_ => return Err(ApiError::conflict("Kimi session is not currently running")),';
  const nested = ["{", "    {", "        {", "            {"].join("\n    ");
  const closing = ["            }", "        }", "    }", "}"].join("\n    ");
  const wrap = `fn pick(kind: usize) -> Result<usize, String> {\n    ${nested}\n                    let value = match kind {\n                        0 => 1,\n                        ${arm}\n                    };\n                    Ok(value)\n    ${closing}\n}\n`;
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("beta.rs");', 'include!("beta.rs");\ninclude!("wrap.rs");'),
    "src/wrap.rs": wrap,
    "src/beta.rs": `${BETA}\nfn calls() -> Result<usize, String> {\n    pick(1)\n}\n`,
  });
  const next = converted(t, base, ["wrap.rs"]);
  const file = join(next, "src/wrap.rs");
  const pass = () => assert.equal(spawnSync("rustfmt", ["--edition", "2024", file], { encoding: "utf8" }).status, 0);
  pass();
  const once = readFileSync(file, "utf8");
  pass();
  const twice = readFileSync(file, "utf8");
  assert.notEqual(twice, once);
  assert.match(twice, /^ +return Err\(ApiError::conflict\(\n +"Kimi session is not currently running",\n +\)\);$/m);
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

test("a blank line keeps a new mod line out of the run of an existing mod line above it", (t) => {
  // rustfmt sorts adjacent mod lines, and `alpha` sorts before `helper`
  const base = fixture(t, {
    "src/main.rs": MAIN.replace('include!("alpha.rs");', '// Helper owns nothing.\n#[cfg(test)]\nmod helper;\ninclude!("alpha.rs");'),
    "src/helper.rs": "pub(crate) fn help() {}\n",
  });
  const next = converted(t, base, ["alpha.rs"]);
  const formatted = spawnSync("rustfmt", ["--edition", "2024", join(next, "src/main.rs")], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
  assert.match(readFileSync(join(next, "src/main.rs"), "utf8"), /^\/\/ Helper owns nothing\.\n#\[cfg\(test\)\]\nmod helper;\n\nmod alpha;\nuse alpha::\*;$/m);
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

const tampering = [
  {
    name: "a changed literal",
    edit: (text) => text.replace("const LIMIT: usize = 3;", "const LIMIT: usize = 4;"),
    file: "src/alpha.rs",
  },
  {
    name: "a changed comment",
    edit: (text) => text.replace("// Alpha owns the counter.", "// Alpha owns nothing."),
    file: "src/alpha.rs",
  },
  {
    name: "a removed statement",
    edit: (text) => text.replace("    counter.bump();\n", ""),
    file: "src/alpha.rs",
  },
  {
    name: "a visibility wider than pub(crate)",
    edit: (text) => text.replace("pub(crate) fn run_alpha", "pub fn run_alpha"),
    file: "src/alpha.rs",
  },
  {
    name: "a re-export the plan calls for that is missing",
    edit: (text) => text.replace("mod alpha;\nuse alpha::*;", "mod alpha;\n"),
    file: "src/main.rs",
  },
  {
    name: "a pub(crate) where the plan puts none",
    edit: (text) => text.replace("fn helper(value: usize)", "pub(crate) fn helper(value: usize)"),
    file: "src/alpha.rs",
  },
  {
    name: "a pub(crate) the plan puts that is missing",
    edit: (text) => text.replace("pub(crate) fn run_alpha", "fn run_alpha"),
    file: "src/alpha.rs",
  },
  {
    // the file's one `use super::*;`, at another place
    name: "a use super::* moved into a function body",
    edit: (text) => text.replace("use super::*;\n\n", "").replace("    value + LIMIT\n", "    use super::*;\n    value + LIMIT\n"),
    file: "src/alpha.rs",
  },
  {
    // nothing but the tokens, inside a macro call the base already has
    name: "a pub(crate) slipped into macro input",
    edit: (text) => text.replace("stringify!(fn)", "stringify!(pub(crate) fn)"),
    file: "src/alpha.rs",
  },
  {
    name: "an include replaced by a different module",
    edit: (text) => text.replace("mod alpha;\nuse alpha::*;", "mod beta;\nuse beta::*;"),
    file: "src/main.rs",
  },
  {
    name: "a re-export narrowed to test builds that the plan keeps for every build",
    edit: (text) => text.replace("mod alpha;\nuse alpha::*;", "mod alpha;\n#[cfg(test)]\nuse alpha::*;"),
    file: "src/main.rs",
  },
  {
    name: "a re-export that lost its cfg(test)",
    edit: (text) => text.replace("#[cfg(test)]\nuse probe::*;", "use probe::*;"),
    file: "src/main.rs",
  },
];

for (const { name, edit, file } of tampering) {
  test(`verify rejects ${name}`, (t) => {
    const base = fixture(t);
    const next = converted(t, base, ["alpha.rs", "probe.rs"]);
    const path = join(next, file);
    const before = readFileSync(path, "utf8");
    const after = edit(before);
    assert.notEqual(after, before);
    writeFileSync(path, after);
    const result = verifyTrees(base, next);
    assert.equal(result.problems.length, 1, JSON.stringify(result.problems));
    assert.ok(result.problems[0].startsWith(file.replace("src/", "")));
  });
}

test("verify needs the names the conversion was told to expose", (t) => {
  const base = fixture(t);
  const next = mkdtempSync(join(testTempDirectory(), "include-to-mod-exposed-"));
  t.after(() => rmSync(next, { recursive: true, force: true }));
  cpSync(base, next, { recursive: true });
  const result = convertUnits(next, ["alpha.rs"], { expose: ["helper"] });
  for (const [full, text] of result.changes) writeFileSync(full, text);
  assert.equal(verifyTrees(base, next).problems.length, 1);
  assert.deepEqual(verifyTrees(base, next, { expose: ["helper"] }).problems, []);
});

test("verify cannot pass a unit the tool refuses to convert", (t) => {
  // converted by hand: the inline module's pub(super) is a hazard for the tool
  const base = fixture(t, { "src/beta.rs": `${BETA}\nmod inner {\n    pub(super) fn reach() {}\n}\n` });
  const next = mkdtempSync(join(testTempDirectory(), "include-to-mod-by-hand-"));
  t.after(() => rmSync(next, { recursive: true, force: true }));
  cpSync(base, next, { recursive: true });
  writeFileSync(join(next, "src/main.rs"), MAIN.replace('include!("beta.rs");', "mod beta;\nuse beta::*;"));
  writeFileSync(join(next, "src/beta.rs"), `use super::*;\n\n${BETA}\nmod inner {\n    pub(crate) fn reach() {}\n}\n`);
  const problems = verifyTrees(base, next).problems;
  assert.match(problems.join("\n"), /beta\.rs: the tool refuses to convert this unit/);
});

test("verify rejects a file that appears or disappears", (t) => {
  const base = fixture(t);
  const next = converted(t, base, ["alpha.rs"]);
  writeFileSync(join(next, "src/extra.rs"), "fn extra() {}\n");
  rmSync(join(next, "src/probe.rs"));
  assert.deepEqual(verifyTrees(base, next).problems.sort(), ["extra.rs: new file", "probe.rs: removed"]);
});

// ------------------------------------------------------------ nested units
// A unit whose parent is a module file, not src/main.rs: outer.rs is mounted
// by src/main.rs and includes its own fragments.
const NEST_MAIN = `use std::collections::HashMap;

fn main() {
    let _ = run_outer();
    let _ = HashMap::<u8, u8>::new();
}

mod outer;
use outer::*;
`;

const OUTER = `// Outer owns the tally's total.

use super::*;

include!("inner.rs");
#[cfg(test)]
include!("inner_probe.rs");

pub(crate) fn run_outer() -> usize {
    let tally = new_tally();
    tally.count + SHARED_LIMIT
}

#[cfg(test)]
fn probed() -> usize {
    probe_seed()
}
`;

const INNER = `// Inner owns the tally's parts.

pub(crate) const SHARED_LIMIT: usize = 3;

struct Tally {
    count: usize,
    spare: usize,
}

fn new_tally() -> Tally {
    Tally {
        count: seed(),
        spare: 0,
    }
}

fn seed() -> usize {
    let _seen: HashMap<u8, u8> = HashMap::new();
    1
}
`;

const INNER_PROBE = `fn probe_seed() -> usize {
    7
}
`;

function nestedFixture(t, overrides = {}) {
  const root = mkdtempSync(join(testTempDirectory(), "include-to-mod-nested-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const files = {
    "src/main.rs": NEST_MAIN,
    "src/outer.rs": OUTER,
    "src/inner.rs": INNER,
    "src/inner_probe.rs": INNER_PROBE,
    ...overrides,
  };
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
  return root;
}

// As `cargo fmt` would: rustfmt follows every `mod` line from src/main.rs.
function formatTree(root) {
  const formatted = spawnSync("rustfmt", ["--edition", "2024", join(root, "src/main.rs")], { encoding: "utf8" });
  assert.equal(formatted.status, 0, formatted.stderr);
}

test("a nested unit becomes a child of its parent: names the parent uses turn pub(super) and the rest stay as they were", (t) => {
  const root = nestedFixture(t);
  const result = convertUnits(root, ["inner.rs"]);
  assert.deepEqual(result.refused, []);
  assert.equal(result.converted[0].parent, "outer.rs");
  assert.equal(result.converted[0].reexportedBeyondParent, true);

  // a plain `mod inner;` in outer.rs would look for outer/inner.rs
  const outer = textOf(result, root, "src/outer.rs");
  assert.match(outer, /^use super::\*;\n\n#\[path = "inner\.rs"\]\nmod inner;\npub\(crate\) use inner::\*;\n#\[cfg\(test\)\]\ninclude!\("inner_probe\.rs"\);$/m);
  // src/main.rs mounts outer.rs as before
  assert.equal(textOf(result, root, "src/main.rs"), undefined);

  const inner = textOf(result, root, "src/inner.rs");
  // HashMap reaches the file through its parent, which has it from the crate root
  assert.ok(inner.startsWith("// Inner owns the tally's parts.\n\nuse super::*;\n"));
  // visible to the whole crate before, and still: the parent re-exports it
  assert.match(inner, /^pub\(crate\) const SHARED_LIMIT: usize = 3;$/m);
  // used by outer.rs, which saw them as its own private items
  assert.match(inner, /^pub\(super\) fn new_tally\(\) -> Tally \{$/m);
  assert.match(inner, /^    pub\(super\) count: usize,$/m);
  // named only in the signature of a function that became visible
  assert.match(inner, /^pub\(super\) struct Tally \{$/m);
  // used only inside inner.rs
  assert.match(inner, /^    spare: usize,$/m);
  assert.match(inner, /^fn seed\(\) -> usize \{$/m);
  // nothing became visible beyond the parent that was not already
  assert.equal(inner.match(/pub\(crate\)/g).length, 1);
});

test("a test-only nested include keeps its cfg on both lines, and a module with no name beyond its parent is imported, not re-exported", (t) => {
  const root = nestedFixture(t);
  const result = convertUnits(root, ["inner_probe.rs"]);
  assert.deepEqual(result.refused, []);
  assert.equal(result.converted[0].reexportedBeyondParent, false);
  assert.match(
    textOf(result, root, "src/outer.rs"),
    /^include!\("inner\.rs"\);\n#\[cfg\(test\)\]\n#\[path = "inner_probe\.rs"\]\nmod inner_probe;\n#\[cfg\(test\)\]\nuse inner_probe::\*;\n\npub\(crate\) fn run_outer/m,
  );
  // it names nothing its parent provides, so it gets no use super::*
  assert.equal(textOf(result, root, "src/inner_probe.rs"), "pub(super) fn probe_seed() -> usize {\n    7\n}\n");
  assert.deepEqual(verifyTrees(root, converted(t, root, ["inner_probe.rs"])).problems, []);
});

test("a fragment of a nested unit goes with it, and is not a unit while its parent is a fragment", (t) => {
  const files = {
    "src/inner.rs": `${INNER}\ninclude!("inner_leaf.rs");\n`,
    "src/inner_leaf.rs": "fn leaf_value() -> usize {\n    2\n}\n",
    "src/outer.rs": `${OUTER}\nfn leafed() -> usize {\n    leaf_value()\n}\n`,
  };
  const root = nestedFixture(t, files);
  const alone = convertUnits(root, ["inner_leaf.rs"]);
  assert.equal(alone.changes.size, 0);
  assert.match(alone.refused[0].hazards.join("\n"), /its parent inner\.rs is itself a fragment of outer\.rs/);

  const result = convertUnits(root, ["inner.rs"]);
  assert.deepEqual(result.refused, []);
  assert.equal(textOf(result, root, "src/inner_leaf.rs"), "pub(super) fn leaf_value() -> usize {\n    2\n}\n");
  assert.match(textOf(result, root, "src/inner.rs"), /^include!\("inner_leaf\.rs"\);$/m);
  const next = converted(t, root, ["inner.rs"]);
  assert.deepEqual(verifyTrees(root, next).problems, []);
  // Once inner.rs is a module, the file it includes is a unit of its own. But
  // its pub(super) lets outer.rs in, and one module further down would not.
  const later = convertUnits(next, ["inner_leaf.rs"]);
  assert.equal(later.changes.size, 0);
  assert.match(later.refused[0].hazards.join("\n"), /inner_leaf\.rs: pub\(super\) at offset 0 would change meaning/);

  // a fragment only inner.rs reads got no visibility, and follows it down
  const kept = nestedFixture(t, { ...files, "src/outer.rs": OUTER, "src/inner.rs": `${INNER}\ninclude!("inner_leaf.rs");\n\nfn leafed() -> usize {\n    leaf_value()\n}\n` });
  const inside = converted(t, kept, ["inner.rs"]);
  assert.equal(readFileSync(join(inside, "src/inner_leaf.rs"), "utf8"), files["src/inner_leaf.rs"]);
  const below = convertUnits(inside, ["inner_leaf.rs"]);
  assert.deepEqual(below.refused, []);
  assert.equal(below.converted[0].parent, "inner.rs");
  assert.match(textOf(below, inside, "src/inner.rs"), /^#\[path = "inner_leaf\.rs"\]\nmod inner_leaf;\nuse inner_leaf::\*;$/m);
  assert.equal(textOf(below, inside, "src/inner_leaf.rs"), "pub(super) fn leaf_value() -> usize {\n    2\n}\n");
});

test("a nested fragment that already has use super::* keeps it as its own when the parent has another", (t) => {
  const own = INNER.replace("\npub(crate) const", "\nuse super::*;\n\npub(crate) const");
  const root = nestedFixture(t, { "src/inner.rs": own });
  const result = convertUnits(root, ["inner.rs"]);
  assert.deepEqual(result.refused, []);
  const inner = textOf(result, root, "src/inner.rs");
  // no second glob, and no visibility on the one it has
  assert.equal(inner.match(/use super::\*;/g).length, 1);
  assert.match(inner, /^use super::\*;$/m);
  assert.deepEqual(verifyTrees(root, converted(t, root, ["inner.rs"])).problems, []);

  // without one of its own, outer.rs would stop seeing the crate root's names
  const bare = nestedFixture(t, { "src/inner.rs": own, "src/outer.rs": OUTER.replace("use super::*;\n\n", "") });
  const refused = convertUnits(bare, ["inner.rs"]);
  assert.equal(refused.changes.size, 0);
  assert.match(refused.refused[0].hazards.join("\n"), /outer\.rs would lose its own parent's names/);
});

test("two nested fragments that each hold the parent's only use super::* cannot both leave it", (t) => {
  const root = nestedFixture(t, {
    "src/outer.rs": OUTER.replace("use super::*;\n\n", "").replace('include!("inner.rs");', 'include!("inner.rs");\ninclude!("second.rs");'),
    "src/inner.rs": INNER.replace("\npub(crate) const", "\nuse super::*;\n\npub(crate) const"),
    "src/second.rs": "use super::*;\n\npub(crate) fn second_value() -> usize {\n    SHARED_LIMIT\n}\n",
  });
  // second.rs is still pasted into outer.rs, and its glob with it
  assert.deepEqual(convertUnits(root, ["inner.rs"]).refused, []);
  const both = convertUnits(root, ["inner.rs", "second.rs"]);
  assert.equal(both.changes.size, 0);
  assert.deepEqual(both.refused.map((entry) => entry.unit), ["inner.rs", "second.rs"]);
});

test("a nested unit may reach its own top level from an inline module, and name its parent by an absolute path", (t) => {
  // `super` inside `mod checks` meant outer and means inner, which holds outer's names;
  // `pub(in crate::outer)` at the top level names an ancestor before and after
  const root = nestedFixture(t, {
    "src/inner.rs": `${INNER}\npub(in crate::outer) fn scoped() {}\n\nmod checks {\n    use super::*;\n\n    fn reads() -> usize {\n        super::seed() + SHARED_LIMIT\n    }\n}\n`,
  });
  assert.deepEqual(convertUnits(root, ["inner.rs"]).refused, []);
});

const nestedRefusals = [
  {
    name: "a super:: path at its top level, which meant the module above its parent",
    files: { "src/inner.rs": `${INNER}\nfn upward() -> usize {\n    super::main_value()\n}\n` },
    hazard: /inner\.rs: the super:: path at offset \d+ would change meaning/,
  },
  {
    name: "a use of a super:: path other than the glob",
    files: { "src/inner.rs": INNER.replace("\npub(crate) const", "\nuse super::HashMap as Map;\n\npub(crate) const") },
    hazard: /inner\.rs: the super:: path at offset \d+ would change meaning/,
  },
  {
    name: "a super::super path in an inline module, which climbs past the file",
    files: { "src/inner.rs": `${INNER}\nmod checks {\n    fn reads() {\n        super::super::main_value();\n    }\n}\n` },
    hazard: /inner\.rs: the super:: path/,
  },
  {
    name: "a string that spells a super:: path",
    files: { "src/inner.rs": `${INNER}\nconst FALLBACK: &str = "super::fallback";\n` },
    hazard: /spells a super:: path/,
  },
  {
    name: "module_path!(), whose value would gain the new module's name",
    files: { "src/inner.rs": `${INNER}\nfn where_am_i() -> &'static str {\n    module_path!()\n}\n` },
    hazard: /inner\.rs: module_path!\(\) at offset \d+ would name the new module/,
  },
  {
    name: "pub(super) at its top level, which meant the module above its parent",
    files: { "src/inner.rs": `${INNER}\npub(super) fn upward() {}\n` },
    hazard: /inner\.rs: pub\(super\)/,
  },
  {
    name: "pub(in crate::...) in an inline module, which may name a module inside the file",
    files: { "src/inner.rs": `${INNER}\nmod deep {\n    pub(in crate::outer::deep) fn reach() {}\n}\n` },
    hazard: /pub\(incrate::outer::deep\)/,
  },
  {
    name: "a file that two parents include",
    files: {
      "src/main.rs": `${NEST_MAIN}mod other;\n`,
      "src/other.rs": 'use super::*;\n\ninclude!("inner.rs");\n',
    },
    hazard: /inner\.rs: included 2 times \(by (other\.rs, outer\.rs|outer\.rs, other\.rs)\), so it has no single parent/,
  },
  {
    name: "a use super::* of its own that carries an attribute",
    files: { "src/inner.rs": INNER.replace("\npub(crate) const", "\n#[cfg(test)]\nuse super::*;\n\npub(crate) const") },
    hazard: /its use super::\*; carries an attribute or a visibility/,
  },
  {
    name: "crate-visible names that only test builds have, beside names every build has",
    files: { "src/inner.rs": INNER.replace("pub(crate) const", "#[cfg(test)]\npub(crate) const") },
    hazard: /exist only in test builds/,
  },
  {
    name: "names that reach beyond its parent only as far as a path",
    files: { "src/inner.rs": INNER.replace("pub(crate) const", "pub(in crate::outer) const") },
    hazard: /are restricted to a path/,
  },
  {
    name: "a module of its name that its parent may already see",
    files: { "src/main.rs": `${NEST_MAIN}\nmod inner {\n    pub(crate) fn elsewhere() {}\n}\n` },
    hazard: /main\.rs declares a module named inner/,
  },
  {
    name: "an include!() that carries an attribute other than #[cfg(test)]",
    files: { "src/outer.rs": OUTER.replace('include!("inner.rs");', '#[cfg(unix)]\ninclude!("inner.rs");') },
    hazard: /carries an attribute other than #\[cfg\(test\)\]/,
  },
];

for (const refusal of nestedRefusals) {
  test(`convert refuses a nested unit with ${refusal.name}, and changes nothing`, (t) => {
    const root = nestedFixture(t, refusal.files);
    const result = convertUnits(root, ["inner.rs"]);
    assert.equal(result.changes.size, 0);
    assert.deepEqual(result.converted, []);
    assert.equal(result.refused.length, 1);
    assert.match(result.refused[0].hazards.join("\n"), refusal.hazard);
    // and --write, asked for together with a unit that could be converted, writes nothing
    const paths = ["src/main.rs", "src/outer.rs", "src/inner.rs", "src/inner_probe.rs"];
    const before = snapshot(root, paths);
    const run = spawnSync(process.execPath, [scriptFile, "convert", root, "inner_probe.rs,inner.rs", "--write"], { encoding: "utf8" });
    assert.equal(run.status, 1);
    assert.equal(JSON.parse(run.stdout).written, 0);
    assert.deepEqual(snapshot(root, paths), before);
  });
}

test("analyze lists the units of every parent, and no include written inside an inline module", (t) => {
  const root = nestedFixture(t, {
    "src/main.rs": `${NEST_MAIN}include!("rooted.rs");\n`,
    "src/rooted.rs": "fn rooted_value() -> usize {\n    4\n}\n",
    "src/outer.rs": `${OUTER}\nmod grouped {\n    include!("grouped_part.rs");\n}\n`,
    "src/grouped_part.rs": "pub(crate) fn grouped_value() -> usize {\n    5\n}\n",
  });
  const run = spawnSync(process.execPath, [scriptFile, "analyze", root], { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  const report = JSON.parse(run.stdout);
  assert.deepEqual(
    report.units.map((unit) => [unit.parent, unit.unit, unit.cfgTest, unit.hazards]),
    [
      ["main.rs", "rooted.rs", false, []],
      ["outer.rs", "inner.rs", false, []],
      ["outer.rs", "inner_probe.rs", true, []],
    ],
  );
  assert.equal(report.totals.units, 3);
  // the indented include is not a unit to convert either
  const result = convertUnits(root, ["grouped_part.rs"]);
  assert.equal(result.changes.size, 0);
  assert.match(result.refused[0].hazards.join("\n"), /grouped_part\.rs is not included at the top level/);
});

// outer.rs with three fragments in a row: one re-exported, one with nothing
// to hand on, one compiled for tests only.
function nestedBase(t) {
  return nestedFixture(t, {
    "src/outer.rs": `${OUTER.replace('include!("inner.rs");', 'include!("inner.rs");\ninclude!("inner_shape.rs");')}\nfn doubled() -> usize {\n    new_tally().doubled()\n}\n`,
    "src/inner_shape.rs": "impl Tally {\n    fn doubled(&self) -> usize {\n        self.count * 2\n    }\n}\n",
  });
}

test("verify accepts the tool's own conversion of nested units, as written and as cargo fmt leaves it", (t) => {
  const base = nestedBase(t);
  const next = converted(t, base, ["inner.rs", "inner_shape.rs", "inner_probe.rs"]);
  const written = verifyTrees(base, next);
  assert.deepEqual(written.problems, []);
  assert.equal(written.convertedUnits, 3);
  // Tally, count, new_tally, doubled and probe_seed
  assert.equal(written.insertedVisibility, 5);
  formatTree(next);
  // a module with nothing to hand on gets no glob, and a blank line keeps
  // rustfmt from sorting its mod line with the one below
  assert.match(
    readFileSync(join(next, "src/outer.rs"), "utf8"),
    /^pub\(crate\) use inner::\*;\n#\[path = "inner_shape\.rs"\]\nmod inner_shape;\n\n#\[cfg\(test\)\]\n#\[path = "inner_probe\.rs"\]\nmod inner_probe;\n#\[cfg\(test\)\]\nuse inner_probe::\*;$/m,
  );
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

test("verify accepts an unformatted nested fragment once rustfmt has reached it", (t) => {
  const base = nestedFixture(t, { "src/inner_probe.rs": "fn probe_seed()->usize{7}\n" });
  const next = converted(t, base, ["inner.rs", "inner_probe.rs"]);
  formatTree(next);
  assert.equal(readFileSync(join(next, "src/inner_probe.rs"), "utf8"), "pub(super) fn probe_seed() -> usize {\n    7\n}\n");
  assert.deepEqual(verifyTrees(base, next).problems, []);
});

const nestedTampering = [
  {
    name: "a changed literal",
    edit: (text) => text.replace("    1\n", "    2\n"),
    file: "src/inner.rs",
  },
  {
    name: "a re-export the plan calls for that is missing",
    edit: (text) => text.replace("mod inner;\npub(crate) use inner::*;\n", "mod inner;\n\n"),
    file: "src/outer.rs",
  },
  {
    name: "a re-export narrowed to an import",
    edit: (text) => text.replace("pub(crate) use inner::*;", "use inner::*;"),
    file: "src/outer.rs",
  },
  {
    name: "an import widened to a re-export",
    edit: (text) => text.replace("use inner_probe::*;", "pub(crate) use inner_probe::*;"),
    file: "src/outer.rs",
  },
  {
    name: "a re-export wider than pub(crate)",
    edit: (text) => text.replace("pub(crate) use inner::*;", "pub use inner::*;"),
    file: "src/outer.rs",
  },
  {
    name: "a #[path] that names another file",
    edit: (text) => text.replace('#[path = "inner.rs"]', '#[path = "inner_probe.rs"]'),
    file: "src/outer.rs",
  },
  {
    name: "an inserted visibility wider than pub(super)",
    edit: (text) => text.replace("pub(super) fn new_tally", "pub(crate) fn new_tally"),
    file: "src/inner.rs",
  },
  {
    name: "a pub(super) where the plan puts none",
    edit: (text) => text.replace("fn seed()", "pub(super) fn seed()"),
    file: "src/inner.rs",
  },
  {
    name: "a pub(super) the plan puts that is missing",
    edit: (text) => text.replace("pub(super) fn new_tally", "fn new_tally"),
    file: "src/inner.rs",
  },
  {
    name: "a second statement in the parent",
    edit: (text) => text.replace("    let tally = new_tally();\n", "    let tally = new_tally();\n    let _ = seed();\n"),
    file: "src/outer.rs",
  },
];

for (const { name, edit, file } of nestedTampering) {
  test(`verify rejects a nested conversion with ${name}`, (t) => {
    const base = nestedBase(t);
    const next = converted(t, base, ["inner.rs", "inner_shape.rs", "inner_probe.rs"]);
    const path = join(next, file);
    const before = readFileSync(path, "utf8");
    const after = edit(before);
    assert.notEqual(after, before);
    writeFileSync(path, after);
    const result = verifyTrees(base, next);
    assert.equal(result.problems.length, 1, JSON.stringify(result.problems));
    assert.ok(result.problems[0].startsWith(file.replace("src/", "")));
  });
}

test("verify cannot pass a nested unit converted by hand that the tool refuses", (t) => {
  // the fragment reaches above its parent, which the tool will not rewrite
  const inner = `${INNER}\nfn upward() -> usize {\n    super::main_value()\n}\n`;
  const base = nestedFixture(t, { "src/inner.rs": inner });
  const next = mkdtempSync(join(testTempDirectory(), "include-to-mod-nested-by-hand-"));
  t.after(() => rmSync(next, { recursive: true, force: true }));
  cpSync(base, next, { recursive: true });
  writeFileSync(join(next, "src/outer.rs"), OUTER.replace('include!("inner.rs");', '#[path = "inner.rs"]\nmod inner;\npub(crate) use inner::*;'));
  writeFileSync(join(next, "src/inner.rs"), `use super::*;\n\n${inner.replace("super::main_value()", "super::super::main_value()")}`);
  assert.match(verifyTrees(base, next).problems.join("\n"), /inner\.rs: the tool refuses to convert this unit/);
});

test("units of src/main.rs and of another parent convert together, each with its own wiring and visibility", (t) => {
  const base = nestedFixture(t, {
    "src/main.rs": `${NEST_MAIN}include!("rooted.rs");\n`,
    "src/rooted.rs": "fn rooted_value() -> usize {\n    4\n}\n",
    "src/outer.rs": `${OUTER}\nfn rooted() -> usize {\n    rooted_value()\n}\n`,
  });
  const result = convertUnits(base, ["rooted.rs", "inner.rs"]);
  assert.deepEqual(result.refused, []);
  assert.deepEqual(result.converted.map((unit) => unit.parent), ["main.rs", "outer.rs"]);
  assert.match(textOf(result, base, "src/main.rs"), /^mod rooted;\nuse rooted::\*;$/m);
  assert.equal(textOf(result, base, "src/rooted.rs"), "pub(crate) fn rooted_value() -> usize {\n    4\n}\n");
  assert.match(textOf(result, base, "src/inner.rs"), /^pub\(super\) fn new_tally/m);
  assert.deepEqual(verifyTrees(base, converted(t, base, ["rooted.rs", "inner.rs"])).problems, []);
});
