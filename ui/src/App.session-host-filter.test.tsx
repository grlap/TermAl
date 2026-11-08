// Exercises host selection through the real session panel, including its row
// window. Does not inspect implementation or instruction/documentation text.
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import { makeTestRunActivity, makeTestRunOverview } from "./test-runs-fixtures";
import {
  EventSourceMock, ResizeObserverMock, clickAndSettle,
  createScheduledAnimationFrameMocks, dispatchOpenedStateEvent, flushUiWork, latestEventSource,
  makeSession, makeStateSessionSummary, makeStateResponse, makeWorkspaceLayoutResponse, renderApp, stubScrollIntoView,
  settleAsyncUi,
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

const remote = (id: string, enabled = true) => ({
  id, name: id, host: `${id}.local`, transport: "ssh" as const, enabled, port: null, user: null,
});
async function choose(surface: HTMLElement, control: string, option: string) {
  await clickAndSettle(within(surface).getByRole("combobox", { name: control }));
  const choice = within(screen.getByRole("listbox")).getAllByRole("option")
    .find(node => node.querySelector(".combo-option-label")?.textContent?.trim() === option);
  if (!choice) throw new Error(`Missing ${control} choice ${option}`);
  await clickAndSettle(choice);
}
function rowNames(surface: HTMLElement) {
  return Array.from(surface.querySelectorAll(".session-row strong"), node => node.textContent);
}

describe("session panel host filtering", () => {
  it.each(["docked", "standalone"])("scopes status counts, working breakdown and search to the host in the %s list", async kind => {
    const state = {
      ...makeStateResponse({
        revision: 1, projects: [], orchestrators: [], workspaces: [],
        preferences: { remotes: [remote("Mac")] },
        sessions: [
          makeSession("local-idle", { name: "Local idle" }),
          makeSession("local-active", { name: "Local active", status: "active" }),
          makeSession("remote-active", { name: "Remote active", remoteId: "Mac", status: "active" }),
          { ...makeStateSessionSummary(makeSession("remote-background", { name: "Remote background", remoteId: "Mac" })),
            testRunActivity: makeTestRunActivity({ ownedRunning: 1, backgroundRunning: 1 }) },
          makeSession("remote-asking", { name: "Remote asking", remoteId: "Mac", status: "approval" }),
          makeSession("remote-idle", { name: "Remote idle", remoteId: "Mac" }),
          makeSession("remote-error", { name: "Remote error", remoteId: "Mac", status: "error" }),
        ],
      }),
      testRunOverview: makeTestRunOverview({ total: 1, running: 1 }),
    };
    vi.spyOn(api, "fetchState").mockResolvedValue(state);
    await renderApp();
    await dispatchOpenedStateEvent(latestEventSource(), state);
    await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
    const docked = document.querySelector<HTMLElement>(".control-panel-section-sessions")!;
    await choose(docked, "Project", "All projects");
    if (kind === "standalone") {
      await choose(docked, "Host", "Local");
      await clickAndSettle(screen.getByRole("button", { name: "Open tab" }));
    }
    const surfaces = document.querySelectorAll<HTMLElement>(".control-panel-section-sessions");
    const surface = surfaces[surfaces.length - 1];
    await choose(surface, "Project", "All projects");
    await choose(surface, "Host", "Mac");
    const list = within(surface);
    expect(list.getByRole("button", { name: "No filter (5)" })).toBeVisible();
    const working = list.getByRole("button", { name: "Working sessions (2)" });
    expect(working).toHaveAccessibleDescription("1 foreground / 1 background. A session with a foreground turn and background work counts in both.");
    expect(list.getByRole("button", { name: "Asking (1)" })).toBeVisible();
    expect(list.getByRole("button", { name: "Completed (1)" })).toBeVisible();
    expect(list.getByRole("button", { name: "Error (1)" })).toBeVisible();
    await clickAndSettle(working);
    expect(rowNames(surface).sort()).toEqual(["Remote active", "Remote background"]);
    fireEvent.change(list.getByRole("searchbox", { name: "Search sessions" }), { target: { value: "active" } });
    await settleAsyncUi();
    expect(rowNames(surface)).toEqual(["Remote active"]);
    expect(list.getByText("1 matching session")).toBeVisible();
    await choose(surface, "Host", "All hosts");
    expect(rowNames(surface).sort()).toEqual(["Local active", "Remote active"]);
    expect(list.getByText("2 matching sessions")).toBeVisible();
    expect(list.getByRole("button", { name: "No filter (7)" })).toBeVisible();
    expect(list.getByRole("button", { name: "Working sessions (3)" })).toHaveAccessibleDescription("2 foreground / 1 background. A session with a foreground turn and background work counts in both.");
    if (kind === "standalone") {
      expect(within(docked).getByRole("combobox", { name: "Host" })).toHaveTextContent("Local");
      expect(rowNames(docked).sort()).toEqual(["Local active", "Local idle"]);
    }
  });

  it("retains independent host choices after leaving and returning to a standalone Sessions tab", async () => {
    const state = makeStateResponse({
      revision: 1, projects: [], orchestrators: [], workspaces: [],
      preferences: { remotes: [remote("Mac")] },
      sessions: [makeSession("local", { name: "Local" }), makeSession("remote", { name: "Remote", remoteId: "Mac" })],
    });
    vi.spyOn(api, "fetchState").mockResolvedValue(state);
    await renderApp();
    await dispatchOpenedStateEvent(latestEventSource(), state);
    await clickAndSettle(await screen.findByRole("button", { name: "Canvas" }));
    await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
    const docked = document.querySelector<HTMLElement>(".control-panel-section-sessions")!;
    await choose(docked, "Host", "Local");
    await clickAndSettle(screen.getByRole("button", { name: "Open tab" }));
    const sessionsTab = screen.getByRole("tab", { name: /^Sessions/ });
    const pane = sessionsTab.closest<HTMLElement>(".workspace-pane")!;
    const standalone = pane.querySelector<HTMLElement>(".control-panel-section-sessions")!;
    await choose(standalone, "Host", "Mac");
    expect(rowNames(standalone)).toEqual(["Remote"]);
    await clickAndSettle(within(pane).getByRole("tab", { name: /Canvas/ }));
    expect(standalone).not.toBeInTheDocument();
    await clickAndSettle(sessionsTab);
    const restored = pane.querySelector<HTMLElement>(".control-panel-section-sessions")!;
    expect(within(restored).getByRole("combobox", { name: "Host" })).toHaveTextContent("Mac");
    expect(rowNames(restored)).toEqual(["Remote"]);
    expect(within(docked).getByRole("combobox", { name: "Host" })).toHaveTextContent("Local");
    expect(rowNames(docked)).toEqual(["Local"]);
  });

  it.each(["docked", "standalone"])("combines host, project and search in the %s list", async kind => {
    const state = makeStateResponse({
      revision: 1, orchestrators: [], workspaces: [],
      projects: [
        { id: "p1", name: "One", rootPath: "/one" },
        { id: "p2", name: "Two", rootPath: "/two", remoteId: "Mac" },
      ],
      preferences: { remotes: [remote("Mac"), remote("Offline", false)] },
      sessions: [
        makeSession("local", { name: "Local work", projectId: "p1" }),
        makeSession("remote", { name: "Remote work", projectId: "p1", remoteId: "Mac" }),
        makeSession("other", { name: "Remote other", projectId: "p2" }),
      ],
    });
    vi.spyOn(api, "fetchState").mockResolvedValue(state);
    await renderApp();
    await dispatchOpenedStateEvent(latestEventSource(), state);
    await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
    if (kind === "standalone") await clickAndSettle(screen.getByRole("button", { name: "Open tab" }));
    const surfaces = document.querySelectorAll<HTMLElement>(".control-panel-section-sessions");
    const surface = surfaces[surfaces.length - 1];
    if (kind === "standalone") {
      expect(within(surface).getByRole("combobox", { name: "Project" })).toHaveTextContent("One");
      expect(rowNames(surface).sort()).toEqual(["Local work", "Remote work"]);
    }
    await choose(surface, "Project", "All projects");
    expect(within(surface).getByRole("combobox", { name: "Host" })).toHaveTextContent("All hosts");
    await choose(surface, "Host", "Local");
    expect(rowNames(surface)).toEqual(["Local work"]);
    await choose(surface, "Host", "Mac");
    expect(rowNames(surface).sort()).toEqual(["Remote other", "Remote work"]);
    await choose(surface, "Project", "One");
    expect(rowNames(surface)).toEqual(["Remote work"]);
    fireEvent.change(within(surface).getByRole("searchbox", { name: "Search sessions" }), { target: { value: "Local work" } });
    await settleAsyncUi();
    expect(rowNames(surface)).toEqual([]);
    await choose(surface, "Host", "All hosts");
    expect(rowNames(surface)).toEqual(["Local work"]);
    await choose(surface, "Host", "Offline");
    expect(rowNames(surface)).toEqual([]);
    await choose(surface, "Host", "All hosts");
    expect(rowNames(surface)).toEqual(["Local work"]);
  });

  it("keeps many sessions windowed and finds a remote row outside the initial window", async () => {
    const state = makeStateResponse({
      revision: 1, projects: [], orchestrators: [], workspaces: [],
      preferences: { remotes: [remote("Mac")] },
      sessions: Array.from({ length: 1000 }, (_, i) => makeSession(`session-${i}`, {
        name: `Work ${i}`, remoteId: i % 2 ? "Mac" : undefined,
      })),
    });
    vi.spyOn(api, "fetchState").mockResolvedValue(state);
    await renderApp();
    await dispatchOpenedStateEvent(latestEventSource(), state);
    await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));
    const surface = document.querySelector<HTMLElement>(".control-panel-section-sessions")!;
    expect(surface.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(30);
    await choose(surface, "Host", "Mac");
    expect(surface.querySelectorAll(".session-row-shell").length).toBeLessThanOrEqual(30);
    expect(rowNames(surface).length).toBeGreaterThan(0);
    expect(rowNames(surface).every(name => Number(name?.split(" ")[1]) % 2 === 1)).toBe(true);
    fireEvent.change(within(surface).getByRole("searchbox", { name: "Search sessions" }), { target: { value: "Work 997" } });
    await settleAsyncUi();
    expect(rowNames(surface)).toEqual(["Work 997"]);
  });
});
