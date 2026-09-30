// Cooperating, bounded-resource fixture. IPC readiness includes every descendant
// before the supervisor captures process handles. No timing sleeps prove exit.
const { fork } = require('node:child_process');
const mode = process.argv[2] || 'root';
const independentLeaf = mode.endsWith('-independent');
const chainMode = independentLeaf ? mode.slice(0, -'-independent'.length) : mode;
if (mode === 'parity') {
  process.stdout.write(JSON.stringify({
    args: process.argv.slice(3), cwd: process.cwd(),
    value: process.env.TERMAL_NATIVE_VALUE,
    removed: process.env.TERMAL_NATIVE_REMOVED ?? null,
    unicode: process.env.TERMAL_NATIVE_UNICODE,
    pathKey: Object.keys(process.env).find(key => key.toUpperCase() === 'PATH'),
  }));
  process.stdout.write(Buffer.from([0, 255, 13, 10]));
  process.stderr.write(Buffer.from([255, 0, 10]));
  process.exitCode = 37;
} else if (mode === 'hold') {
  setInterval(() => {}, 60_000);
  const fs = require('node:fs');
  fs.writeFileSync(process.argv[3] + '.partial', JSON.stringify({ pids: [process.pid] }));
  fs.renameSync(process.argv[3] + '.partial', process.argv[3]);
} else {
  setInterval(() => {}, 60_000);
  if (chainMode === 'leaf') {
    if (independentLeaf) {
      // A post-parent-exit receipt, not a timing window, proves this leaf
      // survived the same IPC disconnect as the default fixture.
      process.once('disconnect', () => {
        const fs = require('node:fs');
        const marker = process.argv[3] + '.leaf-survived';
        fs.writeFileSync(marker + '.partial', JSON.stringify({ pids: [process.pid] }));
        fs.renameSync(marker + '.partial', marker);
      });
    }
    process.send({ pids: [process.pid] });
  }
  else {
    const childMode = chainMode === 'root' ? 'middle' : 'leaf';
    const childArgs = [childMode + (independentLeaf ? '-independent' : '')];
    if (independentLeaf) childArgs.push(process.argv[3]);
    const child = fork(__filename, childArgs, {
      stdio: ['ignore', 'inherit', 'inherit', 'ipc'],
      // On Windows libuv assigns ordinary children to a private parent-owned
      // kill-on-close job. This test mode skips that extra job for the leaf;
      // it still inherits the native host job, as the Rust test proves.
      detached: independentLeaf && chainMode === 'middle',
    });
    child.once('message', ({ pids }) => {
      const ready = { pids: [process.pid, ...pids] };
      if (process.send) process.send(ready);
      else {
        process.stdout.write(JSON.stringify(ready) + '\n');
        if (process.argv[3]) {
          const fs = require('node:fs');
          fs.writeFileSync(process.argv[3] + '.partial', JSON.stringify(ready));
          fs.renameSync(process.argv[3] + '.partial', process.argv[3]);
        }
      }
    });
    child.on('error', error => { console.error(error); process.exit(1); });
  }
}
