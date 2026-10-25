// Test-only locked UI and local npm witness for fresh launcher repositories.
// Owns fixture files, not production setup, Git commits or network installs.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { uiDependencyFiles } from "./test-ui-bootstrap.mjs";

export function writeUiBootstrapFixture(root, metadata) {
  const ui = join(root, "ui");
  mkdirSync(ui, { recursive: true });
  writeFileSync(join(root, ".gitignore"), "/ui/node_modules\n");
  writeFileSync(join(ui, "package.json"), JSON.stringify({ name: "ui-fixture", version: "1.0.0", private: true }));
  writeFileSync(join(ui, "package-lock.json"), JSON.stringify({
    name: "ui-fixture", version: "1.0.0", lockfileVersion: 3,
    packages: { "": { name: "ui-fixture", version: "1.0.0" } },
  }));
  const events = join(metadata, "ui-events.jsonl");
  const npm = join(metadata, "npm-cli.js");
  // tsc is extensionless CommonJS, while vitest.mjs is an ES module.
  const stageSource = "Promise.all([import('node:fs'), import('node:path')]).then(([fs, path]) => " +
    "fs.appendFileSync(process.env.FIXTURE_UI_EVENTS, JSON.stringify(path.basename(process.argv[1])) + String.fromCharCode(10)));";
  writeFileSync(npm, [
    "const assert = require('node:assert/strict');",
    "const fs = require('node:fs'); const path = require('node:path');",
    "assert.deepEqual(process.argv.slice(2), ['ci', '--include=dev', '--no-audit', '--no-fund']);",
    "assert.equal(fs.realpathSync.native(process.cwd()), fs.realpathSync.native(process.env.FIXTURE_UI_CWD));",
    "fs.appendFileSync(process.env.FIXTURE_UI_EVENTS, JSON.stringify('npm-ci') + String.fromCharCode(10));",
    "console.log('fixture npm ci: locked development dependencies');",
    "if (process.env.FIXTURE_UI_MODE === 'fail') { console.error('fixture install error'); process.exit(23); }",
    "if (process.env.FIXTURE_UI_MODE === 'missing-entrypoints') process.exit(0);",
    ...uiDependencyFiles.map((name) => `{
      const target = path.join(process.cwd(), ${JSON.stringify(name)});
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, ${JSON.stringify(stageSource)});
    }`),
    "const lock = JSON.parse(fs.readFileSync('package-lock.json', 'utf8')); delete lock.packages[''];",
    "fs.writeFileSync('node_modules/.package-lock.json', JSON.stringify({ lockfileVersion: 3, packages: lock.packages }));",
  ].join("\n"));
  return {
    npm_execpath: npm, FIXTURE_UI_CWD: ui,
    FIXTURE_UI_EVENTS: events, FIXTURE_UI_MODE: "pass",
  };
}
