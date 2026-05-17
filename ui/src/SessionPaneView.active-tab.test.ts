import { describe, expect, it } from "vitest";
import { resolveSessionPaneActiveTab } from "./SessionPaneView.active-tab";
import type { WorkspacePane, WorkspaceTab } from "./workspace";

const firstTab: WorkspaceTab = {
  id: "session-tab",
  kind: "session",
  sessionId: "session-1",
};

const secondTab: WorkspaceTab = {
  id: "source-tab",
  kind: "source",
  path: "/repo/src/main.ts",
  originSessionId: "session-1",
};

function pane(overrides: Partial<WorkspacePane>): WorkspacePane {
  return {
    id: "pane-1",
    activeSessionId: null,
    activeTabId: null,
    lastSessionViewMode: "session",
    sourcePath: null,
    tabs: [],
    viewMode: "session",
    ...overrides,
  };
}

describe("resolveSessionPaneActiveTab", () => {
  it("returns the matching active tab when present", () => {
    expect(
      resolveSessionPaneActiveTab(
        pane({
          activeTabId: "source-tab",
          tabs: [firstTab, secondTab],
        }),
      ),
    ).toBe(secondTab);
  });

  it("falls back to the first tab when activeTabId is missing or stale", () => {
    expect(
      resolveSessionPaneActiveTab(
        pane({
          activeTabId: "stale-tab",
          tabs: [firstTab, secondTab],
        }),
      ),
    ).toBe(firstTab);

    expect(
      resolveSessionPaneActiveTab(
        pane({
          activeTabId: null,
          tabs: [firstTab, secondTab],
        }),
      ),
    ).toBe(firstTab);
  });

  it("returns null for empty panes", () => {
    expect(resolveSessionPaneActiveTab(pane({ tabs: [] }))).toBeNull();
  });
});
