import { describe, expect, it } from "vitest";
import { resolveSessionPaneScrollStateKey } from "./SessionPaneView.scroll-key";
import type { WorkspaceTab } from "./workspace";

describe("resolveSessionPaneScrollStateKey", () => {
  it("uses stable panel-specific keys for non-session tabs", () => {
    const cases: Array<[WorkspaceTab, string]> = [
      [
        {
          id: "source-1",
          kind: "source",
          path: "/repo/src/main.ts",
          originSessionId: null,
        },
        "pane-1:source:/repo/src/main.ts",
      ],
      [
        { id: "canvas-1", kind: "canvas", cards: [], originSessionId: null },
        "pane-1:canvas:canvas-1",
      ],
      [
        { id: "orch-1", kind: "orchestratorCanvas", originSessionId: null },
        "pane-1:orchestratorCanvas:orch-1",
      ],
      [
        {
          id: "files-1",
          kind: "filesystem",
          rootPath: "/repo",
          originSessionId: null,
        },
        "pane-1:filesystem:/repo",
      ],
      [
        {
          id: "git-1",
          kind: "gitStatus",
          workdir: "/repo",
          originSessionId: null,
        },
        "pane-1:gitStatus:/repo",
      ],
      [
        {
          id: "terminal-1",
          kind: "terminal",
          workdir: "/repo",
          originSessionId: null,
        },
        "pane-1:terminal:terminal-1",
      ],
      [
        {
          id: "debug-1",
          kind: "instructionDebugger",
          workdir: "/repo",
          originSessionId: "session-2",
        },
        "pane-1:instructionDebugger:session-2",
      ],
      [
        {
          id: "diff-1",
          kind: "diffPreview",
          changeType: "edit",
          diff: "",
          diffMessageId: "message-1",
          filePath: null,
          originSessionId: null,
          summary: "",
        },
        "pane-1:diffPreview:message-1",
      ],
    ];

    for (const [tab, expected] of cases) {
      expect(
        resolveSessionPaneScrollStateKey("pane-1", "session", "session-1", tab),
      ).toBe(expected);
    }
  });

  it("falls back to view mode and active session when the tab has no custom key", () => {
    expect(
      resolveSessionPaneScrollStateKey("pane-1", "session", "session-1", {
        id: "tab-1",
        kind: "session",
        sessionId: "session-1",
      }),
    ).toBe("pane-1:session:session-1");

    expect(
      resolveSessionPaneScrollStateKey("pane-1", "commands", null, null),
    ).toBe("pane-1:commands:empty");
  });

  it("preserves empty sentinels for nullable path-based tabs", () => {
    expect(
      resolveSessionPaneScrollStateKey("pane-1", "source", null, {
        id: "source-1",
        kind: "source",
        path: null,
        originSessionId: null,
      }),
    ).toBe("pane-1:source:empty");

    expect(
      resolveSessionPaneScrollStateKey("pane-1", "instructionDebugger", null, {
        id: "debug-1",
        kind: "instructionDebugger",
        workdir: null,
        originSessionId: null,
      }),
    ).toBe("pane-1:instructionDebugger:empty");
  });
});
