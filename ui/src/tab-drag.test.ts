import {
  isWorkspaceTabDragChannelMessage,
  type WorkspaceTabDragChannelMessage,
} from "./tab-drag";

describe("workspace tab drag channel messages", () => {
  it("accepts valid drag lifecycle messages", () => {
    const dragStart: WorkspaceTabDragChannelMessage = {
      type: "drag-start",
      payload: {
        dragId: "drag-1",
        sourceWindowId: "window-a",
        sourcePaneId: "pane-a",
        tabId: "tab-a",
        tab: {
          id: "tab-a",
          kind: "session",
          sessionId: "session-a",
        },
      },
    };
    const dropCommit: WorkspaceTabDragChannelMessage = {
      type: "drop-commit",
      dragId: "drag-1",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-a",
      tabId: "tab-a",
      targetWindowId: "window-b",
    };

    expect(isWorkspaceTabDragChannelMessage(dragStart)).toBe(true);
    expect(isWorkspaceTabDragChannelMessage(dropCommit)).toBe(true);
  });

  it("rejects malformed tab payloads", () => {
    // diffPreview tabs require changeType, diff, filePath, originSessionId, and summary.
    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-1",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "tab-a",
          tab: {
            id: "tab-a",
            kind: "diffPreview",
            diffMessageId: "diff-1",
          },
        },
      }),
    ).toBe(false);
  });
});

