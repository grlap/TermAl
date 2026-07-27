// App.control-panel.board.test.tsx
//
// Owns: integration acceptance for the read-only coordination-board
// control-panel section (tm-uwx.7.3) — proves the mounted path end to end:
// explicit-project scoping (never the launcher-origin fallback), root-session
// selection, the All-projects no-fetch guidance, and the no-local-root
// guidance (surface review, mailbox #238-1/#238-3).
//
// Does not own: BoardPanel's fetch/pagination/race behavior
// (panels/BoardPanel.test.tsx) or dock order/migration
// (panels/ControlPanelSurface.test.tsx).
//
// New file alongside the App.control-panel.* split slices.
import { act, cleanup, screen } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import {
  EventSourceMock,
  ResizeObserverMock,
  clickAndSettle,
  createScheduledAnimationFrameMocks,
  latestEventSource,
  flushUiWork,
  makeSession,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  renderApp,
  selectComboboxOption,
  settleAsyncUi,
  stubScrollIntoView,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";

vi.mock("./MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function MonacoDiffEditorMock(
    _props: Record<string, unknown>,
    _ref: unknown,
  ) {
    return <div data-testid="monaco-diff-mock" />;
  }),
}));

vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function MonacoCodeEditorMock(
    _props: Record<string, unknown>,
    _ref: unknown,
  ) {
    return <div data-testid="monaco-code-mock" />;
  }),
}));

const originalScrollTo = HTMLElement.prototype.scrollTo;

describe("App control panel board section", () => {
  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createScheduledAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    HTMLElement.prototype.scrollTo =
      vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({
      workspaces: [],
    });
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

  it("fetches through a local root of the explicitly selected project and never under All projects", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [
          // A delegation child listed FIRST: the picker must skip it and
          // choose the root session.
          makeSession("session-child", {
            name: "Reviewer child",
            projectId: "project-termal",
            workdir: "/projects/termal",
          }),
          makeSession("session-root", {
            name: "Root session",
            projectId: "project-termal",
            workdir: "/projects/termal",
          }),
          // A second valid root sorts before the first one despite appearing
          // later, pinning deterministic caller selection across list reorder.
          makeSession("session-alpha", {
            name: "Alpha root",
            projectId: "project-termal",
            workdir: "/projects/termal",
          }),
        ],
        delegations: [
          {
            id: "delegation-1",
            parentSessionId: "session-root",
            childSessionId: "session-child",
            mode: "reviewer",
            status: "running",
            title: "Reviewer child",
            agent: "Codex",
            model: "gpt-5.6-sol",
            writePolicy: { kind: "readOnly" },
            createdAt: "2026-07-26T20:00:00Z",
            startedAt: "2026-07-26T20:00:00Z",
            completedAt: null,
            result: null,
          },
        ],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
      const fetchBoardSpy = vi
        .spyOn(api, "fetchCoordinationBoard")
        .mockResolvedValue({
          generation: 3,
          entries: [
            {
              key: "activity.rust-suite",
              revision: 2,
              updatedAtGeneration: 3,
              value: { holder: "Sol" },
              deleted: false,
              authorSessionId: "session-630",
              authorName: "Termal::Codex",
              updatedAt: "2026-07-26T15:00:00.000Z",
              stateStamp: null,
            },
          ],
          nextAfterKey: null,
          unchanged: false,
        });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      stubScrollIntoView();

      await renderApp();
      act(() => {
        latestEventSource().dispatchError();
      });
      await settleAsyncUi();
      await clickAndSettle(
        await screen.findByRole("button", { name: "Board" }),
      );

      // Explicit All-projects scope: guidance, and no board request fired.
      await selectComboboxOption("Project", /All projects/i);
      await settleAsyncUi();
      expect(
        screen.getByText(/Select one project to view its coordination board/),
      ).toBeInTheDocument();
      expect(fetchBoardSpy).not.toHaveBeenCalled();

      // Explicit single-project scope: fetches through the ROOT session and
      // renders the board read-only.
      await selectComboboxOption("Project", /^TermAl$/i);
      await settleAsyncUi();
      expect(fetchBoardSpy).toHaveBeenCalled();
      expect(
        fetchBoardSpy.mock.calls.every(
          ([sessionId]) => sessionId === "session-alpha",
        ),
        "board reads must use the stable lowest-id local root, never the delegation child",
      ).toBe(true);
      expect(
        await screen.findByText("activity.rust-suite"),
      ).toBeInTheDocument();
      expect(screen.getByText(/"holder": "Sol"/)).toBeInTheDocument();
    });
  });

  it("shows honest guidance when the selected project has no local root session", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-orphan",
            name: "Orphan",
            rootPath: "/projects/orphan",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-only-child", {
            name: "Only child",
            projectId: "project-orphan",
            workdir: "/projects/orphan",
          }),
        ],
        delegations: [
          {
            id: "delegation-9",
            parentSessionId: "session-missing-parent",
            childSessionId: "session-only-child",
            mode: "reviewer",
            status: "running",
            title: "Only child",
            agent: "Claude",
            model: "opus[1m]",
            writePolicy: { kind: "readOnly" },
            createdAt: "2026-07-26T20:00:00Z",
            startedAt: "2026-07-26T20:00:00Z",
            completedAt: null,
            result: null,
          },
        ],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
      const fetchBoardSpy = vi
        .spyOn(api, "fetchCoordinationBoard")
        .mockResolvedValue({
          generation: 0,
          entries: [],
          nextAfterKey: null,
          unchanged: false,
        });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      stubScrollIntoView();

      await renderApp();
      act(() => {
        latestEventSource().dispatchError();
      });
      await settleAsyncUi();
      await clickAndSettle(
        await screen.findByRole("button", { name: "Board" }),
      );
      await selectComboboxOption("Project", /^Orphan$/i);
      await settleAsyncUi();

      expect(
        screen.getByText(
          /This project has no local root session to read the board through/,
        ),
      ).toBeInTheDocument();
      expect(fetchBoardSpy).not.toHaveBeenCalled();
    });
  });

  it("explains that coordination boards are unavailable for remote projects", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const initialState = makeStateResponse({
        revision: 1,
        projects: [
          {
            id: "project-remote",
            name: "Remote Project",
            rootPath: "/projects/remote",
            remoteId: "remote-1",
          },
        ],
        orchestrators: [],
        workspaces: [],
        sessions: [
          makeSession("session-remote", {
            name: "Remote session",
            projectId: "project-remote",
            remoteId: "remote-1",
            workdir: "/projects/remote",
          }),
        ],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(initialState);
      vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
      const fetchBoardSpy = vi
        .spyOn(api, "fetchCoordinationBoard")
        .mockResolvedValue({
          generation: 0,
          entries: [],
          nextAfterKey: null,
          unchanged: false,
        });
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );
      stubScrollIntoView();

      await renderApp();
      act(() => {
        latestEventSource().dispatchError();
      });
      await settleAsyncUi();
      await clickAndSettle(
        await screen.findByRole("button", { name: "Board" }),
      );
      await selectComboboxOption("Project", /^Remote Project$/i);
      await settleAsyncUi();

      expect(
        screen.getByText(
          /Coordination boards are local-only in v1; remote projects are not supported/,
        ),
      ).toBeInTheDocument();
      expect(fetchBoardSpy).not.toHaveBeenCalled();
    });
  });
});
