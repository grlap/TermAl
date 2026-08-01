import os from "node:os";
import { performance } from "node:perf_hooks";

export const VITEST_PREFLIGHT_CPU_SAMPLE_MS = 100;
export const VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO = 3;

export function assessVitestResourceSample(
  sample,
  maxWallCpuRatio = VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO,
) {
  const wallMs = Number(sample.wallMs);
  const cpuMs = Number(sample.cpuMs);
  const ratio = wallMs / cpuMs;
  return {
    ...sample,
    wallMs,
    cpuMs,
    ratio,
    availableCpuPercent: Math.min(100, (cpuMs / wallMs) * 100),
    passes:
      Number.isFinite(ratio) &&
      wallMs > 0 &&
      cpuMs > 0 &&
      ratio <= maxWallCpuRatio,
  };
}

export function measureVitestResourceSample({
  targetCpuMs = VITEST_PREFLIGHT_CPU_SAMPLE_MS,
} = {}) {
  const targetCpuMicros = targetCpuMs * 1_000;
  const wallStartedAt = performance.now();
  const cpuStartedAt = process.cpuUsage();
  let accumulator = 0;
  let cpu = process.cpuUsage(cpuStartedAt);

  while (cpu.user + cpu.system < targetCpuMicros) {
    for (let index = 0; index < 100_000; index += 1) {
      accumulator = Math.imul(accumulator + index + 1, 1_664_525);
    }
    cpu = process.cpuUsage(cpuStartedAt);
  }

  return {
    wallMs: performance.now() - wallStartedAt,
    cpuMs: (cpu.user + cpu.system) / 1_000,
    logicalCpuCount: os.cpus().length,
    loadAverage1m: os.loadavg()[0],
    accumulator,
  };
}

export function formatVitestResourceFailure(assessment) {
  return [
    "Vitest resource preflight failed before running tests.",
    `The process received ${assessment.availableCpuPercent.toFixed(1)}% CPU during the sample ` +
      `(${assessment.wallMs.toFixed(1)}ms wall / ${assessment.cpuMs.toFixed(1)}ms CPU; ` +
      `ratio ${assessment.ratio.toFixed(2)} > ${VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO.toFixed(2)}).`,
    `System load is ${assessment.loadAverage1m.toFixed(1)} across ` +
      `${assessment.logicalCpuCount} logical CPUs.`,
    "Pause or reduce VM, agent, build, and test workloads, then rerun the gate.",
    "The unchanged 10-second test timeout is not expanded and tests were not retried.",
  ].join("\n");
}

export function assertVitestResourceBudget({ sample } = {}) {
  const assessment = assessVitestResourceSample(
    sample ?? measureVitestResourceSample(),
  );
  if (!assessment.passes) {
    throw new Error(formatVitestResourceFailure(assessment));
  }
  return assessment;
}
