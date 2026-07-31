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
    vi.spyOn(api, "fetchResponseBoard").mockResolvedValue({ cards: [] });
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

  it("opens the real singleton board tab from the left panel", async () => {
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
          "Drop an agent response anywhere on the board.",
        ),
      ).toBeInTheDocument();
      expect(api.fetchResponseBoard).toHaveBeenCalledOnce();
    });
  });
});
