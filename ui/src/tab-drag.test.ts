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
    const dragEnd: WorkspaceTabDragChannelMessage = {
      type: "drag-end",
      dragId: "drag-1",
      sourceWindowId: "window-a",
    };

    expect(isWorkspaceTabDragChannelMessage(dragStart)).toBe(true);
    expect(isWorkspaceTabDragChannelMessage(dropCommit)).toBe(true);
    expect(isWorkspaceTabDragChannelMessage(dragEnd)).toBe(true);
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

  it("accepts orchestrator canvas tab payloads", () => {
    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-orchestrator",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "orchestrator-a",
          tab: {
            id: "orchestrator-a",
            kind: "orchestratorCanvas",
            originSessionId: null,
            templateId: "template-a",
          },
        },
      }),
    ).toBe(true);
  });

  it("accepts diff preview payloads without a language field", () => {
    expect(
      isWorkspaceTabDragChannelMessage({
        type: "drag-start",
        payload: {
          dragId: "drag-diff",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "diff-a",
          tab: {
            id: "diff-a",
            kind: "diffPreview",
            changeType: "edit",
            diff: "@@ -1 +1 @@",
            diffMessageId: "diff-message-a",
            filePath: "src/main.rs",
            originSessionId: "session-a",
            originProjectId: "project-a",
            summary: "Update preview",
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
    const invalidTabs = [
      {
        id: "tab-a",
        kind: "controlPanel",
        originSessionId: null,
        originProjectId: 7,
      },
      {
        id: "tab-b",
        kind: "sessionList",
        originSessionId: null,
        originProjectId: { bad: true },
      },
      {
        id: "tab-c",
        kind: "projectList",
        originSessionId: null,
        originProjectId: ["bad"],
      },
      {
        id: "tab-d",
        kind: "orchestratorList",
        originSessionId: null,
        originProjectId: { bad: true },
      },
      {
        id: "tab-e",
        kind: "source",
        path: "src/main.rs",
        originSessionId: null,
        originProjectId: 99,
      },
      {
        id: "tab-f",
        kind: "filesystem",
        rootPath: "C:/repo",
        originSessionId: null,
        originProjectId: { bad: true },
      },
      {
        id: "tab-g",
        kind: "gitStatus",
        workdir: "C:/repo",
        originSessionId: null,
        originProjectId: ["bad"],
      },
      {
        id: "tab-h",
        kind: "canvas",
        cards: [{ sessionId: "session-a", x: 160, y: 220 }],
        originSessionId: null,
        originProjectId: 11,
      },
      {
        id: "tab-i",
        kind: "instructionDebugger",
        workdir: "C:/repo",
        originSessionId: null,
        originProjectId: { bad: true },
      },
      {
        id: "tab-j",
        kind: "diffPreview",
        changeType: "edit",
        diff: "@@ -1 +1 @@",
        diffMessageId: "diff-message-b",
        filePath: "src/main.rs",
        originSessionId: null,
        originProjectId: ["bad"],
        summary: "Preview diff",
      },
    ] satisfies Array<Record<string, unknown>>;

    for (const [index, tab] of invalidTabs.entries()) {
      expect(
        isWorkspaceTabDragChannelMessage({
          type: "drag-start",
          payload: {
            dragId: `drag-${index + 1}`,
            sourceWindowId: "window-a",
            sourcePaneId: "pane-a",
            tabId: String(tab.id),
            tab,
          },
        }),
      ).toBe(false);
    }
  });
});
