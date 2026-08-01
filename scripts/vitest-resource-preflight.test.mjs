import assert from "node:assert/strict";
import test from "node:test";

import {
  VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO,
  assessVitestResourceSample,
  assertVitestResourceBudget,
} from "./vitest-resource-preflight.mjs";

const systemSample = {
  logicalCpuCount: 8,
  loadAverage1m: 12,
  accumulator: 1,
};

test("resource preflight accepts the wall-to-CPU boundary", () => {
  const assessment = assessVitestResourceSample({
    ...systemSample,
    wallMs: VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO * 100,
    cpuMs: 100,
  });

  assert.equal(assessment.passes, true);
  assert.equal(assessment.ratio, VITEST_PREFLIGHT_MAX_WALL_CPU_RATIO);
});

test("resource preflight rejects a starved process with actionable metrics", () => {
  assert.throws(
    () =>
      assertVitestResourceBudget({
        sample: {
          ...systemSample,
          wallMs: 550,
          cpuMs: 100,
        },
      }),
    (error) => {
      assert.match(error.message, /failed before running tests/);
      assert.match(error.message, /18\.2% CPU/);
      assert.match(error.message, /ratio 5\.50 > 3\.00/);
      assert.match(error.message, /System load is 12\.0 across 8 logical CPUs/);
      assert.match(error.message, /not expanded and tests were not retried/);
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
