const fs = require("fs");
const path = require("path");
const childProcess = require("child_process");

const ROOT = path.resolve(__dirname, "..", "..");
const NODE = process.execPath;

const SCENARIOS = [
  {
    name: "active-codex-typing",
    script: path.join(__dirname, "active-codex-typing.js"),
    reportPath: path.join(ROOT, ".tmp", "active-codex-typing-profile.json"),
    thresholds: {
      maxAverageNextFrameDelayMs: 20,
      maxWorstNextFrameDelayMs: 40,
      maxTaskDuration: 0.8,
      maxKeystrokesOver32Ms: 0,
    },
    evaluate(report) {
      const findings = [];
      const averageDelay = report.summary?.averageNextFrameDelayMs ?? null;
      const worstDelay = report.summary?.maxNextFrameDelayMs ?? null;
      const taskDuration = report.metricsDelta?.TaskDuration ?? null;
      const keystrokesOver32Ms = report.summary?.keystrokesOver32Ms ?? null;
      const hasActiveCodexState =
        report.setup?.bodyHasCodexWorking || report.setup?.bodyHasWaitingForChunk;

      if ((report.summary?.keystrokes ?? 0) === 0) {
        findings.push("no typing samples were captured");
      }
      if (!hasActiveCodexState) {
        findings.push("no active Codex waiting/working session was visible");
      }
      if (
        typeof averageDelay === "number" &&
        averageDelay > this.thresholds.maxAverageNextFrameDelayMs
      ) {
        findings.push(
          `average next-frame delay ${averageDelay}ms exceeds ${this.thresholds.maxAverageNextFrameDelayMs}ms`,
        );
      }
      if (
        typeof worstDelay === "number" &&
        worstDelay > this.thresholds.maxWorstNextFrameDelayMs
      ) {
        findings.push(
          `worst next-frame delay ${worstDelay}ms exceeds ${this.thresholds.maxWorstNextFrameDelayMs}ms`,
        );
      }
      if (
        typeof taskDuration === "number" &&
        taskDuration > this.thresholds.maxTaskDuration
      ) {
        findings.push(
          `TaskDuration delta ${taskDuration}s exceeds ${this.thresholds.maxTaskDuration}s`,
        );
      }
      if (
        typeof keystrokesOver32Ms === "number" &&
        keystrokesOver32Ms > this.thresholds.maxKeystrokesOver32Ms
      ) {
        findings.push(
          `keystrokes over 32ms (${keystrokesOver32Ms}) exceed ${this.thresholds.maxKeystrokesOver32Ms}`,
        );
      }

      return findings;
    },
  },
  {
    name: "first-key-after-tab-switch",
    script: path.join(__dirname, "first-key-after-tab-switch.js"),
    reportPath: path.join(ROOT, ".tmp", "termal-first-key-after-tab-switch.json"),
    thresholds: {
      maxFirstKeyNextFrameDelayMs: 24,
      maxTaskDuration: 0.9,
    },
    evaluate(report) {
      const findings = [];
      const firstKeyDelay = report.capture?.sample?.nextFrameDelay ?? null;
      const taskDuration = report.metricsDelta?.TaskDuration ?? null;

      if (typeof firstKeyDelay !== "number") {
        findings.push("no first-key sample was captured");
      }
      if (
        typeof firstKeyDelay === "number" &&
        firstKeyDelay > this.thresholds.maxFirstKeyNextFrameDelayMs
      ) {
        findings.push(
          `first-key next-frame delay ${firstKeyDelay}ms exceeds ${this.thresholds.maxFirstKeyNextFrameDelayMs}ms`,
        );
      }
      if (
        typeof taskDuration === "number" &&
        taskDuration > this.thresholds.maxTaskDuration
      ) {
        findings.push(
          `TaskDuration delta ${taskDuration}s exceeds ${this.thresholds.maxTaskDuration}s`,
        );
      }

      return findings;
    },
  },
];

function runScenario(scenario) {
  const result = childProcess.spawnSync(NODE, [scenario.script], {
    cwd: ROOT,
    encoding: "utf8",
    stdio: "pipe",
  });
  if (result.status !== 0) {
    throw new Error(
      [
        `${scenario.name} failed with exit code ${result.status ?? "unknown"}.`,
        result.stdout?.trim(),
        result.stderr?.trim(),
      ]
        .filter(Boolean)
        .join("\n\n"),
    );
  }

  const report = JSON.parse(fs.readFileSync(scenario.reportPath, "utf8"));
  const findings = scenario.evaluate(report);
  return {
    name: scenario.name,
    findings,
    reportPath: path.relative(ROOT, scenario.reportPath),
    summary:
      scenario.name === "active-codex-typing"
        ? {
            averageNextFrameDelayMs: report.summary?.averageNextFrameDelayMs ?? null,
            maxNextFrameDelayMs: report.summary?.maxNextFrameDelayMs ?? null,
            taskDuration: report.metricsDelta?.TaskDuration ?? null,
            keystrokesOver32Ms: report.summary?.keystrokesOver32Ms ?? null,
          }
        : {
            firstKeyNextFrameDelayMs:
              report.capture?.sample?.nextFrameDelay ?? null,
            taskDuration: report.metricsDelta?.TaskDuration ?? null,
          },
  };
}

function main() {
  const results = SCENARIOS.map(runScenario);
  const failingResults = results.filter((result) => result.findings.length > 0);

  console.log(
    JSON.stringify(
      {
        sampledAt: new Date().toISOString(),
        results,
      },
      null,
      2,
    ),
  );

  if (failingResults.length > 0) {
    const messages = failingResults.flatMap((result) =>
      result.findings.map((finding) => `${result.name}: ${finding}`),
    );
    throw new Error(messages.join("\n"));
  }
}

try {
  main();
} catch (error) {
  console.error(error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
}
