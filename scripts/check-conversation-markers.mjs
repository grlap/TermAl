// Real Chrome guard for the React marker window and its complete stylesheet cascade.
// Run from the repository root after npm ci in ui/:
// node scripts/check-conversation-markers.mjs --chrome-executable PATH
// This renders the production component without a backend or a ui/dist build.
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
if (args.length !== 2 || args[0] !== '--chrome-executable' || !isAbsolute(args[1]) || !existsSync(args[1])) {
  throw new Error('Pass --chrome-executable followed by an existing absolute Chrome/Chromium path.');
}
const root = resolve('.');
const requireUi = createRequire(join(root, 'ui/package.json'));
const { build } = requireUi('esbuild');
mkdirSync(join(root, '.tmp'), { recursive: true });
const output = mkdtempSync(join(root, '.tmp/conversation-markers-check-'));
const stylesheet = file => pathToFileURL(join(root, file)).href;

const fixture = await build({
  absWorkingDir: join(root, 'ui'),
  stdin: {
    resolveDir: join(root, 'ui'),
    loader: 'tsx',
    contents: `
      import React from 'react';
      import { createRoot } from 'react-dom/client';
      import { flushSync } from 'react-dom';
      import { ConversationMarkerFloatingWindow } from './src/panels/conversation-markers';

      const failures = [];
      const observations = [];
      for (const theme of ['warm-light', 'dark']) {
        for (const density of [1]) {
          for (const empty of [false, true]) {
            const section = document.createElement('section');
            section.dataset.theme = theme;
            section.style.cssText = '--density-scale:' + density + ';padding:16px;background:var(--paper);color:var(--ink)';
            section.innerHTML = '<h2>' + theme + ' / ' + density + (empty ? ' / empty' : '') + '</h2><div></div>';
            document.body.append(section);
            const calls = { previous: 0, next: 0, close: 0, jump: 0 };
            const marker = {
              id: 'marker', sessionId: 'fixture', kind: 'checkpoint', name: 'Review point',
              color: '#3b82f6', messageId: 'message', messageIndexHint: 10,
              createdAt: '2026-01-01', updatedAt: '2026-01-01', createdBy: 'user',
            };
            const reactRoot = createRoot(section.lastElementChild);
            flushSync(() => reactRoot.render(<ConversationMarkerFloatingWindow
              markers={empty ? [] : [marker]} activeMarkerId={null}
              onClose={() => calls.close++} onJump={() => calls.jump++}
              onNavigatePrevious={() => calls.previous++} onNavigateNext={() => calls.next++}
            />));
            const controls = [...section.querySelectorAll('.conversation-marker-nav-button')];
            if (controls.length !== 3) failures.push({ theme, density, empty, reason: 'Expected three header controls' });
            for (const [index, button] of controls.entries()) {
              const name = ['Previous marker', 'Next marker', 'Hide markers window'][index];
              const svg = button.querySelector('svg');
              const path = svg?.querySelector('path');
              const rect = svg?.getBoundingClientRect();
              const css = getComputedStyle(button);
              const fill = path && getComputedStyle(path).fill;
              const observation = {
                theme, density, empty, name: button.getAttribute('aria-label'),
                width: rect?.width, height: rect?.height, padding: css.padding,
                fill, disabled: button.disabled,
              };
              observations.push(observation);
              if (button.getAttribute('aria-label') !== name || button.title !== name ||
                  !rect || Math.abs(rect.width - 14 * density) > 0.5 ||
                  Math.abs(rect.height - 14 * density) > 0.5 ||
                  !path?.getAttribute('d') || fill === 'none' || fill === 'rgba(0, 0, 0, 0)' ||
                  button.disabled !== (empty && index < 2)) {
                failures.push({ ...observation, reason: 'Header icon, name, or enabled state is incorrect' });
              }
              if (!button.disabled) {
                button.focus();
                if (document.activeElement !== button) failures.push({ ...observation, reason: 'Control cannot take focus' });
                button.click();
              }
            }
            if (calls.previous !== (empty ? 0 : 1) || calls.next !== (empty ? 0 : 1) || calls.close !== 1) {
              failures.push({ theme, density, empty, calls, reason: 'Header actions did not reach their handlers' });
            }
          }
        }
      }
      document.body.dataset.markerResult = btoa(JSON.stringify({ observations, failures }));
    `,
  },
  bundle: true,
  write: false,
  jsx: 'automatic',
  define: { 'process.env.NODE_ENV': '"production"' },
});
const html = `<!doctype html><html><head><meta charset="utf-8">
  <link rel="stylesheet" href="${stylesheet('ui/src/themes/index.css')}">
  <link rel="stylesheet" href="${stylesheet('ui/src/styles.css')}">
  <style>body{display:grid;grid-template-columns:repeat(2,1fr);gap:8px;padding:8px}h2{font-size:14px}</style>
  </head><body><script>${fixture.outputFiles[0].text.replaceAll('</script', '<\\/script')}</script></body></html>`;
const htmlPath = join(output, 'fixture.html');
writeFileSync(htmlPath, html);
const chrome = spawnSync(args[1], [
  '--headless=new', '--no-first-run', '--no-default-browser-check',
  '--disable-extensions', '--disable-background-networking',
  '--host-resolver-rules=MAP * ~NOTFOUND', '--window-size=1280,1800',
  `--user-data-dir=${join(output, 'chrome-profile')}`,
  '--dump-dom', `--screenshot=${join(output, 'screenshot.png')}`,
  pathToFileURL(htmlPath).href,
], { encoding: 'utf8', windowsHide: true, timeout: 30_000, maxBuffer: 8 * 1024 * 1024 });
writeFileSync(join(output, 'chrome.stdout.html'), chrome.stdout ?? '');
writeFileSync(join(output, 'chrome.stderr.log'), chrome.stderr ?? '');
const encoded = chrome.stdout?.match(/data-marker-result="([^"]+)"/)?.[1];
if (chrome.error || chrome.status !== 0 || !encoded) {
  throw new Error(`Chrome did not produce marker observations. Inspect ${output}: ${chrome.error ?? chrome.status}`);
}
const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
writeFileSync(join(output, 'result.json'), JSON.stringify(result, null, 2));
console.log(JSON.stringify({ output, observations: result.observations.length, failures: result.failures }, null, 2));
if (result.observations.length !== 12 || result.failures.length > 0) process.exitCode = 1;
