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
    createSession("approval", "approval"),
    createSession("idle", "idle"),
    createSession("error", "error"),
  ];

  it("counts sessions by requested filter buckets", () => {
    expect(countSessionsByFilter(sessions)).toEqual({
      all: 4,
      working: 1,
      asking: 1,
      completed: 1,
    });
  });

  it("keeps error sessions in no-filter results but excludes them from status buckets", () => {
    expect(filterSessionsByListFilter(sessions, "all").map((session) => session.id)).toEqual([
      "active",
      "approval",
      "idle",
      "error",
    ]);
    expect(filterSessionsByListFilter(sessions, "working").map((session) => session.id)).toEqual([
      "active",
    ]);
    expect(filterSessionsByListFilter(sessions, "asking").map((session) => session.id)).toEqual([
      "approval",
    ]);
    expect(
      filterSessionsByListFilter(sessions, "completed").map((session) => session.id),
    ).toEqual(["idle"]);
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
