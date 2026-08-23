// Owns the production wiring from the persistent left control-panel action
// to the singleton Response Board workspace tab.

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
  makeSession,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  renderApp,
  stubScrollIntoView,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";

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

const originalScrollTo = HTMLElement.prototype.scrollTo;

describe("App Response Board launcher", () => {
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
    vi.spyOn(api, "fetchResponseBoardTabs").mockResolvedValue({
      stagedCardCount: 0,
      tabs: [
        {
          id: "response-board-default",
          name: "Board",
          kind: "custom",
          projectId: null,
          sortOrder: 0,
          createdAt: "2026-08-22T00:00:00Z",
          placedCardCount: 0,
        },
      ],
    });
    vi.spyOn(api, "fetchResponseBoardTab").mockResolvedValue({
      tab: {
        id: "response-board-default",
        name: "Board",
        kind: "custom",
        projectId: null,
        sortOrder: 0,
        createdAt: "2026-08-22T00:00:00Z",
        placedCardCount: 0,
      },
      cards: [],
      stagedCards: [],
    });
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

  it("opens the real partitioned board tab from the left panel", async () => {
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

      await renderApp();
      await clickAndSettle(
        screen.getByRole("button", { name: "Open Response Board" }),
      );

      expect(
        await screen.findByText(
          "Drag a staged response here, or drop a transcript message.",
        ),
      ).toBeInTheDocument();
      expect(api.fetchResponseBoardTabs).toHaveBeenCalled();
      expect(api.fetchResponseBoardTab).toHaveBeenCalledWith(
        "response-board-default",
      );
    });
  });
});
