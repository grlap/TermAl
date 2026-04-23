const fs = require("fs");

const OUT_PATH = ".tmp/termal-first-key-after-tab-switch.json";
const SETTLE_MS = 900;

function round(value, digits = 3) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return value;
  }
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

function pickMetrics(metricMap) {
  const keys = [
    "TaskDuration",
    "ScriptDuration",
    "LayoutDuration",
    "RecalcStyleDuration",
    "LayoutCount",
    "RecalcStyleCount",
    "JSHeapUsedSize",
    "JSHeapTotalSize",
    "Nodes",
    "Documents",
  ];
  return Object.fromEntries(
    keys.filter((key) => key in metricMap).map((key) => [key, metricMap[key]]),
  );
}

function diffMetrics(before, after) {
  const delta = {};
  for (const key of Object.keys({ ...before, ...after })) {
    if (typeof before[key] === "number" && typeof after[key] === "number") {
      delta[key] = round(after[key] - before[key]);
    }
  }
  return delta;
}

function topFrames(profile) {
  const nodes = new Map((profile.nodes || []).map((node) => [node.id, node]));
  const totals = new Map();
  const samples = profile.samples || [];
  const deltas = profile.timeDeltas || [];
  for (let index = 0; index < samples.length; index += 1) {
    const node = nodes.get(samples[index]);
    if (!node) continue;
    const frame = node.callFrame || {};
    const functionName = frame.functionName || "(anonymous)";
    const url = frame.url || "(no url)";
    const line = typeof frame.lineNumber === "number" ? frame.lineNumber + 1 : null;
    const column =
      typeof frame.columnNumber === "number" ? frame.columnNumber + 1 : null;
    const key = `${functionName}@@${url}@@${line ?? ""}@@${column ?? ""}`;
    const current = totals.get(key) || {
      functionName,
      url,
      line,
      column,
      selfMs: 0,
    };
    current.selfMs += (deltas[index] || 0) / 1000;
    totals.set(key, current);
  }
  return [...totals.values()]
    .sort((first, second) => second.selfMs - first.selfMs)
    .map((entry) => ({ ...entry, selfMs: round(entry.selfMs, 1) }));
}

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function main() {
  const targets = await fetch("http://127.0.0.1:9222/json/list").then((response) =>
    response.json(),
  );
  const page = targets.find(
    (target) => target.type === "page" && target.url.includes("127.0.0.1:4173"),
  );
  if (!page) {
    throw new Error("No live TermAl page found.");
  }

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  let nextId = 0;
  const pending = new Map();

  function send(method, params = {}) {
    const id = ++nextId;
    ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject, method });
    });
  }

  function rejectAll(error) {
    for (const { reject } of pending.values()) {
      reject(error);
    }
    pending.clear();
  }

  ws.addEventListener("message", (event) => {
    const payload = JSON.parse(event.data);
    if (typeof payload.id !== "number") {
      return;
    }
    const resolver = pending.get(payload.id);
    if (!resolver) {
      return;
    }
    pending.delete(payload.id);
    if (payload.error) {
      resolver.reject(new Error(`${resolver.method}: ${payload.error.message}`));
    } else {
      resolver.resolve(payload.result);
    }
  });

  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve, { once: true });
    ws.addEventListener(
      "error",
      () => reject(new Error("Failed to connect to the DevTools websocket.")),
      { once: true },
    );
  });

  try {
    await send("Page.bringToFront");
    await send("Runtime.enable");
    await send("Performance.enable");
    await send("Profiler.enable");
    await send("Profiler.setSamplingInterval", { interval: 1000 });

    const tabInfoResult = await send("Runtime.evaluate", {
      expression: `(() => {
        const activePane = document.querySelector('.workspace-pane.active');
        const tablist = activePane?.querySelector('[role="tablist"][aria-label="Tile tabs"]');
        const tabs = [...(tablist?.querySelectorAll('[role="tab"]') ?? [])]
          .map((tab, index) => {
            const rect = tab.getBoundingClientRect();
            return {
              index,
              label: (tab.textContent ?? '').trim(),
              selected: tab.getAttribute('aria-selected') === 'true',
              rect: {
                left: rect.left,
                top: rect.top,
                width: rect.width,
                height: rect.height,
                right: rect.right,
                bottom: rect.bottom,
              },
            };
          })
          .filter((tab) => tab.rect.width > 0 && tab.rect.height > 0);
        return {
          tabCount: tabs.length,
          tabs,
        };
      })()`,
      returnByValue: true,
    });
    const tabInfo = tabInfoResult.result?.value;
    if (!tabInfo || tabInfo.tabCount < 2) {
      throw new Error("Need at least two visible session tabs to profile the switch path.");
    }

    const targetTab =
      tabInfo.tabs.find((tab) => !tab.selected && /codex/i.test(tab.label)) ??
      tabInfo.tabs.find((tab) => !tab.selected) ??
      tabInfo.tabs[0];
    if (!targetTab) {
      throw new Error("Could not find a target tab to switch to.");
    }

    const beforeMetrics = Object.fromEntries(
      ((await send("Performance.getMetrics")).metrics || []).map((metric) => [
        metric.name,
        metric.value,
      ]),
    );

    await send("Profiler.start");
    await send("Runtime.evaluate", {
      expression: "window.__termalFirstKeySwitchStartedAt = performance.now(); true;",
      returnByValue: true,
    });

    const targetX = targetTab.rect.left + targetTab.rect.width / 2;
    const targetY = targetTab.rect.top + targetTab.rect.height / 2;
    await send("Input.dispatchMouseEvent", {
      type: "mousePressed",
      x: targetX,
      y: targetY,
      button: "left",
      clickCount: 1,
    });
    await send("Input.dispatchMouseEvent", {
      type: "mouseReleased",
      x: targetX,
      y: targetY,
      button: "left",
      clickCount: 1,
    });

    await wait(180);

    const setupResult = await send("Runtime.evaluate", {
      expression: `(() => {
        if (window.__termalFirstKeyProfile?.cleanup) {
          window.__termalFirstKeyProfile.cleanup();
        }
        const activePane = document.querySelector('.workspace-pane.active');
        const textareas = [...(activePane?.querySelectorAll('textarea') ?? [])]
          .filter((textarea) => {
            const rect = textarea.getBoundingClientRect();
            const style = getComputedStyle(textarea);
            return (
              rect.width > 0 &&
              rect.height > 0 &&
              style.display !== 'none' &&
              style.visibility !== 'hidden' &&
              !textarea.disabled &&
              !textarea.readOnly
            );
          })
          .sort((first, second) => {
            const firstRect = first.getBoundingClientRect();
            const secondRect = second.getBoundingClientRect();
            return secondRect.bottom - firstRect.bottom;
          });
        const target = textareas[0] ?? null;
        if (!target) {
          return { error: 'No visible editable textarea in the active pane.' };
        }
        const rect = target.getBoundingClientRect();
        const state = {
          originalValue: target.value,
          sample: null,
          target,
          textareaCount: textareas.length,
        };
        state.cleanup = () => {};
        target.focus();
        target.setSelectionRange(target.value.length, target.value.length);
        window.__termalFirstKeyProfile = state;
        return {
          textareaCount: textareas.length,
          targetRect: {
            left: rect.left,
            top: rect.top,
            width: rect.width,
            height: rect.height,
            bottom: rect.bottom,
          },
          originalValueLength: target.value.length,
        };
      })()`,
      returnByValue: true,
    });
    const setup = setupResult.result?.value;
    if (!setup || setup.error) {
      throw new Error(setup?.error || "Failed to prepare prompt profiling.");
    }

    await send("Input.insertText", { text: "x" });
    await send("Runtime.evaluate", {
      expression: `(async () => {
        const state = window.__termalFirstKeyProfile;
        if (!state || !state.target) {
          return false;
        }
        const inputTime = performance.now();
        const switchStartedAt = window.__termalFirstKeySwitchStartedAt ?? null;
        state.sample = await new Promise((resolve) => {
          requestAnimationFrame((frameTime) => {
            resolve({
            inputTime,
            nextFrameDelay: frameTime - inputTime,
            sinceSwitchMs:
              switchStartedAt === null ? null : inputTime - switchStartedAt,
            valueLength: state.target.value.length,
            });
          });
        });
        return true;
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    await wait(SETTLE_MS);

    const profileResult = await send("Profiler.stop");
    const afterMetrics = Object.fromEntries(
      ((await send("Performance.getMetrics")).metrics || []).map((metric) => [
        metric.name,
        metric.value,
      ]),
    );

    const captureResult = await send("Runtime.evaluate", {
      expression: `(() => {
        const state = window.__termalFirstKeyProfile;
        if (!state) {
          return null;
        }
        return {
          sample: state.sample,
          originalValueLength: state.originalValue.length,
          currentValueLength: state.target.value.length,
        };
      })()`,
      returnByValue: true,
    });
    const capture = captureResult.result?.value;

    const restoreResult = await send("Runtime.evaluate", {
      expression: `(() => {
        const state = window.__termalFirstKeyProfile;
        if (!state || !state.target) {
          return false;
        }
        const descriptor = Object.getOwnPropertyDescriptor(
          HTMLTextAreaElement.prototype,
          'value',
        );
        descriptor?.set?.call(state.target, state.originalValue);
        state.target.dispatchEvent(new Event('input', { bubbles: true }));
        state.cleanup?.();
        delete window.__termalFirstKeyProfile;
        delete window.__termalFirstKeySwitchStartedAt;
        return true;
      })()`,
      returnByValue: true,
    });

    const allFrames = topFrames(profileResult.profile || {});
    const appFrames = allFrames.filter((entry) =>
      entry.url.includes("127.0.0.1:4173"),
    );

    const report = {
      sampledAt: new Date().toISOString(),
      page: {
        title: page.title,
        url: page.url,
      },
      targetTab,
      setup,
      capture,
      restoreSucceeded: restoreResult.result?.value ?? false,
      metricsBefore: pickMetrics(beforeMetrics),
      metricsAfter: pickMetrics(afterMetrics),
      metricsDelta: diffMetrics(beforeMetrics, afterMetrics),
      topFrames: allFrames.slice(0, 20),
      topAppFrames: appFrames.slice(0, 20),
    };

    fs.writeFileSync(OUT_PATH, JSON.stringify(report, null, 2));
    console.log(
      JSON.stringify(
        {
          outPath: OUT_PATH,
          targetTab: {
            label: targetTab.label,
            index: targetTab.index,
          },
          setup,
          capture,
          restoreSucceeded: report.restoreSucceeded,
          metricsDelta: report.metricsDelta,
          topAppFrames: report.topAppFrames.slice(0, 10),
        },
        null,
        2,
      ),
    );
  } finally {
    try {
      ws.close();
    } catch {}
    rejectAll(new Error("DevTools websocket closed."));
  }
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
});
