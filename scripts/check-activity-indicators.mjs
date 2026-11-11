// Real-browser activity, accessibility and reserved-geometry guard.
// Run from the repository root after npm ci in ui/:
// node scripts/check-activity-indicators.mjs --chrome-executable PATH
// Uses production React components/styles, without a backend or ui/dist build.
import { spawn } from 'node:child_process';
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
const output = mkdtempSync(join(root, '.tmp/activity-indicators-check-'));
await build({
  absWorkingDir: join(root, 'ui'),
  stdin: {
    resolveDir: join(root, 'ui'), loader: 'tsx', contents: `
      import React from 'react';
      import { createRoot } from 'react-dom/client';
      import { flushSync } from 'react-dom';
      import { TranscriptActivitySlot } from './src/transcript-activity-slot';
      import { SessionActivityStrip } from './src/panels/session-activity-cards';
      import { RuntimeActionButton } from './src/runtime-action-button';
      import { startActivityAnimationVisibility } from './src/activity-animation-visibility';
      import './src/themes/index.css';
      import './src/styles.css';
      window.stopActivityVisibility = startActivityAnimationVisibility(document.documentElement);
      window.waitForActivity = predicate => new Promise((resolve, reject) => {
        if (predicate()) { resolve(); return; }
        const observer = new MutationObserver(() => {
          if (predicate()) { clearTimeout(guard); observer.disconnect(); resolve(); }
        });
        const guard = setTimeout(() => { observer.disconnect(); reject(Error('Visibility observation did not settle')); }, 5000);
        observer.observe(document.documentElement, {subtree:true, attributes:true});
      });
      window.checkActivity = async () => {
      const failures = [], observations = [], reservedHeights = new Map();
      const host = document.getElementById('root'), reactRoot = createRoot(host);
      const render = (count, status) => flushSync(() => reactRoot.render(<>
        {Array.from({length: count}, (_, i) => {
          const sessionStatus = status === 'background' ? 'idle' : status;
          const session = {id: 'session-'+i, name: 'Session', emoji: '', agent: 'Codex',
            workdir: '/fixture', model: 'fixture', status:sessionStatus, preview: '', messages: []};
          return <section key={i}>
            <span className="status-agent-badge pane-tab-agent-badge" data-status={sessionStatus}
              data-work={status === 'background' ? 'background' : undefined}>C</span>
            <TranscriptActivitySlot session={session}/><SessionActivityStrip session={session}/>
            <RuntimeActionButton action="stop" ariaLabel="Stop agent" title="Stop agent"
              classNamePrefix="fixture-action" isPending onClick={() => {}}/>
            <span className="composer-slash-status-spinner" aria-hidden="true"/>
            <span className="composer-slash-option-spinner" aria-hidden="true"/>
            <span>Loading</span>
          </section>;
        })}
      </>));
      for (const theme of ['warm-light', 'dark']) {
        document.documentElement.dataset.theme = theme;
        for (const count of [1, 6]) for (const status of ['active', 'stopping', 'background', 'idle']) {
          render(count, status);
          for (const placement of ['visible', 'offscreen', 'hidden']) {
            host.style.transform = placement === 'offscreen' ? 'translateY(2000px)' : '';
            host.style.display = placement === 'hidden' ? 'none' : '';
            await window.waitForActivity(() => [...host.querySelectorAll('.status-agent-badge, .activity-spinner, .composer-slash-status-spinner, .composer-slash-option-spinner, .message-activity-squares')]
              .every(e => e.dataset.activityVisible === String(placement === 'visible')));
            const slots = [...host.querySelectorAll('.transcript-activity-slot')];
            const sizes = slots.map(slot => slot.getBoundingClientRect().height);
            const strips = [...host.querySelectorAll('.session-activity-strip')];
            const stripHeights = strips.map(strip => strip.getBoundingClientRect().height);
            // Read actual browser timelines, including pseudo-elements and dots.
            const animations = document.getAnimations().filter(a => a instanceof CSSAnimation && a.playState !== 'finished');
            const timings = animations.map(a => {
              const timing = a.effect.getTiming();
              // CSS animation-timing-function belongs to each keyframe segment;
              // getTiming().easing is the enclosing effect's (linear) easing.
              const frames = a.effect.getKeyframes();
              const segments = frames.slice(0, -1).map((frame, index) => {
                const duration = Number(timing.duration) * (frames[index + 1].computedOffset - frame.computedOffset);
                const steps = Number(frame.easing.match(/^steps\\((\\d+)(?:, end)?\\)$/)?.[1]);
                return {easing:frame.easing, duration, steps, framesPerSecond:steps * 1000 / duration};
              });
              return {name:a.animationName, duration:timing.duration, easing:timing.easing, segments};
            });
            const observation = {theme, count, status, placement, sizes, stripHeights,
              animations: animations.length, timings, reduced: matchMedia('(prefers-reduced-motion: reduce)').matches};
            observations.push(observation);
            if (strips.length !== count || stripHeights.some(height => Math.abs(height - (placement === 'hidden' ? 0 : 3)) > .5)) {
              failures.push({...observation, reason: 'Status changes the reserved three-pixel strip'});
            }
            const reduced = observation.reduced;
            const hasDots = status === 'active' || status === 'stopping';
            const expectedAnimations = reduced || placement === 'hidden' ? 0 : count * (status === 'idle' ? 3 : hasDots ? 7 : 4);
            if (animations.length !== expectedAnimations ||
                animations.some(a => a.playState !== (placement === 'visible' ? 'running' : 'paused'))) {
              failures.push({...observation, reason: 'Activity motion does not follow visibility/motion preference'});
            }
            if (!reduced && placement === 'visible' && animations.some(a => a.startTime !== 0)) {
              failures.push({...observation, reason: 'Independent activity phases multiply page draws'});
            }
            if (timings.some(timing => timing.easing !== 'linear' || !timing.segments.length ||
                timing.segments.some(segment => segment.framesPerSecond !== 30))) {
              failures.push({...observation, reason: 'Activity motion is not 30 frames per second'});
            }
            if (!reduced && placement === 'visible' && hasDots) {
              const dots = animations.filter(a => a.animationName === 'transcript-activity-lift');
              for (const animation of dots) {
                animation.currentTime = 0;
                const before = getComputedStyle(animation.effect.target).transform;
                animation.currentTime = Number(animation.effect.getTiming().delay) + 400;
                const after = getComputedStyle(animation.effect.target).transform;
                animation.startTime = 0;
                if (before === after) failures.push({...observation, reason:'Working dots do not move'});
              }
            }
            if (placement !== 'hidden' && sizes.some(h => h <= 0 || Math.abs(h - sizes[0]) > .5)) {
              failures.push({...observation, reason: 'Activity slot loses its reserved line'});
            }
            if (placement !== 'hidden') {
              const key = theme+'-'+count;
              if (!reservedHeights.has(key)) reservedHeights.set(key, sizes[0]);
              if (sizes.some(h => Math.abs(h - reservedHeights.get(key)) > .5)) {
                failures.push({...observation, reason: 'Changing status shifts transcript geometry'});
              }
            }
            const expected = status === 'active' ? 'working' : status === 'stopping' ? 'stopping' : null;
            if (slots.length !== count || slots.some(slot => expected
              ? !slot.textContent.includes('Agent is '+expected) : slot.textContent !== '')) {
              failures.push({...observation, reason: 'Working/stopping label is lost or idle label is stale'});
            }
            const announcements = [...host.querySelectorAll('[role="status"]')];
            if (announcements.length !== count || announcements.some(a => !a.textContent.includes(
              'Codex is '+(expected ?? 'idle')))) {
              failures.push({...observation, reason: 'Session announcement is lost'});
            }
            if (placement !== 'hidden') {
              const strip = host.querySelector('.session-activity-strip'); strip.focus();
              const label = host.querySelector('.message-activity-label');
              if (document.activeElement !== strip || (label &&
                  (getComputedStyle(label).visibility !== 'visible' || label.getBoundingClientRect().width <= 0))) {
                failures.push({...observation, reason: 'Status cannot be read or focused'});
              }
              const fill = strip.querySelector('.session-activity-strip-fill');
              const ring = host.querySelector('.status-agent-badge');
              if (expected && (getComputedStyle(fill).backgroundColor === 'rgba(0, 0, 0, 0)' ||
                  parseFloat(getComputedStyle(strip.querySelector('.session-activity-strip-track')).opacity) <= 0 ||
                  parseFloat(getComputedStyle(ring, '::before').borderTopWidth) <= 0)) {
                failures.push({...observation, reason: 'Static activity color or ring is invisible'});
              }
            }
          }
        }
      }
      host.style.transform = ''; host.style.display = ''; render(6, 'active');
      await window.waitForActivity(() => [...host.querySelectorAll('[data-activity-visible]')].every(e => e.dataset.activityVisible === 'true'));
      // A keyed row move may recreate its CSSAnimation without an intersection.
      host.prepend(host.lastElementChild);
      await Promise.resolve();
      const moved = document.getAnimations().filter(a => a instanceof CSSAnimation);
      if (!matchMedia('(prefers-reduced-motion: reduce)').matches && moved.some(a => a.startTime !== 0)) {
        failures.push({reason:'Moving a working row loses its synchronized phase'});
      }
      return {observations, failures};
      };
    `,
  },
  bundle: true, jsx: 'automatic', outfile: join(output, 'fixture.js'),
  define: { 'process.env.NODE_ENV': '"production"' },
  loader: {'.woff2': 'file', '.woff': 'file', '.ttf': 'file', '.svg': 'file', '.png': 'file'},
});
const html = join(output, 'fixture.html');
writeFileSync(html, `<!doctype html><html><head><meta charset="utf-8">
  <link rel="stylesheet" href="${pathToFileURL(join(output, 'fixture.css')).href}">
  <style>section{padding:12px;border:1px solid #888}#root{display:grid;grid-template-columns:repeat(3,1fr)}</style>
  </head><body><div id="root"></div><script src="${pathToFileURL(join(output, 'fixture.js')).href}"></script></body></html>`);
