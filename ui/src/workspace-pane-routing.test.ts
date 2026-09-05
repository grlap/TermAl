// Owns focused control-pane and viewer-lane routing regressions.
// Does not own the general workspace reducer suite.
// Created as the focused home for routing scenarios that cross pane roles.

import { describe, expect, it, vi } from "vitest";

import { createSessionTab } from "./workspace-tabs";
import {
  openDiffPreviewInWorkspaceState,
  openSourceInWorkspaceState,
  placeDraggedTab,
  placeExternalTab,
  placeSessionDropInWorkspaceState,
} from "./workspace";
import {
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace-types";

function makeSessionTab(id: string, sessionId: string): WorkspaceTab {
  return { id, kind: "session", sessionId };
}

function makeControlPanelTab(id: string): WorkspaceTab {
  return { id, kind: "controlPanel", originSessionId: null };
}

function makePane(
  id: string,
  tabs: WorkspaceTab[],
  options?: {
    activeTabId?: string | null;
    activeSessionId?: string | null;
    viewMode?: WorkspacePane["viewMode"];
  },
): WorkspacePane {
  return {
    id,
    tabs,
    activeTabId: options?.activeTabId ?? tabs[0]?.id ?? null,
    activeSessionId:
      options?.activeSessionId ??
      tabs.find((tab) => tab.kind === "session")?.sessionId ??
      null,
    viewMode: options?.viewMode ?? "session",
    lastSessionViewMode: "session",
    sourcePath: null,
  };
}

function makeControlPane(): WorkspacePane {
  return makePane("pane-control", [makeControlPanelTab("tab-control")], {
    activeTabId: "tab-control",
    activeSessionId: null,
    viewMode: "controlPanel",
  });
}

function makeSinglePaneWorkspace(pane: WorkspacePane): WorkspaceState {
  return {
    lastContentPaneId: null,
    lastViewerPaneId: null,
    root: { type: "pane", paneId: pane.id },
    panes: [pane],
    activePaneId: pane.id,
  };
}

function makeControlSplitWorkspace() {
  const sessionPane = makePane("pane-session", [
    makeSessionTab("tab-session", "session-a"),
  ]);
  const controlPane = makeControlPane();
  const workspace: WorkspaceState = {
    lastContentPaneId: null,
    lastViewerPaneId: null,
    root: {
      id: "split-root",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: { type: "pane", paneId: sessionPane.id },
      second: { type: "pane", paneId: controlPane.id },
    },
    panes: [sessionPane, controlPane],
    activePaneId: controlPane.id,
  };
  return { controlPane, sessionPane, workspace };
}

describe("workspace pane routing", () => {
  it("preserves an explicitly allocated session tab id", () => {
    expect(createSessionTab("session-new", "tab-gesture")).toEqual({
      id: "tab-gesture",
      kind: "session",
      sessionId: "session-new",
    });
  });

  it("threads a gesture-owned session tab id through rail and edge drops", () => {
    const { sessionPane, workspace } = makeControlSplitWorkspace();

    const railDrop = placeSessionDropInWorkspaceState(
      workspace,
      "session-rail",
      sessionPane.id,
      "tabs",
      undefined,
      "tab-gesture-rail",
    );
    expect(
      railDrop.panes
        .find((pane) => pane.id === sessionPane.id)
        ?.tabs.find(
          (tab) => tab.kind === "session" && tab.sessionId === "session-rail",
        ),
    ).toEqual({
      id: "tab-gesture-rail",
      kind: "session",
      sessionId: "session-rail",
    });

    const edgeDrop = placeSessionDropInWorkspaceState(
      workspace,
      "session-edge",
      sessionPane.id,
      "right",
      undefined,
      "tab-gesture-edge",
    );
    expect(
      edgeDrop.panes
        .flatMap((pane) => pane.tabs)
        .find(
          (tab) => tab.kind === "session" && tab.sessionId === "session-edge",
        ),
    ).toEqual({
      id: "tab-gesture-edge",
      kind: "session",
      sessionId: "session-edge",
    });
  });

  it("keeps a generated session tab id across the edge-drop clone boundary", () => {
    const randomUuid = vi
      .spyOn(crypto, "randomUUID")
      .mockReturnValue("00000000-0000-4000-8000-000000000103")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000101")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000102");
    const { sessionPane, workspace } = makeControlSplitWorkspace();

    try {
      const edgeDrop = placeSessionDropInWorkspaceState(
        workspace,
        "session-generated",
        sessionPane.id,
        "right",
      );

      expect(
        edgeDrop.panes
          .flatMap((pane) => pane.tabs)
          .find(
            (tab) =>
              tab.kind === "session" &&
              tab.sessionId === "session-generated",
          )?.id,
      ).toBe("00000000-0000-4000-8000-000000000101");
      // One id each for the session tab, adjacent pane, and split node. The
      // clone boundary must not allocate a fourth replacement tab id.
      expect(randomUuid).toHaveBeenCalledTimes(3);
    } finally {
      randomUuid.mockRestore();
    }
  });

  it("refuses session tab drops into a control-panel rail", () => {
    const { controlPane, workspace } = makeControlSplitWorkspace();

    expect(
      placeSessionDropInWorkspaceState(
        workspace,
        "session-new",
        controlPane.id,
        "tabs",
      ),
    ).toBe(workspace);
    expect(
      placeSessionDropInWorkspaceState(
        workspace,
        "session-a",
        controlPane.id,
        "tabs",
      ),
    ).toBe(workspace);
  });

  it("refuses vertical control-pane drops and accepts horizontal side splits", () => {
    const { controlPane, sessionPane, workspace } =
      makeControlSplitWorkspace();
    const externalTab = makeSessionTab("tab-external", "session-external");

    expect(
      placeSessionDropInWorkspaceState(
        workspace,
        "session-new",
        controlPane.id,
        "top",
      ),
    ).toBe(workspace);
    expect(
      placeExternalTab(workspace, externalTab, controlPane.id, "bottom"),
    ).toBe(workspace);
    expect(
      placeDraggedTab(
        workspace,
        sessionPane.id,
        "tab-session",
        controlPane.id,
        "top",
      ),
    ).toBe(workspace);

    const externalSideSplit = placeExternalTab(
      workspace,
      externalTab,
      controlPane.id,
      "left",
    );
    expect(externalSideSplit.panes).toHaveLength(3);
    expect(externalSideSplit.panes).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          tabs: [
            expect.objectContaining({
              kind: "session",
              sessionId: "session-external",
            }),
          ],
        }),
        expect.objectContaining({ id: controlPane.id }),
      ]),
    );

    const draggedSideSplit = placeDraggedTab(
      workspace,
      sessionPane.id,
      "tab-session",
      controlPane.id,
      "right",
    );
    expect(draggedSideSplit.panes).toHaveLength(2);
    expect(draggedSideSplit.panes).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          tabs: [makeSessionTab("tab-session", "session-a")],
        }),
        expect.objectContaining({ id: controlPane.id }),
      ]),
    );
  });

  it("refuses external drops when their explicit target pane disappeared", () => {
    const { workspace } = makeControlSplitWorkspace();
    const externalTab = makeSessionTab("tab-external", "session-external");

    expect(
      placeExternalTab(workspace, externalTab, "pane-missing", "tabs"),
    ).toBe(workspace);
    expect(
      placeExternalTab(workspace, externalTab, "pane-missing", "right"),
    ).toBe(workspace);
  });

  it("refuses stacking an external control panel into a tab rail", () => {
    const { sessionPane, workspace } = makeControlSplitWorkspace();

    expect(
      placeExternalTab(
        workspace,
        makeControlPanelTab("tab-external-control"),
        sessionPane.id,
        "tabs",
      ),
    ).toBe(workspace);
  });

  it("preserves a control-only pane when opening the first source viewer", () => {
    const controlPane = makeControlPane();
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(controlPane),
      "/tmp/new.ts",
      controlPane.id,
      null,
      { allowViewerSplit: false },
    );

    const viewerPane = next.panes.find((pane) => pane.id !== controlPane.id);
    expect(next.panes).toHaveLength(2);
    expect(
      next.panes.find((pane) => pane.id === controlPane.id)?.tabs,
    ).toEqual([makeControlPanelTab("tab-control")]);
    expect(viewerPane?.tabs).toEqual([
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/new.ts",
        originSessionId: null,
      },
    ]);
    expect(next.activePaneId).toBe(viewerPane?.id);
    expect(next.lastViewerPaneId).toBe(viewerPane?.id);
  });

  it("preserves a control-only pane when opening the first diff viewer", () => {
    const controlPane = makeControlPane();
    const next = openDiffPreviewInWorkspaceState(
      makeSinglePaneWorkspace(controlPane),
      {
        changeType: "edit",
        diff: "-before\n+after",
        diffMessageId: "diff-a",
        filePath: "/tmp/app.ts",
        language: "typescript",
        originSessionId: null,
        summary: "Updated app.ts",
      },
      controlPane.id,
      { allowViewerSplit: false },
    );

    const viewerPane = next.panes.find((pane) => pane.id !== controlPane.id);
    expect(next.panes).toHaveLength(2);
    expect(
      next.panes.find((pane) => pane.id === controlPane.id)?.tabs,
    ).toEqual([makeControlPanelTab("tab-control")]);
    expect(viewerPane?.tabs).toEqual([
      expect.objectContaining({
        kind: "diffPreview",
        diffMessageId: "diff-a",
        filePath: "/tmp/app.ts",
      }),
    ]);
    expect(next.activePaneId).toBe(viewerPane?.id);
    expect(next.lastViewerPaneId).toBe(viewerPane?.id);
  });
});
