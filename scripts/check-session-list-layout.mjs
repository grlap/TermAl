// Browser geometry guard for the session-row markup in AppControlSurface.
// Uses the real stylesheets; no backend, dev server, or browser installation.
// Run from the repository root with an absolute Chrome/Chromium executable:
// node scripts/check-session-list-layout.mjs --chrome-executable PATH
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
if (args.length !== 2 || args[0] !== '--chrome-executable' || !isAbsolute(args[1]) || !existsSync(args[1])) {
  throw new Error('Pass --chrome-executable followed by an existing absolute Chrome/Chromium path.');
}
const root = resolve('.');
for (const file of ['ui/src/styles.css', 'ui/src/themes/index.css', 'ui/src/session-list-window.css']) {
  if (!existsSync(join(root, file))) throw new Error('Run from the TermAl repository root.');
}
mkdirSync(join(root, '.tmp'), { recursive: true });
const output = mkdtempSync(join(root, '.tmp/session-list-layout-check-'));
const stylesheet = file => pathToFileURL(join(root, file)).href;

function checkLayout() {
  const failures = [];
  const observations = [];
  // Widths observed in actual standalone-minimum, docked-minimum and wider lists.
  for (const width of [186, 447, 574]) {
    for (const grouped of [false, true]) {
      for (const remote of [false, true]) {
        for (const name of ['Claude', 'Engram::Coder-VerificationWorkspaceSol', 'Session with a longer readable name']) {
          for (const label of ['', 'Foreground turn', 'Foreground turn + background work']) {
            for (const count of ['', '12 hits', 'Transcript not loaded']) {
              const scenario = { width, grouped, remote, name, label, count };
              const list = document.createElement('div');
              list.className = 'session-list';
              list.style.width = `${width}px`;
              const entries = document.createElement('div');
              entries.className = 'session-list-entries';
              list.append(entries);
              let parent = entries;
              if (grouped) {
                const group = document.createElement('section');
                group.className = 'session-orchestrator-group session-window-group';
                const members = document.createElement('div');
                members.className = 'session-orchestrator-group-list';
                const row = document.createElement('div');
                row.className = 'session-window-group-row last';
                entries.append(group); group.append(members); members.append(row); parent = row;
              }
              const shell = document.createElement('div');
              shell.className = `session-row-shell${remote ? ' remote' : ''}`;
              const row = document.createElement('button'); row.className = 'session-row';
              const copy = document.createElement('div'); copy.className = 'session-copy';
              const title = document.createElement('div'); title.className = 'session-title-line';
              const strong = document.createElement('strong'); strong.textContent = name;
              title.append(strong);
              for (const [className, text] of [['session-work-label', label], ['session-search-count', count]]) {
                if (!text) continue;
                const badge = document.createElement('span'); badge.className = className;
                badge.textContent = text; title.append(badge);
              }
              const preview = document.createElement('div'); preview.className = 'session-preview';
              preview.textContent = 'An ordinary session preview';
              copy.append(title, preview); row.append(copy); shell.append(row); parent.append(shell);
              document.body.append(list);
              const range = document.createRange(); range.selectNodeContents(strong);
              const lines = new Set([...range.getClientRects()].map(rect => Math.round(rect.top))).size;
              const nameRect = strong.getBoundingClientRect();
              const titleRect = title.getBoundingClientRect();
              const right = titleRect.right - parseFloat(getComputedStyle(title).paddingRight);
              const badges = [...title.querySelectorAll('span')].map(badge => {
                const rect = badge.getBoundingClientRect();
                return { text: badge.textContent, width: rect.width, right: rect.right,
                  overlapsName: rect.left < nameRect.right - 1 && rect.right > nameRect.left + 1
                    && rect.top < nameRect.bottom - 1 && rect.bottom > nameRect.top + 1 };
              });
              const observation = { ...scenario, clientWidth: list.clientWidth, scrollWidth: list.scrollWidth,
                nameWidth: nameRect.width, nameLines: lines, badges };
              observations.push(observation);
              if (list.clientWidth < width - 1 || nameRect.width <= 0 || lines === 0
                  || list.scrollWidth > list.clientWidth + 1 || nameRect.right > right + 1
                  || (name === 'Claude' && lines !== 1)
                  || badges.some(badge => badge.width <= 0 || badge.right > right + 1 || badge.overlapsName)) {
                failures.push(observation);
              }
              list.remove();
            }
          }
        }
      }
    }
  }
  const result = { cases: observations.length, failures, observations };
  document.body.dataset.layoutResult = btoa(JSON.stringify(result));
}

const html = `<!doctype html><html><head><meta charset="utf-8">
<link rel="stylesheet" href="${stylesheet('ui/src/themes/index.css')}">
<link rel="stylesheet" href="${stylesheet('ui/src/styles.css')}">
<link rel="stylesheet" href="${stylesheet('ui/src/session-list-window.css')}">
</head><body><script>window.addEventListener('load', ${checkLayout.toString()});</script></body></html>`;
const fixture = join(output, 'fixture.html');
writeFileSync(fixture, html);
const browser = spawnSync(args[1], ['--headless=new', '--no-first-run', '--no-default-browser-check',
  '--disable-extensions', '--disable-background-networking', '--host-resolver-rules=MAP * ~NOTFOUND',
  '--window-size=1280,1000', `--user-data-dir=${join(output, 'chrome-profile')}`,
  '--dump-dom', pathToFileURL(fixture).href], { encoding: 'utf8', windowsHide: true,
  timeout: 30_000, maxBuffer: 8 * 1024 * 1024 });
writeFileSync(join(output, 'browser.stderr.log'), browser.stderr ?? '');
writeFileSync(join(output, 'browser.stdout.html'), browser.stdout ?? '');
if (browser.error || browser.status !== 0) throw new Error(`Browser failed; see ${output}: ${browser.error ?? browser.status}`);
const encoded = /data-layout-result="([A-Za-z0-9+/=]+)"/.exec(browser.stdout ?? '')?.[1];
if (!encoded) throw new Error(`Browser produced no layout observations; see ${output}`);
const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
writeFileSync(join(output, 'result.json'), JSON.stringify(result, null, 2));
console.log(JSON.stringify({ cases: result.cases, failures: result.failures.length, output }));
if (result.cases !== 324 || result.failures.length) process.exitCode = 1;
