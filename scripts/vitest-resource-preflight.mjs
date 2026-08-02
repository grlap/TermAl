// Vitest resource-preflight primitives. Importing this module is side-effect
// free: callers explicitly decide when to measure CPU availability or assert
// the budget, so production Vite builds can load shared config safely.
import os from "node:os";
import { performance } from "node:perf_hooks";

export const VITEST_PREFLIGHT_CPU_SAMPLE_MS = 100;
export const VITEST_PREFLIGHT_SAMPLE_COUNT = 3;
export const VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO = 3;

export function assessVitestResourceSample(
  sample,
  maxWallCpuRatio = VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO,
) {
  const wallMs = Number(sample.wallMs);
  const cpuMs = Number(sample.cpuMs);
  const validMeasurement = wallMs > 0 && cpuMs > 0;
  const ratio = validMeasurement ? wallMs / cpuMs : Number.POSITIVE_INFINITY;
  return {
    ...sample,
    wallMs,
    cpuMs,
    ratio,
    maxWallCpuRatio,
    availableCpuPercent: validMeasurement
      ? Math.min(100, (cpuMs / wallMs) * 100)
      : 0,
    passes:
      Number.isFinite(ratio) &&
      Number.isFinite(maxWallCpuRatio) &&
      maxWallCpuRatio > 0 &&
      ratio <= maxWallCpuRatio,
  };
}

export function assessVitestResourceSamples(
  samples,
  maxWallCpuRatio = VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO,
) {
  assertPositiveOddSampleCount(samples?.length);

  const assessments = samples.map((sample) =>
    assessVitestResourceSample(sample, maxWallCpuRatio),
  );
  const byRatio = [...assessments].sort((left, right) => {
    const leftFinite = Number.isFinite(left.ratio);
    const rightFinite = Number.isFinite(right.ratio);
    if (leftFinite !== rightFinite) {
      return leftFinite ? -1 : 1;
    }
    if (!leftFinite) {
      return 0;
    }
    return left.ratio - right.ratio;
  });
  const representative = byRatio[Math.floor(byRatio.length / 2)];

  return {
    ...representative,
    sampleCount: assessments.length,
    samples: assessments,
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
    loadAverageSupported: process.platform !== "win32",
    accumulator,
  };
}

export function measureVitestResourceSamples({
  sampleCount = VITEST_PREFLIGHT_SAMPLE_COUNT,
  targetCpuMs = VITEST_PREFLIGHT_CPU_SAMPLE_MS,
} = {}) {
  assertPositiveOddSampleCount(sampleCount);

  return Array.from({ length: sampleCount }, () =>
    measureVitestResourceSample({ targetCpuMs }),
  );
}

function assertPositiveOddSampleCount(sampleCount) {
  if (
    !Number.isInteger(sampleCount) ||
    sampleCount <= 0 ||
    sampleCount % 2 === 0
  ) {
    throw new TypeError(
      "Vitest resource preflight sampleCount must be a positive odd integer.",
    );
  }
}

export function formatVitestResourceFailure(assessment) {
  const loadDiagnostic = assessment.loadAverageSupported
    ? `System load is ${assessment.loadAverage1m.toFixed(1)} across ` +
      `${assessment.logicalCpuCount} logical CPUs.`
    : "System load average is unavailable on Windows; " +
      "the process CPU measurement above is authoritative.";
  const sampleRatios = assessment.samples
    .map((sample) => sample.ratio.toFixed(2))
    .join(", ");

  return [
    "Vitest resource preflight failed before running tests.",
    `The process received ${assessment.availableCpuPercent.toFixed(1)}% CPU during the median of ` +
      `${assessment.sampleCount} fixed samples ` +
      `(${assessment.wallMs.toFixed(1)}ms wall / ${assessment.cpuMs.toFixed(1)}ms CPU; ` +
      `median ratio ${assessment.ratio.toFixed(2)} > ${assessment.maxWallCpuRatio.toFixed(2)}; ` +
      `sample ratios: ${sampleRatios}).`,
    loadDiagnostic,
    "Pause or reduce VM, agent, build, and test workloads, then rerun the gate.",
    "The unchanged 10-second test timeout is not expanded and tests were not retried.",
  ].join("\n");
}

export function assertVitestResourceBudget({
  samples,
  maxWallCpuRatio = VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO,
} = {}) {
  const assessment = assessVitestResourceSamples(
    samples ?? measureVitestResourceSamples(),
    maxWallCpuRatio,
  );
  if (!assessment.passes) {
    throw new Error(formatVitestResourceFailure(assessment));
  }
  return assessment;
}
