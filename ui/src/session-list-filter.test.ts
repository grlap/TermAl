import { describe, expect, it } from "vitest";

import {
  countSessionsByFilter,
  filterSessionListVisibleSessions,
  filterSessionsByListFilter,
} from "./session-list-filter";
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
