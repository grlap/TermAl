import assert from "node:assert/strict";
import test from "node:test";

import {
  VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO,
  assessVitestResourceSample,
  assessVitestResourceSamples,
  assertVitestResourceBudget,
  measureVitestResourceSamples,
} from "./vitest-resource-preflight.mjs";

const systemSample = {
  logicalCpuCount: 8,
  loadAverage1m: 12,
  loadAverageSupported: true,
  accumulator: 1,
};

test("resource preflight accepts the median boundary despite one scheduler spike", () => {
  const assessment = assessVitestResourceSamples([
    { ...systemSample, wallMs: 200, cpuMs: 100 },
    {
      ...systemSample,
      wallMs: VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO * 100,
      cpuMs: 100,
    },
    { ...systemSample, wallMs: 900, cpuMs: 100 },
  ]);

  assert.equal(assessment.passes, true);
  assert.equal(assessment.ratio, VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO);
  assert.equal(assessment.sampleCount, 3);
});

test("resource preflight rejects a starved process with actionable metrics", () => {
  assert.throws(
    () =>
      assertVitestResourceBudget({
        samples: [
          { ...systemSample, wallMs: 500, cpuMs: 100 },
          { ...systemSample, wallMs: 550, cpuMs: 100 },
          { ...systemSample, wallMs: 600, cpuMs: 100 },
        ],
      }),
    (error) => {
      assert.match(error.message, /failed before running tests/);
      assert.match(error.message, /18\.2% CPU/);
      assert.match(error.message, /median of 3 fixed samples/);
      assert.match(error.message, /median ratio 5\.50 > 3\.00/);
      assert.match(error.message, /sample ratios: 5\.00, 5\.50, 6\.00/);
      assert.match(error.message, /System load is 12\.0 across 8 logical CPUs/);
      assert.match(error.message, /not expanded and tests were not retried/);
      return true;
    },
  );
});

test("resource preflight reports the effective custom threshold", () => {
  assert.throws(
    () =>
      assertVitestResourceBudget({
        samples: [
          { ...systemSample, wallMs: 500, cpuMs: 100 },
          { ...systemSample, wallMs: 550, cpuMs: 100 },
          { ...systemSample, wallMs: 600, cpuMs: 100 },
        ],
        maxWallCpuRatio: 4.25,
      }),
    /median ratio 5\.50 > 4\.25/,
  );
});

test("resource preflight explains that load average is unavailable on Windows", () => {
  const windowsSample = {
    ...systemSample,
    loadAverage1m: 0,
    loadAverageSupported: false,
  };

  assert.throws(
    () =>
      assertVitestResourceBudget({
        samples: [
          { ...windowsSample, wallMs: 500, cpuMs: 100 },
          { ...windowsSample, wallMs: 550, cpuMs: 100 },
          { ...windowsSample, wallMs: 600, cpuMs: 100 },
        ],
      }),
    (error) => {
      assert.match(error.message, /load average is unavailable on Windows/);
      assert.doesNotMatch(error.message, /System load is 0\.0/);
      return true;
    },
  );
});

test("resource preflight rejects invalid measurements", () => {
  const assessment = assessVitestResourceSample({
    ...systemSample,
    wallMs: 0,
    cpuMs: 0,
  });

  assert.equal(assessment.passes, false);
});

test("resource preflight rejects empty and even-sized sample sets", () => {
  assert.throws(
    () => assessVitestResourceSamples([]),
    /positive odd integer/,
  );
  assert.throws(
    () => assessVitestResourceSamples([systemSample, systemSample]),
    /positive odd integer/,
  );
  assert.throws(
    () =>
      assertVitestResourceBudget({
        samples: [systemSample, systemSample],
      }),
    /positive odd integer/,
  );
  assert.throws(
    () => measureVitestResourceSamples({ sampleCount: 2 }),
    /positive odd integer/,
  );
});

test("resource preflight rejects a majority of invalid samples deterministically", () => {
  const assessment = assessVitestResourceSamples([
    { ...systemSample, wallMs: 100, cpuMs: 100 },
    { ...systemSample, wallMs: 0, cpuMs: 100 },
    { ...systemSample, wallMs: 100, cpuMs: 0 },
  ]);

  assert.equal(assessment.passes, false);
  assert.equal(assessment.ratio, Number.POSITIVE_INFINITY);
});
