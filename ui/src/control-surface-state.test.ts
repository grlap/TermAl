import { describe, expect, it } from "vitest";

import { buildControlSurfaceSessionListState } from "./control-surface-state";
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

describe("buildControlSurfaceSessionListState", () => {
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
