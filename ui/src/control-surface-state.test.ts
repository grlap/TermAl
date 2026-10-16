import { describe, expect, it } from "vitest";

import {
  buildControlSurfaceSessionListEntries,
  buildControlSurfaceSessionListState,
  createControlPanelSectionLauncherTab,
  resolveWorkspaceScopedSessionId,
} from "./control-surface-state";
import type { Project, Session } from "./types";

function createSession(overrides: Partial<Session>): Session {
  return {
    id: "session-1",
    name: "Session",
    emoji: "S",
    agent: "Codex",
    workdir: "/repo",
    model: "gpt-5",
    status: "idle",
    preview: "Preview",
    messages: [],
    ...overrides,
  };
}

describe("createControlPanelSectionLauncherTab", () => {
  it("does not create a launcher tab for Workspaces", () => {
    expect(createControlPanelSectionLauncherTab("workspaces", {
      filesystemRoot: "/tmp",
      gitWorkdir: "/tmp",
      originProjectId: "project-1",
      originSessionId: "session-1",
    })).toBeNull();
  });
});

describe("buildControlSurfaceSessionListState", () => {
  it("counts and filters errors within the visible project scope before search", () => {
    const project = { id: "project-1", name: "Project", rootPath: "/repo" };
    const sessions = [
      createSession({ id: "refused", name: "Provider refused", projectId: project.id, status: "error" }),
      createSession({ id: "host-error", name: "Host failed", projectId: project.id, status: "error" }),
      createSession({ id: "idle", projectId: project.id }),
      createSession({ id: "other-project", projectId: "project-2", status: "error" }),
      createSession({ id: "child", projectId: project.id, status: "error", parentDelegationId: "delegation-1" }),
    ];
    const state = buildControlSurfaceSessionListState(sessions, project, "error", "");
    expect(state.filteredSessions.map(({ id }) => id)).toEqual(["refused", "host-error"]);
    expect(state.sessionFilterCounts).toEqual({
      all: 3, working: 0, asking: 0, completed: 1, error: 2,
    });
    const searched = buildControlSurfaceSessionListState(sessions, project, "error", "Provider");
    expect(searched.filteredSessions.map(({ id }) => id)).toEqual(["refused"]);
    expect(searched.sessionFilterCounts).toEqual(state.sessionFilterCounts);
  });

  it("separates definite session-list matches from unloaded transcript state", () => {
    const project: Project = {
      id: "project-1",
      name: "Project",
      rootPath: "/repo",
    };
    const definiteMatch = createSession({
      id: "definite",
      name: "Release automation",
      projectId: project.id,
      preview: "Ready",
    });
    const unloadedTranscript = createSession({
      id: "unloaded",
      name: "Unloaded transcript",
      projectId: project.id,
      messagesLoaded: false,
      preview: "Metadata only",
    });
    const nonMatch = createSession({
      id: "other",
      name: "Other work",
      projectId: project.id,
      preview: "Ready",
    });

    const state = buildControlSurfaceSessionListState(
      [definiteMatch, unloadedTranscript, nonMatch],
      project,
      "all",
      "release",
    );

    expect(state.filteredSessions.map((session) => session.id)).toEqual([
      "definite",
      "unloaded",
    ]);
    expect(state.sessionListSearchResults.get("definite")).toEqual({
      hasMatch: true,
      matchCount: 1,
      snippet: "Release automation",
      transcriptIncomplete: false,
    });
    expect(state.sessionListSearchResults.get("unloaded")).toEqual({
      hasMatch: false,
      matchCount: 0,
      snippet: "Transcript not loaded",
      transcriptIncomplete: true,
    });
    expect(state.sessionListSearchResults.has("other")).toBe(false);
  });
});

describe("buildControlSurfaceSessionListEntries", () => {
  it("omits delegated child sessions even when given raw session state", () => {
    const parent = createSession({
      id: "parent",
      name: "Parent",
    });
    const delegatedChild = createSession({
      id: "reviewer-child",
      name: "Review docs - Codex",
      parentDelegationId: "delegation-1",
    });

    const entries = buildControlSurfaceSessionListEntries(
      [delegatedChild, parent],
      [],
    );

    expect(entries).toEqual([{ kind: "session", session: parent }]);
  });
});

describe("resolveWorkspaceScopedSessionId", () => {
  it("does not choose delegated child sessions as project fallback", () => {
    const projectId = "project-1";
    const delegatedChild = createSession({
      id: "reviewer-child",
      projectId,
      parentDelegationId: "delegation-1",
    });
    const visibleSession = createSession({
      id: "visible-session",
      projectId,
    });
    const sessions = [delegatedChild, visibleSession];
    const sessionLookup = new Map(
      sessions.map((session) => [session.id, session]),
    );

    expect(
      resolveWorkspaceScopedSessionId(
        projectId,
        null,
        null,
        sessions,
        sessionLookup,
      ),
    ).toBe("visible-session");
  });

  it("returns null when only delegated child sessions match the project fallback", () => {
    const projectId = "project-1";
    const delegatedChild = createSession({
      id: "reviewer-child",
      projectId,
      parentDelegationId: "delegation-1",
    });
    const sessions = [delegatedChild];
    const sessionLookup = new Map(
      sessions.map((session) => [session.id, session]),
    );

    expect(
      resolveWorkspaceScopedSessionId(
        projectId,
        null,
        null,
        sessions,
        sessionLookup,
      ),
    ).toBeNull();
  });
});
