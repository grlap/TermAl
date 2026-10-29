import { describe, expect, it } from "vitest";

import {
  countSessionsByFilter,
  countWorkingBreakdown,
  filterSessionListVisibleSessions,
  filterSessionsByListFilter,
} from "./session-list-filter";
import { sessionWorkActivity, sessionWorkLabel } from "./session-work-activity";
import { NO_RUNNING_RUNS_BY_OWNER, runningRunsByOwner } from "./test-runs";
import { makeTestRun } from "./test-runs-fixtures";
import type { Session } from "./types";

function createSession(
  id: string,
  status: Session["status"],
  overrides: Partial<Session> = {},
): Session {
  return {
    id,
    name: `Session ${id}`,
    emoji: "T",
    agent: "Codex",
    workdir: "C:/repo",
    model: "gpt-5",
    status,
    preview: "Preview",
    messages: [],
    ...overrides,
  };
}

describe("session list filters", () => {
  const sessions = [
    createSession("active", "active"),
    createSession("stopping", "stopping"),
    createSession("approval", "approval"),
    createSession("idle", "idle"),
    createSession("error", "error"),
  ];

  it("reports zero errors when the list is empty or contains only other statuses", () => {
    expect(countSessionsByFilter([])).toEqual({
      all: 0, working: 0, asking: 0, completed: 0, error: 0,
    });
    expect(countSessionsByFilter(sessions.slice(0, 4)).error).toBe(0);
    expect(filterSessionsByListFilter(sessions.slice(0, 4), "error")).toEqual([]);
  });

  it("counts sessions by requested filter buckets", () => {
    expect(countSessionsByFilter(sessions)).toEqual({
      all: 5,
      working: 2,
      asking: 1,
      completed: 1,
      error: 1,
    });
  });

  it("keeps error sessions in no-filter results and their own status bucket", () => {
    expect(filterSessionsByListFilter(sessions, "all").map((session) => session.id)).toEqual([
      "active",
      "stopping",
      "approval",
      "idle",
      "error",
    ]);
    expect(filterSessionsByListFilter(sessions, "working").map((session) => session.id)).toEqual([
      "active",
      "stopping",
    ]);
    expect(filterSessionsByListFilter(sessions, "asking").map((session) => session.id)).toEqual([
      "approval",
    ]);
    expect(
      filterSessionsByListFilter(sessions, "completed").map((session) => session.id),
    ).toEqual(["idle"]);
    expect(
      filterSessionsByListFilter(sessions, "error").map((session) => session.id),
    ).toEqual(["error"]);
  });

  describe("with owned running launcher work", () => {
    const runs = (...list: Parameters<typeof makeTestRun>[0][]) => runningRunsByOwner(list.map(makeTestRun));
    const ids = (list: Session[]) => list.map((session) => session.id);
    // The four cases: a foreground turn only, background work only, both,
    // and neither.
    const four = [
      createSession("fg-only", "active"),
      createSession("bg-only", "idle"),
      createSession("both", "active"),
      createSession("neither", "idle"),
    ];
    const fourRuns = runs(
      { runId: "bg", ownerSessionId: "bg-only", detached: true },
      { runId: "both-bg", ownerSessionId: "both", detached: true },
    );

    it("counts a session busy in the background as working, never completed", () => {
      expect(countSessionsByFilter(four, fourRuns)).toEqual({
        all: 4, working: 3, asking: 0, completed: 1, error: 0,
      });
      expect(ids(filterSessionsByListFilter(four, "working", fourRuns))).toEqual(["fg-only", "bg-only", "both"]);
      expect(ids(filterSessionsByListFilter(four, "completed", fourRuns))).toEqual(["neither"]);
      expect(countWorkingBreakdown(four, fourRuns)).toEqual({ foreground: 2, background: 2 });
    });

    it("labels each row with the kind of work keeping it busy", () => {
      expect(four.map((session) => sessionWorkLabel(sessionWorkActivity(session, fourRuns)))).toEqual([
        "Foreground turn", "Background work", "Foreground turn + background work", null,
      ]);
    });

    it("counts a session once whatever the number of its runs", () => {
      const many = runs(
        { runId: "a", ownerSessionId: "bg-only", detached: true },
        { runId: "b", ownerSessionId: "bg-only", detached: false },
        { runId: "c", ownerSessionId: "bg-only", detached: null },
      );
      expect(countSessionsByFilter([createSession("bg-only", "idle")], many).working).toBe(1);
    });

    it("never makes a notification recipient, an unowned run or a finished run busy", () => {
      const notOwned = runs(
        { runId: "notify", ownerSessionId: "someone-else", notifySessionId: "neither" },
        { runId: "unowned", ownerSessionId: null, notifySessionId: null },
        { runId: "passed", ownerSessionId: "neither", state: "passed" },
        { runId: "failed", ownerSessionId: "neither", state: "failed" },
        { runId: "unknown", ownerSessionId: "neither", state: "unknown" },
      );
      expect(countSessionsByFilter([createSession("neither", "idle")], notOwned)).toEqual({
        all: 1, working: 0, asking: 0, completed: 1, error: 0,
      });
    });

    it("treats a run the turn itself started in the foreground as part of that turn", () => {
      const session = createSession("turn", "active");
      const ownRun = runs({ runId: "fg", ownerSessionId: "turn", detached: false });
      expect(sessionWorkActivity(session, ownRun)).toEqual({ foreground: true, background: false, modeUnknown: false });
      // Without a turn, the same run is background work.
      expect(sessionWorkActivity(createSession("turn", "idle"), ownRun).background).toBe(true);
      // During a turn, a run whose detach mode the host did not observe says so.
      const unknownMode = runs({ runId: "seen", ownerSessionId: "turn", detached: null, registrationState: "hostObserved" });
      expect(sessionWorkLabel(sessionWorkActivity(session, unknownMode))).toBe("Foreground turn + running work (mode unknown)");
      // Only a host-observed run lacks a known mode, as for the pane's marker:
      // a registered, discovered or unlabelled run without one is background.
      for (const registrationState of ["registered", "discovered", undefined] as const) {
        const nullMode = runs({ runId: "null", ownerSessionId: "turn", detached: null, registrationState });
        expect(sessionWorkLabel(sessionWorkActivity(session, nullMode))).toBe("Foreground turn + background work");
      }
    });

    it("keeps approval and error in their buckets while also counting their background work", () => {
      const busy = runs(
        { runId: "x", ownerSessionId: "asking", detached: true },
        { runId: "y", ownerSessionId: "failed", detached: true },
      );
      expect(countSessionsByFilter(
        [createSession("asking", "approval"), createSession("failed", "error")], busy,
      )).toEqual({ all: 2, working: 2, asking: 1, completed: 0, error: 1 });
    });

    it("indexes running owned runs once and returns the shared empty index otherwise", () => {
      expect(runningRunsByOwner([])).toBe(NO_RUNNING_RUNS_BY_OWNER);
      expect(runningRunsByOwner([makeTestRun({ state: "passed" })])).toBe(NO_RUNNING_RUNS_BY_OWNER);
      expect(runningRunsByOwner([makeTestRun()]).get("session-owner")?.map((run) => run.runId)).toEqual(["test-one"]);
    });
  });

  it("omits delegated child sessions from default session lists", () => {
    const visibleSessions = filterSessionListVisibleSessions([
      createSession("parent", "idle"),
      createSession("child", "idle", { parentDelegationId: "delegation-1" }),
      createSession("empty-parent-id", "idle", { parentDelegationId: "" }),
    ]);

    expect(visibleSessions.map((session) => session.id)).toEqual([
      "parent",
      "empty-parent-id",
    ]);
  });
});
