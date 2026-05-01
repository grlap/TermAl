import { beforeEach, describe, expect, it } from "vitest";

import {
  WORKSPACE_LAYOUT_STORAGE_KEY,
  WORKSPACE_VIEW_QUERY_PARAM,
  createWorkspaceViewId,
  deleteStoredWorkspaceLayout,
  ensureWorkspaceViewId,
  getStoredWorkspaceLayout,
  parseStoredWorkspaceLayout,
  persistWorkspaceLayout,
  type StoredWorkspaceLayout,
} from "./workspace-storage";

describe("workspace storage", () => {
  const workspaceViewId = "workspace-test";

  beforeEach(() => {
    window.localStorage.clear();
    window.history.replaceState(null, "", "/");
  });

  it("createWorkspaceViewId returns unique workspace-prefixed ids", () => {
    const first = createWorkspaceViewId();
    const second = createWorkspaceViewId();

    expect(first).toMatch(/^workspace-/);
    expect(second).toMatch(/^workspace-/);
    expect(first).not.toBe(second);
  });

  it("ensureWorkspaceViewId reuses the existing query parameter", () => {
    window.history.replaceState(
      null,
      "",
      `/?${WORKSPACE_VIEW_QUERY_PARAM}=workspace-existing`,
    );

    expect(ensureWorkspaceViewId()).toBe("workspace-existing");
    expect(
      new URL(window.location.href).searchParams.get(
        WORKSPACE_VIEW_QUERY_PARAM,
      ),
    ).toBe("workspace-existing");
  });

  it("ensureWorkspaceViewId generates and persists a query parameter when absent", () => {
    const workspaceViewId = ensureWorkspaceViewId();

    expect(workspaceViewId).toMatch(/^workspace-/);
    expect(
      new URL(window.location.href).searchParams.get(
        WORKSPACE_VIEW_QUERY_PARAM,
      ),
    ).toBe(workspaceViewId);
  });

  it("returns null when storage is empty or invalid", () => {
    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();

    window.localStorage.setItem(
      `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      "not-json",
    );
    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();

    window.localStorage.setItem(
      `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      JSON.stringify({ controlPanelSide: "up" }),
    );
    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });

  it("persists and restores a valid workspace layout", () => {
    const layout: StoredWorkspaceLayout = {
      controlPanelSide: "right",
      workspace: {
        root: {
          id: "split-1",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: {
            type: "pane",
            paneId: "pane-control",
          },
          second: {
            type: "pane",
            paneId: "pane-session",
          },
        },
        panes: [
          {
            id: "pane-control",
            tabs: [
              {
                id: "tab-control",
                kind: "controlPanel",
                originSessionId: null,
              },
              {
                id: "tab-orchestrators",
                kind: "orchestratorList",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-control",
            activeSessionId: null,
            viewMode: "controlPanel",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
          {
            id: "pane-session",
            tabs: [
              {
                id: "tab-session",
                kind: "session",
                sessionId: "session-1",
              },
              {
                id: "tab-source",
                kind: "source",
                path: "C:/repo/src/main.ts",
                originSessionId: "session-1",
              },
              {
                id: "tab-canvas",
                kind: "canvas",
                cards: [{ sessionId: "session-1", x: 160, y: 200 }],
                zoom: 1.35,
                originSessionId: "session-1",
              },
              {
                id: "tab-orchestrator-canvas",
                kind: "orchestratorCanvas",
                originSessionId: "session-1",
                originProjectId: "project-1",
                templateId: "template-1",
              },
            ],
            activeTabId: "tab-source",
            activeSessionId: "session-1",
            viewMode: "source",
            lastSessionViewMode: "session",
            sourcePath: "C:/repo/src/main.ts",
          },
        ],
        activePaneId: "pane-session",
      },
    };

    persistWorkspaceLayout(workspaceViewId, layout);

    expect(
      window.localStorage.getItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      ),
    ).not.toBeNull();
    expect(getStoredWorkspaceLayout(workspaceViewId)).toEqual(layout);
  });

  it("strips full Markdown diff document content before persisting layout", () => {
    const layout: StoredWorkspaceLayout = {
      controlPanelSide: "right",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-diff",
        },
        panes: [
          {
            id: "pane-diff",
            tabs: [
              {
                id: "tab-diff",
                kind: "diffPreview",
                changeType: "edit",
                diff: "-before\n+after",
                documentContent: {
                  before: {
                    content: "secret before",
                    source: "head",
                  },
                  after: {
                    content: "secret after",
                    source: "worktree",
                  },
                  canEdit: true,
                  isCompleteDocument: true,
                },
                diffMessageId: "diff-1",
                filePath: "/repo/README.md",
                gitDiffRequest: {
                  path: "README.md",
                  sectionId: "unstaged",
                  workdir: "/repo",
                },
                gitDiffRequestKey: "git-preview:pane-diff:/repo:unstaged::README.md",
                language: "markdown",
                originSessionId: "session-1",
                summary: "Updated README",
              },
            ],
            activeTabId: "tab-diff",
            activeSessionId: "session-1",
            viewMode: "diffPreview",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-diff",
      },
    };

    persistWorkspaceLayout(workspaceViewId, layout);

    const stored = window.localStorage.getItem(`${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`);
    expect(stored).not.toContain("secret before");
    expect(stored).not.toContain("secret after");
    const parsedTab = getStoredWorkspaceLayout(workspaceViewId)?.workspace.panes[0]?.tabs[0];
    expect(parsedTab?.id).toBe("tab-diff");
    expect(parsedTab).toEqual(
      expect.not.objectContaining({
        documentContent: expect.anything(),
      }),
    );
  });

  it("round-trips a workspace that contains an empty pane", () => {
    // `splitPane` of a single-tab pane creates an empty sibling
    // (`createPane(null, lastSessionViewMode)` → `tabs: []`,
    // `activeTabId: null`). The validator must accept that shape so
    // the layout survives a `persistWorkspaceLayout` →
    // `getStoredWorkspaceLayout` round-trip; without this case any
    // future tightening of `isWorkspacePane` that requires a non-
    // empty `tabs` array would silently invalidate every stored
    // layout that contains a fresh split.
    const layout: StoredWorkspaceLayout = {
      controlPanelSide: "left",
      workspace: {
        root: {
          id: "split-1",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: { type: "pane", paneId: "pane-control" },
          second: {
            id: "split-2",
            type: "split",
            direction: "row",
            ratio: 0.5,
            first: { type: "pane", paneId: "pane-session" },
            second: { type: "pane", paneId: "pane-empty" },
          },
        },
        panes: [
          {
            id: "pane-control",
            tabs: [
              {
                id: "tab-control",
                kind: "controlPanel",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-control",
            activeSessionId: null,
            viewMode: "controlPanel",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
          {
            id: "pane-session",
            tabs: [
              {
                id: "tab-session",
                kind: "session",
                sessionId: "session-1",
              },
            ],
            activeTabId: "tab-session",
            activeSessionId: "session-1",
            viewMode: "session",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
          {
            // The empty pane — what splitPane creates when called
            // on a single-tab pane.
            id: "pane-empty",
            tabs: [],
            activeTabId: null,
            activeSessionId: null,
            viewMode: "session",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-empty",
      },
    };

    persistWorkspaceLayout(workspaceViewId, layout);
    const restored = getStoredWorkspaceLayout(workspaceViewId);

    expect(restored).not.toBeNull();
    expect(restored).toEqual(layout);
    expect(restored?.workspace.panes).toHaveLength(3);
    expect(restored?.workspace.panes[2]?.tabs).toEqual([]);
  });

  it("removes a stored workspace layout by workspace id", () => {
    const layout: StoredWorkspaceLayout = {
      controlPanelSide: "left",
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
    };

    persistWorkspaceLayout(workspaceViewId, layout);
    deleteStoredWorkspaceLayout(workspaceViewId);

    expect(
      window.localStorage.getItem(
        `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      ),
    ).toBeNull();
    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });

  it("ignores the old global key", () => {
    const layout: StoredWorkspaceLayout = {
      controlPanelSide: "left",
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
    };

    window.localStorage.setItem(
      WORKSPACE_LAYOUT_STORAGE_KEY,
      JSON.stringify(layout),
    );

    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });

  it("parses a valid raw layout payload", () => {
    const raw = JSON.stringify({
      controlPanelSide: "left",
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
    });

    expect(parseStoredWorkspaceLayout(raw)).toEqual({
      controlPanelSide: "left",
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
    });
  });

  it("drops the retired Mermaid neo look without rejecting the layout", () => {
    const raw = JSON.stringify({
      controlPanelSide: "left",
      diagramLook: "neo",
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
    });

    expect(parseStoredWorkspaceLayout(raw)).toEqual({
      controlPanelSide: "left",
      workspace: {
        root: null,
        panes: [],
        activePaneId: null,
      },
    });
  });

  it("parses and normalizes legacy Windows workspace paths", () => {
    const legacyRoot = String.raw`\\?\C:\repo`;
    const legacyFile = String.raw`\\?\C:\repo\src\main.ts`;
    const normalizedRoot = String.raw`C:\repo`;
    const normalizedFile = String.raw`C:\repo\src\main.ts`;
    const raw = JSON.stringify({
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-source",
                kind: "source",
                path: legacyFile,
                originSessionId: "session-1",
              },
              {
                id: "tab-files",
                kind: "filesystem",
                rootPath: legacyRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-git",
                kind: "gitStatus",
                workdir: legacyRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-terminal",
                kind: "terminal",
                workdir: legacyRoot,
                originSessionId: "session-1",
                originProjectId: "project-1",
              },
              {
                id: "tab-debug",
                kind: "instructionDebugger",
                workdir: legacyRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-diff",
                kind: "diffPreview",
                changeType: "edit",
                diff: "-before\n+after",
                diffMessageId: "message-1",
                filePath: legacyFile,
                originSessionId: "session-1",
                summary: "Updated file",
              },
            ],
            activeTabId: "tab-source",
            activeSessionId: "session-1",
            viewMode: "source",
            lastSessionViewMode: "session",
            sourcePath: legacyFile,
          },
        ],
        activePaneId: "pane-a",
      },
    });

    expect(parseStoredWorkspaceLayout(raw)).toEqual({
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-source",
                kind: "source",
                path: normalizedFile,
                originSessionId: "session-1",
              },
              {
                id: "tab-files",
                kind: "filesystem",
                rootPath: normalizedRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-git",
                kind: "gitStatus",
                workdir: normalizedRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-terminal",
                kind: "terminal",
                workdir: normalizedRoot,
                originSessionId: "session-1",
                // Pin that `originProjectId` survives
                // `normalizeWorkspaceStatePaths`: the field drives
                // remote-scope resolution in `TerminalPanel` via
                // `runTerminalCommand`'s `projectId`, and the normalizer's
                // terminal branch preserves it only incidentally via the
                // `...tab` spread at `ui/src/workspace.ts` (the terminal
                // history store itself is keyed on the per-tab UUID, not
                // on `originProjectId`). A future refactor that enumerates
                // fields explicitly would silently drop this field.
                originProjectId: "project-1",
              },
              {
                id: "tab-debug",
                kind: "instructionDebugger",
                workdir: normalizedRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-diff",
                kind: "diffPreview",
                changeType: "edit",
                diff: "-before\n+after",
                diffMessageId: "message-1",
                filePath: normalizedFile,
                originSessionId: "session-1",
                summary: "Updated file",
              },
            ],
            activeTabId: "tab-source",
            activeSessionId: "session-1",
            viewMode: "source",
            lastSessionViewMode: "session",
            sourcePath: normalizedFile,
          },
        ],
        activePaneId: "pane-a",
      },
    });
  });
  it("parses and normalizes legacy Windows UNC workspace paths", () => {
    const legacyRoot = String.raw`\\?\UNC\server\share`;
    const legacyFile = String.raw`\\?\UNC\server\share\src\main.ts`;
    const normalizedRoot = String.raw`\\server\share`;
    const normalizedFile = String.raw`\\server\share\src\main.ts`;
    const raw = JSON.stringify({
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-source",
                kind: "source",
                path: legacyFile,
                originSessionId: "session-1",
              },
              {
                id: "tab-files",
                kind: "filesystem",
                rootPath: legacyRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-git",
                kind: "gitStatus",
                workdir: legacyRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-terminal",
                kind: "terminal",
                workdir: legacyRoot,
                originSessionId: "session-1",
                originProjectId: "project-1",
              },
              {
                id: "tab-debug",
                kind: "instructionDebugger",
                workdir: legacyRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-diff",
                kind: "diffPreview",
                changeType: "edit",
                diff: "-before\n+after",
                diffMessageId: "message-1",
                filePath: legacyFile,
                originSessionId: "session-1",
                summary: "Updated file",
              },
            ],
            activeTabId: "tab-source",
            activeSessionId: "session-1",
            viewMode: "source",
            lastSessionViewMode: "session",
            sourcePath: legacyFile,
          },
        ],
        activePaneId: "pane-a",
      },
    });

    expect(parseStoredWorkspaceLayout(raw)).toEqual({
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-source",
                kind: "source",
                path: normalizedFile,
                originSessionId: "session-1",
              },
              {
                id: "tab-files",
                kind: "filesystem",
                rootPath: normalizedRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-git",
                kind: "gitStatus",
                workdir: normalizedRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-terminal",
                kind: "terminal",
                workdir: normalizedRoot,
                originSessionId: "session-1",
                // See the legacy-path round-trip test above for the
                // rationale pinning `originProjectId`.
                originProjectId: "project-1",
              },
              {
                id: "tab-debug",
                kind: "instructionDebugger",
                workdir: normalizedRoot,
                originSessionId: "session-1",
              },
              {
                id: "tab-diff",
                kind: "diffPreview",
                changeType: "edit",
                diff: "-before\n+after",
                diffMessageId: "message-1",
                filePath: normalizedFile,
                originSessionId: "session-1",
                summary: "Updated file",
              },
            ],
            activeTabId: "tab-source",
            activeSessionId: "session-1",
            viewMode: "source",
            lastSessionViewMode: "session",
            sourcePath: normalizedFile,
          },
        ],
        activePaneId: "pane-a",
      },
    });
  });
  it("rejects malformed canvas tabs", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-canvas",
                kind: "canvas",
                cards: [{ sessionId: "session-1", x: "bad", y: 200 }],
                originSessionId: null,
              },
            ],
            activeTabId: "tab-canvas",
            activeSessionId: null,
            viewMode: "canvas",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-a",
      },
    };

    window.localStorage.setItem(
      `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      JSON.stringify(malformed),
    );

    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });

  it("rejects malformed canvas zoom values", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-canvas",
                kind: "canvas",
                cards: [{ sessionId: "session-1", x: 160, y: 200 }],
                zoom: "bad",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-canvas",
            activeSessionId: null,
            viewMode: "canvas",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-a",
      },
    };

    window.localStorage.setItem(
      `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      JSON.stringify(malformed),
    );

    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });

  it("rejects malformed orchestrator canvas tabs", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-orchestrator-canvas",
                kind: "orchestratorCanvas",
                originSessionId: null,
                templateId: ["bad"],
              },
            ],
            activeTabId: "tab-orchestrator-canvas",
            activeSessionId: null,
            viewMode: "orchestratorCanvas",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-a",
      },
    };

    window.localStorage.setItem(
      `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      JSON.stringify(malformed),
    );

    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });

  it("rejects malformed workspace trees", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "missing-pane",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-a",
                kind: "controlPanel",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-a",
            activeSessionId: null,
            viewMode: "controlPanel",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-a",
      },
    };

    window.localStorage.setItem(
      `${WORKSPACE_LAYOUT_STORAGE_KEY}:${workspaceViewId}`,
      JSON.stringify(malformed),
    );

    expect(getStoredWorkspaceLayout(workspaceViewId)).toBeNull();
  });
});
