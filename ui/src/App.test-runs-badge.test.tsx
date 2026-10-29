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
  makeSession, makeStateResponse, makeStateSessionSummary, makeWorkspaceLayoutResponse, renderApp, stubScrollIntoView,
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

  // No listed session owns these runs (their owner is not in the session
  // list), so nothing is working; the flask still counts the active run.
  it("keeps zero working sessions when no listed session owns the running detached test", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      await renderRuns([
        makeTestRun({ detached: true, restartWouldVoidEvidence: true }),
        makeTestRun({ runId: "finished", state: "passed" }),
        makeTestRun({ runId: "failed", state: "failed" }),
        makeTestRun({ runId: "unknown", state: "unknown" }),
      ]);
      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      expect(screen.getByRole("button", { name: "Working sessions (0)" })).toBeVisible();
      expect(railButton()).toHaveAccessibleName("Open Test Runs (1 active run)");
      expect(within(railButton()).getByText("1")).toBeVisible();
      expect(screen.queryByRole("button", { name: /^Tests:/ })).not.toBeInTheDocument();
      expect(document.querySelector(".test-runs-global-status")).toBeNull();
      await clickAndSettle(railButton());
      const row = within(screen.getByLabelText("Discovered runs"))
        .getByRole("button", { name: /test-one/ });
      expect(row).toHaveTextContent("running");
    });
  });

  // A session whose turn has ended but whose detached gate is still running is
  // busy in the background: Working, not Completed, with a row label and a
  // breakdown. A run-only delta (no session update) moves it back.
  it("counts a listed session busy only with a detached run as working, and follows run-only deltas", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const owner = makeSession("session-owner", { name: "Gate owner" });
      const run = makeTestRun({ detached: true });
      const state = {
        ...makeStateResponse({
          revision: 1, projects: [], sessions: [makeStateSessionSummary(owner)], orchestrators: [], workspaces: [],
        }),
        testRuns: [run],
      };
      vi.spyOn(api, "fetchState").mockResolvedValue(state);
      await renderApp();
      await dispatchOpenedStateEvent(latestEventSource(), state);
      await clickAndSettle(screen.getByRole("button", { name: "Sessions" }));
      expect(screen.getByRole("button", { name: "Working sessions (1)" })).toBeVisible();
      expect(screen.getByRole("button", { name: "Completed (0)" })).toBeVisible();
      expect(screen.getByText("0 foreground · 1 background")).toBeVisible();
      expect(screen.getByText("Background work")).toBeVisible();
      expect(document.querySelector('.session-row-status-badge[data-work="background"]')).not.toBeNull();
      expect(railButton()).toHaveAccessibleName("Open Test Runs (1 active run)");

      await act(async () => {
        latestEventSource().dispatchNamedEvent("delta", {
          type: "testRunChanged", serverInstanceId: "test-instance", revision: 2, run: { ...run, state: "passed" },
        });
        await flushUiWork();
      });
      expect(screen.getByRole("button", { name: "Working sessions (0)" })).toBeVisible();
      expect(screen.getByRole("button", { name: "Completed (1)" })).toBeVisible();
      expect(screen.queryByText("Background work")).toBeNull();
      expect(document.querySelector(".sidebar-status-breakdown")).toBeNull();
      expect(document.querySelector('.session-row-status-badge[data-work="background"]')).toBeNull();
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