async function connect(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let id = 0;
  const pending = new Map();
  socket.onclose = () => {
    for (const response of pending.values()) response.reject(new Error('Chrome debugging connection closed'));
    pending.clear();
  };
  socket.onmessage = event => {
    const message = JSON.parse(event.data);
    const response = pending.get(message.id);
    if (response) {
      pending.delete(message.id);
      if (message.error) response.reject(new Error(JSON.stringify(message.error)));
      else response.resolve(message.result);
    }
  };
  return {
    send(method, params = {}, sessionId) {
      return new Promise((resolve, reject) => {
        const requestId = ++id;
        pending.set(requestId, {resolve, reject});
        socket.send(JSON.stringify({id:requestId, method, params, sessionId}));
      });
    },
    close() { socket.close(); },
  };
}
for (const reduced of [false, true]) {
  const mode = reduced ? 'reduced' : 'normal';
  const chrome = spawn(args[1], [
    '--headless=new', '--no-first-run', '--no-default-browser-check', '--disable-extensions',
    '--disable-background-networking', '--force_low_power_gpu', '--host-resolver-rules=MAP * ~NOTFOUND',
    '--window-size=1366,900', `--user-data-dir=${join(output, 'profile-'+mode)}`,
    '--remote-debugging-port=0', '--remote-debugging-address=127.0.0.1', 'about:blank',
  ], {windowsHide:true, stdio:['ignore','ignore','pipe']});
  let stderr = '', cdp;
  chrome.stderr.on('data', chunk => { stderr += chunk; });
  try {
    const websocketUrl = await new Promise((resolve, reject) => {
      const guard = setTimeout(() => reject(new Error('Chrome debugging endpoint did not start')), 30_000);
      chrome.once('error', error => { clearTimeout(guard); reject(error); });
      chrome.once('exit', code => { clearTimeout(guard); reject(new Error('Chrome exited '+code)); });
      chrome.stderr.on('data', () => {
        const url = stderr.match(/DevTools listening on (ws:\/\/\S+)/)?.[1];
        if (url) { clearTimeout(guard); resolve(url); }
      });
    });
    cdp = await connect(websocketUrl);
    const {targetId} = await cdp.send('Target.createTarget', {url:pathToFileURL(html).href});
    const {sessionId} = await cdp.send('Target.attachToTarget', {targetId, flatten:true});
    await cdp.send('Target.activateTarget', {targetId});
    const evaluate = async expression => {
      const response = await cdp.send('Runtime.evaluate', {expression, returnByValue:true, awaitPromise:true}, sessionId);
      if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
      return response.result.value;
    };
    await cdp.send('Emulation.setEmulatedMedia', {features:[{name:'prefers-reduced-motion',value:reduced?'reduce':'no-preference'}]}, sessionId);
    await cdp.send('Page.enable', {}, sessionId);
    await evaluate('new Promise(resolve => { if (document.readyState === "complete") resolve(); else addEventListener("load", resolve, {once:true}); })');
    const result = await evaluate('window.checkActivity()');
    const {targetId:background} = await cdp.send('Target.createTarget', {url:'about:blank'});
    await cdp.send('Target.activateTarget', {targetId:background});
    const backgroundAnimations = await evaluate('window.waitForActivity(() => document.documentElement.dataset.activityDocumentVisible === "false").then(() => document.getAnimations().map(a => ({type:a.constructor.name,name:a.animationName,property:a.transitionProperty,state:a.playState})))');
    result.backgroundAnimations = backgroundAnimations;
    if (backgroundAnimations.some(a => a.type === 'CSSAnimation' && a.state === 'running')) result.failures.push({reason:'Background activity still runs'});
    await cdp.send('Target.activateTarget', {targetId});
    const foregroundStates = await evaluate('window.waitForActivity(() => document.documentElement.dataset.activityDocumentVisible === "true").then(() => document.getAnimations().filter(a => a instanceof CSSAnimation).map(a => ({state:a.playState,start:a.startTime})))');
    result.foregroundStates = foregroundStates;
    if (!reduced && (foregroundStates.length !== 42 || foregroundStates.some(a => a.state !== 'running' || a.start !== 0))) {
      result.failures.push({reason:'Visible activity fails to resume in phase'});
    }
    const screenshot = await cdp.send('Page.captureScreenshot', {format:'png'}, sessionId);
    writeFileSync(join(output, mode+'.png'), Buffer.from(screenshot.data, 'base64'));
    await evaluate('window.stopActivityVisibility()');
    if ((await evaluate('document.getAnimations().filter(a => a instanceof CSSAnimation).some(a => a.playState === "running")'))) {
      result.failures.push({reason:'Visibility cleanup leaves motion running'});
    }
  if (result.observations.length !== 48 || result.observations.some(o => o.reduced !== reduced)) {
    result.failures.push({reason:'Browser did not exercise the requested scenarios/motion preference'});
  }
  writeFileSync(join(output, mode+'.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify({output, mode, observations:result.observations.length, failures:result.failures}));
  if (result.failures.length) process.exitCode = 1;
  } finally {
    if (cdp) {
      await cdp.send('Browser.close').catch(() => {});
      cdp.close();
    } else chrome.kill();
    writeFileSync(join(output, mode+'.stderr.log'), stderr);
  }
}
