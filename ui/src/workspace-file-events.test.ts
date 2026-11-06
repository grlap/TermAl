import { describe, expect, it } from "vitest";

import {
  mergeWorkspaceFilesChangedEvents,
  normalizeWorkspaceFileEventPath,
  workspaceFileChangeMatchesCandidate,
  workspaceFileChangeMatchesScope,
  workspaceFilesChangedEventChangeForPath,
  workspaceFilesChangedEventTouchesGitDiffTab,
  workspaceFilesChangedEventTouchesRoot,
  workspacePathContains,
} from "./workspace-file-events";
import type { WorkspaceDiffPreviewTab } from "./workspace-types";
import type {
  WorkspaceFileChange,
  WorkspaceFilesChangedEvent,
} from "./types";

function change(
  path: string,
  overrides: Partial<WorkspaceFileChange> = {},
): WorkspaceFileChange {
  return {
    path,
    kind: "modified",
    ...overrides,
  };
}

describe("workspace file events", () => {
  it("resolves relative exact deletions against the source and Git diff roots", () => {
    const event: WorkspaceFilesChangedEvent = { revision: 1, changes: [change("/repo-b/src/a.ts", { kind: "deleted" })] };
    expect(workspaceFilesChangedEventChangeForPath(event, "src/a.ts", { rootPath: "/repo-a" })).toBeNull();
    expect(workspaceFilesChangedEventChangeForPath(event, "src/a.ts", { rootPath: "/repo-b" })?.kind).toBe("deleted");
    const tab: WorkspaceDiffPreviewTab = {
      id: "tab", kind: "diffPreview", changeType: "edit", diff: "", diffMessageId: "message", summary: "Source",
      filePath: "src/a.ts", displayPath: "src/a.ts", originSessionId: null,
      gitDiffRequest: { sectionId: "unstaged", workdir: "/repo-a", path: "src/a.ts" },
    };
    expect(workspaceFilesChangedEventTouchesGitDiffTab(event, tab)).toBe(false);
    expect(workspaceFilesChangedEventTouchesGitDiffTab(event, { ...tab, gitDiffRequest: { ...tab.gitDiffRequest!, workdir: "/repo-b" } })).toBe(true);
  });

  it("merges wide exact bursts and removes only hints covered by a marker", () => {
    const current = { revision: 1, changes: Array.from({ length: 4000 }, (_, i) => change(`/repo/src/f${i}`)) };
    const next = { revision: 2, changes: [change("/repo/src", { kind: "subtree" }), change("/other/src/f1", { kind: "deleted" })] };
    expect(mergeWorkspaceFilesChangedEvents(current, next).changes.map((c) => c.path)).toEqual(["/repo/src", "/other/src/f1"]);
    expect(mergeWorkspaceFilesChangedEvents(next, { revision: 3, changes: current.changes }).changes).toEqual(next.changes);
    const drive = change("C:/", { kind: "subtree" });
    expect(mergeWorkspaceFilesChangedEvents(current, { revision: 2, changes: [drive, change("C:/a")] }).changes).toEqual([...current.changes, drive]);
  });
  it("routes directory markers to covered source, diff and tree scopes", () => {
    const event: WorkspaceFilesChangedEvent = {
      revision: 1,
      changes: [change("C:/repo/src", { kind: "subtree" })],
    };
    expect(workspaceFilesChangedEventChangeForPath(event, "C:/repo/src/main.ts")?.kind).toBe("subtree");
    expect(workspaceFilesChangedEventChangeForPath(event, "C:/repo/src2/main.ts")).toBeNull();
    expect(workspaceFilesChangedEventTouchesRoot(event, "C:/repo")).toBe(true);
    expect(workspaceFilesChangedEventTouchesRoot(event, "C:/repo/src/nested")).toBe(true);
    expect(workspaceFilesChangedEventTouchesRoot(event, "C:/other")).toBe(false);
  });

  it("retains a global covering marker across buffered event merges", () => {
    const event = mergeWorkspaceFilesChangedEvents(
      { revision: 1, changes: [change("C:/repo/a")] },
      { revision: 2, changes: [change("", { kind: "subtree" })] },
    );
    expect(workspaceFilesChangedEventChangeForPath(event, "C:/another/open.ts")?.kind).toBe("subtree");
    expect(workspaceFilesChangedEventTouchesRoot(event, "C:/another")).toBe(true);
  });

  it("normalizes Windows paths, duplicate separators, and trailing slashes", () => {
    expect(normalizeWorkspaceFileEventPath(" C:\\Repo\\src\\\\main.ts ")).toBe(
      "c:/repo/src/main.ts",
    );
    expect(normalizeWorkspaceFileEventPath("C:\\Repo\\")).toBe("c:/repo");
    expect(normalizeWorkspaceFileEventPath("C:\\")).toBe("c:/");
    expect(normalizeWorkspaceFileEventPath("/repo//src/")).toBe("/repo/src");
  });

  it("matches contained paths without accepting similarly prefixed roots", () => {
    expect(workspacePathContains("/repo/", "/repo/src/main.ts")).toBe(true);
    expect(workspacePathContains("C:\\Repo", "c:/repo/src/main.ts")).toBe(true);
    expect(workspacePathContains("/repo", "/repository/src/main.ts")).toBe(false);
    expect(workspacePathContains("", "/repo/src/main.ts")).toBe(false);
  });

  it("filters changes by session and compatible root scopes", () => {
    const scopedChange = change("C:\\Repo\\src\\main.ts", {
      rootPath: "C:\\Repo\\packages\\app",
      sessionId: "session-1",
    });

    expect(
      workspaceFileChangeMatchesScope(scopedChange, {
        rootPath: "c:/repo/",
        sessionId: "session-1",
      }),
    ).toBe(true);
    expect(
      workspaceFileChangeMatchesScope(scopedChange, {
        rootPath: "C:\\Repo\\packages\\app",
        sessionId: "session-1",
      }),
    ).toBe(true);
    expect(
      workspaceFileChangeMatchesScope(scopedChange, {
        rootPath: "C:\\Other",
        sessionId: "session-1",
      }),
    ).toBe(false);
    expect(
      workspaceFileChangeMatchesScope(scopedChange, {
        rootPath: "C:\\Repo",
        sessionId: "session-2",
      }),
    ).toBe(false);
  });

  it("matches absolute paths and relative suffix candidates", () => {
    expect(
      workspaceFileChangeMatchesCandidate(
        "C:\\Repo\\src\\main.ts",
        "c:/repo/src/main.ts",
      ),
    ).toBe(true);
    expect(
      workspaceFileChangeMatchesCandidate(
        "C:\\Repo\\src\\main.ts",
        "src/main.ts",
      ),
    ).toBe(true);
    expect(
      workspaceFileChangeMatchesCandidate(
        "C:\\Repo\\src\\main.ts",
        "C:\\Other\\src\\main.ts",
      ),
    ).toBe(false);
    expect(workspaceFileChangeMatchesCandidate("/repo/src/main.ts", "")).toBe(
      false,
    );
  });

  it("returns the matching scoped change for a target path", () => {
    const event: WorkspaceFilesChangedEvent = {
      revision: 1,
      changes: [
        change("/repo/src/main.ts", {
          rootPath: "/repo",
          sessionId: "session-2",
        }),
        change("/repo/src/main.ts", {
          rootPath: "/repo",
          sessionId: "session-1",
        }),
      ],
    };

    expect(
      workspaceFilesChangedEventChangeForPath(event, "src/main.ts", {
        rootPath: "/repo",
        sessionId: "session-1",
      })?.sessionId,
    ).toBe("session-1");
    expect(
      workspaceFilesChangedEventChangeForPath(event, "src/main.ts", {
        rootPath: "/other",
        sessionId: "session-1",
      }),
    ).toBeNull();
  });

  it("detects root touches while rejecting mismatched scopes", () => {
    const event: WorkspaceFilesChangedEvent = {
      revision: 3,
      changes: [
        change("/repo/src/main.ts", {
          rootPath: "/repo",
          sessionId: "session-1",
        }),
      ],
    };

    expect(
      workspaceFilesChangedEventTouchesRoot(event, "/repo/", {
        sessionId: "session-1",
      }),
    ).toBe(true);
    expect(workspaceFilesChangedEventTouchesRoot(event, "/repo-other")).toBe(
      false,
    );
    expect(
      workspaceFilesChangedEventTouchesRoot(event, "/repo", {
        sessionId: "session-2",
      }),
    ).toBe(false);
  });

  it("matches git diff tabs through request path candidates", () => {
    const tab: WorkspaceDiffPreviewTab = {
      id: "tab-1",
      kind: "diffPreview",
      changeType: "edit",
      diff: "",
      diffMessageId: "message-1",
      filePath: "/repo/src/new.ts",
      gitDiffRequest: {
        sectionId: "unstaged",
        path: "src/new.ts",
        originalPath: "src/old.ts",
        workdir: "/repo",
      },
      originSessionId: "session-1",
      summary: "Renamed file",
    };

    expect(
      workspaceFilesChangedEventTouchesGitDiffTab(
        {
          revision: 4,
          changes: [
            change("/repo/src/old.ts", {
              rootPath: "/repo",
              sessionId: "session-1",
            }),
          ],
        },
        tab,
      ),
    ).toBe(true);
    expect(
      workspaceFilesChangedEventTouchesGitDiffTab(
        {
          revision: 5,
          changes: [
            change("/repo/src/old.ts", {
              rootPath: "/repo",
              sessionId: "session-2",
            }),
          ],
        },
        tab,
      ),
    ).toBe(false);
  });

  it("merges increasing revisions and ignores stale revisions", () => {
    const current: WorkspaceFilesChangedEvent = {
      revision: 2,
      changes: [
        change("/repo/src/main.ts", {
          kind: "deleted",
          rootPath: "/repo",
          sessionId: "session-1",
        }),
      ],
    };
    const sameRevision: WorkspaceFilesChangedEvent = {
      revision: 2,
      changes: [change("/repo/src/same-tick.ts", { kind: "created" })],
    };
    const stale: WorkspaceFilesChangedEvent = {
      revision: 1,
      changes: [change("/repo/src/stale.ts", { kind: "created" })],
    };
    const next: WorkspaceFilesChangedEvent = {
      revision: 3,
      changes: [
        change("/repo/src/main.ts", {
          kind: "created",
          rootPath: "/repo",
          sessionId: "session-1",
        }),
        change("/repo/src/other.ts", {
          kind: "modified",
          rootPath: "/repo",
          sessionId: "session-1",
        }),
        change("   ", { kind: "created" }),
      ],
    };

    const sameRevisionMerged = mergeWorkspaceFilesChangedEvents(
      current,
      sameRevision,
    );
    expect(sameRevisionMerged).toEqual({
      revision: 2,
      changes: [
        {
          path: "/repo/src/main.ts",
          kind: "deleted",
          rootPath: "/repo",
          sessionId: "session-1",
        },
        {
          path: "/repo/src/same-tick.ts",
          kind: "created",
        },
      ],
    });
    expect(mergeWorkspaceFilesChangedEvents(current, stale)).toBe(current);

    const merged = mergeWorkspaceFilesChangedEvents(current, next);
    expect(merged.revision).toBe(3);
    expect(merged.changes).toEqual([
      {
        path: "/repo/src/main.ts",
        kind: "modified",
        rootPath: "/repo",
        sessionId: "session-1",
      },
      {
        path: "/repo/src/other.ts",
        kind: "modified",
        rootPath: "/repo",
        sessionId: "session-1",
      },
    ]);
  });
});
