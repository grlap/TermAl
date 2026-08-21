// Owns focused tests for pane/session scroll-position migration.
// Does not own DOM scroll restoration or workspace placement behavior.
// Split from: ui/src/App.scroll-behavior.test.tsx.

import { describe, expect, it } from "vitest";
import { resolveSessionPaneScrollStateKey } from "./SessionPaneView.scroll-key";
import {
  beginSessionPaneScrollPositionMigration,
  migrateSessionPaneScrollPosition,
  type PaneScrollPositionsByPane,
} from "./pane-scroll-position-migration";
import { splitPane, type WorkspaceState } from "./workspace";

function scrollKey(
  paneId: string,
  sessionId: string,
  viewMode: "session" | "prompt" | "commands" | "diffs" = "session",
) {
  return resolveSessionPaneScrollStateKey(
    paneId,
    viewMode,
    sessionId,
    null,
  );
}

describe("migrateSessionPaneScrollPosition", () => {
  it("moves the exact detached position and sticky intent without changing other panes", () => {
    const sourceKey = scrollKey("pane-source", "session-a");
    const targetKey = scrollKey("pane-target", "session-a");
    const sourcePromptKey = scrollKey(
      "pane-source",
      "session-a",
      "prompt",
    );
    const targetPromptKey = scrollKey(
      "pane-target",
      "session-a",
      "prompt",
    );
    const duplicateKey = scrollKey("pane-duplicate", "session-a");
    const otherKey = scrollKey("pane-other", "session-b");
    const savedPosition = { shouldStick: false, top: 12_345.75 };
    const duplicatePosition = { shouldStick: true, top: 98_765.5 };
    const otherPosition = { shouldStick: false, top: 321.25 };
    const paneScrollPositions: PaneScrollPositionsByPane = {
      "pane-source": {
        [sourceKey]: savedPosition,
        [sourcePromptKey]: { shouldStick: true, top: 88 },
      },
      "pane-duplicate": { [duplicateKey]: duplicatePosition },
      "pane-other": { [otherKey]: otherPosition },
    };
    const paneShouldStickToBottom: Record<string, boolean | undefined> = {
      [sourceKey]: false,
      [sourcePromptKey]: true,
      [duplicateKey]: true,
      [otherKey]: false,
    };

    expect(
      migrateSessionPaneScrollPosition({
        paneShouldStickToBottom,
        paneScrollPositions,
        sessionId: "session-a",
        sourcePaneId: "pane-source",
        targetPaneId: "pane-target",
      }),
    ).toBe(true);

    expect(paneScrollPositions["pane-source"]).toEqual({});
    expect(paneScrollPositions["pane-target"]?.[targetKey]).toBe(
      savedPosition,
    );
    expect(paneShouldStickToBottom).not.toHaveProperty(sourceKey);
    expect(paneShouldStickToBottom[targetKey]).toBe(false);
    expect(paneScrollPositions["pane-target"]?.[targetPromptKey]).toEqual({
      shouldStick: true,
      top: 88,
    });
    expect(paneShouldStickToBottom[targetPromptKey]).toBe(true);
    expect(paneScrollPositions["pane-duplicate"]?.[duplicateKey]).toBe(
      duplicatePosition,
    );
    expect(paneShouldStickToBottom[duplicateKey]).toBe(true);
    expect(paneScrollPositions["pane-other"]?.[otherKey]).toBe(otherPosition);
    expect(paneShouldStickToBottom[otherKey]).toBe(false);
  });

  it("leaves existing pane state untouched when the source pair has no history", () => {
    const existingKey = scrollKey("pane-existing", "session-a");
    const paneScrollPositions: PaneScrollPositionsByPane = {
      "pane-existing": {
        [existingKey]: { shouldStick: false, top: 4_200 },
      },
    };
    const paneShouldStickToBottom = { [existingKey]: false };
    const positionsBefore = structuredClone(paneScrollPositions);
    const stickyBefore = structuredClone(paneShouldStickToBottom);

    expect(
      migrateSessionPaneScrollPosition({
        paneShouldStickToBottom,
        paneScrollPositions,
        sessionId: "session-a",
        sourcePaneId: "pane-new",
        targetPaneId: "pane-target",
      }),
    ).toBe(false);
    expect(paneScrollPositions).toEqual(positionsBefore);
    expect(paneShouldStickToBottom).toEqual(stickyBefore);
  });

  it("is a no-op when the pane id does not change", () => {
    const key = scrollKey("pane-a", "session-a");
    const paneScrollPositions: PaneScrollPositionsByPane = {
      "pane-a": { [key]: { shouldStick: false, top: 777 } },
    };
    const paneShouldStickToBottom = { [key]: false };

    expect(
      migrateSessionPaneScrollPosition({
        paneShouldStickToBottom,
        paneScrollPositions,
        sessionId: "session-a",
        sourcePaneId: "pane-a",
        targetPaneId: "pane-a",
      }),
    ).toBe(false);
    expect(paneScrollPositions["pane-a"]?.[key]?.top).toBe(777);
    expect(paneShouldStickToBottom[key]).toBe(false);
  });

  it("rolls a speculative migration back to exact source and target ownership", () => {
    const sourceKey = scrollKey("pane-source", "session-a");
    const targetKey = scrollKey("pane-target", "session-a");
    const paneScrollPositions: PaneScrollPositionsByPane = {
      "pane-source": {
        [sourceKey]: { shouldStick: false, top: 8_888 },
      },
      "pane-target": {
        [targetKey]: { shouldStick: true, top: 99_999 },
      },
    };
    const paneShouldStickToBottom: Record<string, boolean | undefined> = {
      [sourceKey]: false,
      [targetKey]: true,
    };
    const positionsBefore = structuredClone(paneScrollPositions);
    const stickyBefore = structuredClone(paneShouldStickToBottom);

    const migration = beginSessionPaneScrollPositionMigration({
      paneShouldStickToBottom,
      paneScrollPositions,
      sessionId: "session-a",
      sourcePaneId: "pane-source",
      targetPaneId: "pane-target",
    });
    expect(migration).not.toBeNull();
    expect(paneScrollPositions["pane-target"]?.[targetKey]?.top).toBe(8_888);

    migration?.rollback();
    expect(paneScrollPositions).toEqual(positionsBefore);
    expect(paneShouldStickToBottom).toEqual(stickyBefore);
  });

  it("preserves the moved active session position when splitPane creates its target", () => {
    const workspace: WorkspaceState = {
      root: { type: "pane", paneId: "pane-source" },
      activePaneId: "pane-source",
      panes: [
        {
          id: "pane-source",
          activeSessionId: "session-moved",
          activeTabId: "tab-moved",
          lastSessionViewMode: "session",
          sourcePath: null,
          tabs: [
            {
              id: "tab-remaining",
              kind: "session",
              sessionId: "session-remaining",
            },
            {
              id: "tab-moved",
              kind: "session",
              sessionId: "session-moved",
            },
          ],
          viewMode: "session",
        },
      ],
    };
    const movedSourceKey = scrollKey("pane-source", "session-moved");
    const movedTargetKey = scrollKey("pane-created", "session-moved");
    const remainingKey = scrollKey("pane-source", "session-remaining");
    const paneScrollPositions: PaneScrollPositionsByPane = {
      "pane-source": {
        [movedSourceKey]: { shouldStick: false, top: 19_876.5 },
        [remainingKey]: { shouldStick: false, top: 444 },
      },
    };
    const paneShouldStickToBottom: Record<string, boolean | undefined> = {
      [movedSourceKey]: false,
      [remainingKey]: false,
    };

    const next = splitPane(workspace, "pane-source", "row", "pane-created");
    const movedTab = next.panes
      .find((pane) => pane.id === "pane-created")
      ?.tabs.find((tab) => tab.id === "tab-moved");
    expect(movedTab).toMatchObject({
      kind: "session",
      sessionId: "session-moved",
    });

    expect(
      migrateSessionPaneScrollPosition({
        paneShouldStickToBottom,
        paneScrollPositions,
        sessionId: "session-moved",
        sourcePaneId: "pane-source",
        targetPaneId: "pane-created",
      }),
    ).toBe(true);
    expect(paneScrollPositions["pane-created"]?.[movedTargetKey]).toEqual({
      shouldStick: false,
      top: 19_876.5,
    });
    expect(paneShouldStickToBottom[movedTargetKey]).toBe(false);
    expect(paneScrollPositions["pane-source"]?.[remainingKey]).toEqual({
      shouldStick: false,
      top: 444,
    });
    expect(paneShouldStickToBottom[remainingKey]).toBe(false);
  });
});
