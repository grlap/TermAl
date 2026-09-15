// Owns the production wiring from the persistent left control-panel action
// to the singleton Work workspace tab (no Work dock section remains).

import { act, cleanup, screen } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import {
  EventSourceMock,
  ResizeObserverMock,
  clickAndSettle,
  createScheduledAnimationFrameMocks,
  flushUiWork,
  latestEventSource,
  makeSession,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  renderApp,
  settleAsyncUi,
  stubScrollIntoView,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";
import { readProjectWork } from "./work-visualizer-api";

vi.mock("./MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function MonacoDiffEditorMock() {
    return <div data-testid="monaco-diff-mock" />;
  }),
}));

vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function MonacoCodeEditorMock() {
    return <div data-testid="monaco-code-mock" />;
  }),
}));

vi.mock("./work-visualizer-api", () => ({
  readProjectWork: vi.fn(),
  readWorkDetail: vi.fn(),
  readWorkBeadsDetail: vi.fn(),
}));

const originalScrollTo = HTMLElement.prototype.scrollTo;

describe("App Work launcher", () => {
  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createScheduledAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    vi.stubGlobal(
      "EventSource",
      EventSourceMock as unknown as typeof EventSource,
    );
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverMock as unknown as typeof ResizeObserver,
    );
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    stubScrollIntoView();
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(
      makeWorkspaceLayoutResponse(),
    );
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    window.localStorage.clear();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("opens the Work tab from the left panel scoped to the focused project", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      vi.spyOn(api, "fetchState").mockResolvedValue(
        makeStateResponse({
          revision: 1,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          orchestrators: [],
          sessions: [
            makeSession("session-source", {
              name: "Source session",
              projectId: "project-termal",
              workdir: "/projects/termal",
            }),
          ],
          workspaces: [],
        }),
      );
      vi.mocked(readProjectWork).mockResolvedValue({
        sources: [
          { source: "engram", state: "absent", message: "No .engram-project file" },
          { source: "beads", state: "ready", message: "Beads reads use the native bd binary" },
        ],
        readerId: null,
        observedAt: "now",
        page: null,
        beads: {
          items: [
            {
              id: "tm-goal", shortRef: "tm-goal", title: "Goal", kind: "task", lifecycle: "open",
              availability: "ready", priority: 2, labels: [], assignedTo: null, parentId: null,
              updatedAt: "today", blockedBy: [], source: "beads", prerequisites: [],
            },
          ],
          total: 1, shownBefore: 0, more: false, after: null, hint: null,
        },
      });

      await renderApp();
      // Projects arrive with the state resync after the live stream settles.
      act(() => {
        latestEventSource().dispatchError();
      });
      await settleAsyncUi();
      expect(screen.queryByRole("button", { name: "Work" })).not.toBeInTheDocument();
      await clickAndSettle(screen.getByRole("button", { name: "Open Work" }));

      expect(
        await screen.findByRole("combobox", { name: "Work project" }),
      ).toHaveValue("project-termal");
      expect(
        await screen.findByRole("button", { name: "tm-goal — Goal" }),
      ).toBeInTheDocument();
      expect(readProjectWork).toHaveBeenCalledWith(
        "project-termal",
        { search: "", label: "", availability: "" },
        expect.any(AbortSignal),
        undefined,
      );
    });
  });
});
