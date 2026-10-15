// App wiring for the test-run rail badge and retained panel evidence.
// Uses live-state fixtures; does not launch tests or backend processes.
import { act, cleanup, screen, within } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import * as runsApi from "./test-runs-api";
import { makeTestRun, makeTestRunDetail } from "./test-runs-fixtures";
import type { TestRunSummary } from "./test-runs";
import {
  EventSourceMock, ResizeObserverMock, clickAndSettle,
  createScheduledAnimationFrameMocks, dispatchOpenedStateEvent, flushUiWork, latestEventSource,
  makeStateResponse, makeWorkspaceLayoutResponse, renderApp, stubScrollIntoView,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";

vi.mock("./MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function EditorMock() { return <div />; }),
}));
vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function EditorMock() { return <div />; }),
}));

const originalScrollTo = HTMLElement.prototype.scrollTo;

beforeEach(() => {
  const frames = createScheduledAnimationFrameMocks();
  vi.stubGlobal("requestAnimationFrame", frames.requestAnimationFrameMock);
  vi.stubGlobal("cancelAnimationFrame", frames.cancelAnimationFrameMock);
  vi.stubGlobal("EventSource", EventSourceMock);
  vi.stubGlobal("ResizeObserver", ResizeObserverMock);
  HTMLElement.prototype.scrollTo = vi.fn();
  EventSourceMock.instances = [];
  stubScrollIntoView();
  vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
  vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
  vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(makeWorkspaceLayoutResponse());
});

afterEach(async () => {
  await act(async () => { cleanup(); await flushUiWork(); });
  HTMLElement.prototype.scrollTo = originalScrollTo;
  window.localStorage.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function renderRuns(testRuns: TestRunSummary[]) {
  const state = {
    ...makeStateResponse({ revision: 1, projects: [], sessions: [], orchestrators: [], workspaces: [] }),
    testRuns,
  };
  vi.spyOn(api, "fetchState").mockResolvedValue(state);
  await renderApp();
  // The app first waits for SSE; mocking the fallback HTTP fetch alone does
  // not deliver a snapshot before the fallback timer fires.
  await dispatchOpenedStateEvent(latestEventSource(), state);
}

function railButton() {
  return within(screen.getByRole("navigation", { name: "Control panel dock" }))
    .getByRole("button", { name: /^Open Test Runs/ });
}

describe("App test-run badge", () => {
  it("loads the run snapshot and preserves per-run restart detail in the panel", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const run = makeTestRun({ restartWouldVoidEvidence: true });
      vi.spyOn(runsApi, "readTestRun").mockResolvedValue({ run, detail: makeTestRunDetail() });
      await renderRuns([run]);
      await clickAndSettle(railButton());
      const row = within(screen.getByLabelText("Discovered runs"))
        .getByRole("button", { name: /test-one/ });
      expect(row).toHaveTextContent("running");
      expect(row).toHaveTextContent("Evidence impact on restart: would void evidence");
      await clickAndSettle(row);
      expect(await screen.findByText(/Restart evidence impact: would void evidence/)).toBeInTheDocument();
    });
  });

  it("removes the global header chip even while evidence is at risk", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      await renderRuns([makeTestRun({ restartWouldVoidEvidence: true })]);
      expect(screen.queryByRole("button", { name: /^Tests:/ })).not.toBeInTheDocument();
      expect(document.querySelector(".test-runs-global-status")).toBeNull();
    });
  });

  it.each([0, 1, 3])("shows only the %i active runs on the flask icon", async count => {
    await withVerifiedNoReactActWarnings(async () => {
      await renderRuns([
        ...Array.from({ length: count }, (_, index) => makeTestRun({
          runId: `active-${index}`, projectId: `project-${index}`,
        })),
        makeTestRun({ runId: "finished", state: "passed", restartWouldVoidEvidence: true }),
        makeTestRun({ runId: "failed", state: "failed" }),
        makeTestRun({ runId: "unknown", state: "unknown", restartWouldVoidEvidence: null }),
      ]);
      const button = railButton();
      expect(button.querySelector("svg")).not.toBeNull();
      if (count) {
        const label = `Open Test Runs (${count} active ${count === 1 ? "run" : "runs"})`;
        expect(button).toHaveAccessibleName(label);
        expect(button).toHaveAttribute("title", label);
        expect(within(button).getByText(String(count))).toBeVisible();
      } else {
        expect(button).toHaveAccessibleName("Open Test Runs");
        expect(button.textContent?.trim()).toBe("");
        expect(button).not.toHaveAttribute("data-evidence-warning", "true");
      }
    });
  });

  it("removes the badge when the last run finishes while retaining panel evidence", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const run = makeTestRun({ projectId: "another-project", restartWouldVoidEvidence: true });
      const finished = { ...run, state: "passed" as const };
      vi.spyOn(runsApi, "readTestRun").mockResolvedValue({ run: finished, detail: makeTestRunDetail() });
      await renderRuns([run]);
      expect(within(railButton()).getByText("1")).toBeVisible();
      await act(async () => {
        latestEventSource().dispatchNamedEvent("delta", {
          type: "testRunChanged", serverInstanceId: "test-instance", revision: 2, run: finished,
        });
        await flushUiWork();
      });
      expect(railButton()).toHaveAccessibleName("Open Test Runs");
      expect(railButton().textContent?.trim()).toBe("");
      await clickAndSettle(railButton());
      const list = screen.getByLabelText("Discovered runs");
      const row = within(list).getByRole("button", { name: /test-one/ });
      expect(row).toHaveTextContent("passed");
      expect(row).toHaveTextContent("Evidence impact on restart: would void evidence");
      await clickAndSettle(row);
      expect(await screen.findByText(/Restart evidence impact: would void evidence/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /^Tests:/ })).not.toBeInTheDocument();
    });
  });
});
