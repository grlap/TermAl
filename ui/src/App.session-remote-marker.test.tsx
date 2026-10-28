// App wiring for the remote marker on session list tiles: a remote-hosted
// session's tile shows the marker beside its agent icon, a local tile shows
// none. The marker's own wording and the CSS that places it are covered in
// session-row-remote-marker.test.tsx.
import { act, cleanup, screen } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import {
  EventSourceMock, ResizeObserverMock, clickAndSettle,
  createScheduledAnimationFrameMocks, dispatchOpenedStateEvent, flushUiWork, latestEventSource,
  makeSession, makeStateResponse, makeWorkspaceLayoutResponse, renderApp, stubScrollIntoView,
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

function tileFor(name: string) {
  const title = Array.from(document.querySelectorAll(".session-row strong"))
    .find((element) => element.textContent === name);
  const shell = title?.closest(".session-row-shell");
  if (!(shell instanceof HTMLElement)) throw new Error(`Session tile ${name} not found`);
  return shell;
}

describe("App session list remote marker", () => {
  it("marks the remote session's tile beside its agent icon and leaves the local tile unchanged", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const state = makeStateResponse({
        revision: 1,
        projects: [],
        orchestrators: [],
        workspaces: [],
        preferences: {
          remotes: [{
            id: "mac-studio",
            name: "Mac Studio",
            transport: "ssh",
            enabled: true,
            host: "mac.local",
            port: null,
            user: "greg",
          }],
        },
        sessions: [
          makeSession("remote-1", { name: "Remote work", remoteId: "mac-studio" }),
          makeSession("local-1", { name: "Local work", agent: "Claude" }),
        ],
      });
      vi.spyOn(api, "fetchState").mockResolvedValue(state);
      await renderApp();
      await dispatchOpenedStateEvent(latestEventSource(), state);
      await clickAndSettle(await screen.findByRole("button", { name: "Sessions" }));

      const remoteTile = tileFor("Remote work");
      const marker = remoteTile.querySelector(".session-row-remote-marker");
      expect(marker).not.toBeNull();
      expect(marker).toHaveAttribute("role", "img");
      expect(marker).toHaveAccessibleName("Remote session on Mac Studio (greg@mac.local)");
      expect(marker).toHaveAttribute("title", "Remote session on Mac Studio (greg@mac.local)");
      // Inside the tile's row button, so a click on it still selects the
      // session; the agent icon stays in its own status button beside it.
      expect(marker?.closest("button")).toHaveClass("session-row");
      const statusButton = remoteTile.querySelector(".session-row-status-button");
      expect(statusButton?.querySelector(".agent-icon")).not.toBeNull();
      expect(marker?.closest("button")?.nextElementSibling).toBe(statusButton);
      expect(remoteTile).toHaveClass("remote");

      const localTile = tileFor("Local work");
      expect(localTile.querySelector(".session-row-remote-marker")).toBeNull();
      expect(localTile).not.toHaveClass("remote");
      expect(localTile.querySelector(".session-row-status-button .agent-icon")).not.toBeNull();
    });
  });
});
