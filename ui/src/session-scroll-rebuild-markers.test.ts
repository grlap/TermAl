import { describe, expect, it } from "vitest";

import { beginSessionScrollBottomRebuild } from "./session-scroll-rebuild-markers";
import type { WorkspaceState, WorkspaceTab } from "./workspace";

function makeWorkspace(tabs: WorkspaceTab[]): WorkspaceState {
  return {
    root: { type: "pane", paneId: "pane-a" },
    activePaneId: "pane-a",
    panes: [
      {
        id: "pane-a",
        activeSessionId: "session-a",
        activeTabId: tabs[0]?.id ?? null,
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs,
        viewMode: "session",
      },
    ],
  };
}

describe("session scroll rebuild markers", () => {
  it("removes markers that were newly armed when the rebuild is rolled back", () => {
    const markers: Record<string, true | undefined> = {};
    const workspace = makeWorkspace([
      { id: "tab-a", kind: "session", sessionId: "session-a" },
    ]);
    const rollback = beginSessionScrollBottomRebuild(
      () => markers,
      workspace,
      {
        sessionIds: ["session-b"],
        tabs: [
          { id: "tab-c", kind: "session", sessionId: "session-c" },
          {
            id: "tab-source",
            kind: "source",
            path: null,
            originSessionId: null,
          },
        ],
      },
    );

    expect(markers).toEqual({
      "session-a": true,
      "session-b": true,
      "session-c": true,
    });

    rollback();

    expect(markers).toEqual({});
  });

  it("restores prior truthy markers on the current marker map", () => {
    let markers: Record<string, true | undefined> = {
      "session-a": true,
      "session-b": undefined,
    };
    const workspace = makeWorkspace([
      { id: "tab-a", kind: "session", sessionId: "session-a" },
    ]);
    const rollback = beginSessionScrollBottomRebuild(
      () => markers,
      workspace,
      { sessionIds: ["session-b", "session-c"] },
    );

    markers = { unrelated: true };
    rollback();

    expect(markers["session-a"]).toBe(true);
    expect(Object.prototype.hasOwnProperty.call(markers, "session-b")).toBe(
      false,
    );
    expect(Object.prototype.hasOwnProperty.call(markers, "session-c")).toBe(
      false,
    );
    expect(markers.unrelated).toBe(true);
  });
});
