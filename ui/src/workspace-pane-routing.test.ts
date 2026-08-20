// Owns focused control-pane and viewer-lane routing regressions.
// Does not own the general workspace reducer suite.
// Created as the focused home for routing scenarios that cross pane roles.

import { describe, expect, it } from "vitest";

import {
  openDiffPreviewInWorkspaceState,
  openSourceInWorkspaceState,
  placeDraggedTab,
  placeExternalTab,
  placeSessionDropInWorkspaceState,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";

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
