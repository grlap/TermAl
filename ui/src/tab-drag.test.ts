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

  it("accepts canvas tab payloads with persisted cards", () => {
    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-canvas",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "canvas-a",
          tab: {
            id: "canvas-a",
            kind: "canvas",
            cards: [{ sessionId: "session-a", x: 160, y: 220 }],
            zoom: 1.4,
            originSessionId: "session-a",
          },
        },
      }),
    ).toBe(true);
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

  it("rejects malformed canvas card payloads", () => {
    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-bad-canvas",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "canvas-a",
          tab: {
            id: "canvas-a",
            kind: "canvas",
            cards: [{ sessionId: "session-a", x: "bad", y: 220 }],
            originSessionId: null,
          },
        },
      }),
    ).toBe(false);
  });

  it("rejects malformed canvas zoom payloads", () => {
    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-bad-canvas-zoom",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "canvas-a",
          tab: {
            id: "canvas-a",
            kind: "canvas",
            cards: [{ sessionId: "session-a", x: 160, y: 220 }],
            zoom: "bad",
            originSessionId: null,
          },
        },
      }),
    ).toBe(false);
  });

  it("rejects origin-only tab payloads with invalid originProjectId values", () => {
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
            kind: "controlPanel",
            originSessionId: null,
            originProjectId: 7,
          },
        },
      }),
    ).toBe(false);

    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-2",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "tab-b",
          tab: {
            id: "tab-b",
            kind: "sessionList",
            originSessionId: null,
            originProjectId: { bad: true },
          },
        },
      }),
    ).toBe(false);

    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-3",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "tab-c",
          tab: {
            id: "tab-c",
            kind: "projectList",
            originSessionId: null,
            originProjectId: ["bad"],
          },
        },
      }),
    ).toBe(false);
  });
});

