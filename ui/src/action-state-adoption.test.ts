import { describe, expect, it } from "vitest";

import {
  classifyRejectedActionState,
  staleSessionActionTargetEvidenceExists,
} from "./action-state-adoption";
import type { StateResponse } from "./api";
import type { Session } from "./types";

function makeSession(id: string, overrides: Partial<Session> = {}): Session {
  return {
    id,
    name: "Session",
    emoji: "S",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.4",
    status: "idle",
    preview: "Ready",
    messages: [],
    ...overrides,
  };
}

function makeStateResponse(
  revision: number,
  sessions: Session[] = [],
): StateResponse {
  return {
    revision,
    serverInstanceId: "server-a",
    codex: {
      rateLimits: [],
      notices: [],
    },
    agentReadiness: [],
    preferences: {
      defaultCodexModel: "default",
      defaultClaudeModel: "default",
      defaultCursorModel: "default",
      defaultGeminiModel: "default",
      defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
    },
    projects: [],
    orchestrators: [],
    workspaces: [],
    sessions,
  } as StateResponse;
}

describe("stale action target evidence", () => {
  it("accepts a current session mutation stamp at least as new as the response", () => {
    expect(
      staleSessionActionTargetEvidenceExists({
        currentSessions: [
          makeSession("session-1", { sessionMutationStamp: 3 }),
        ],
        sessionId: "session-1",
        staleSuccessSessionEvidence: undefined,
        state: makeStateResponse(4, [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ]),
      }),
    ).toBe(true);
  });

  it("rejects a current session mutation stamp older than the response", () => {
    expect(
      staleSessionActionTargetEvidenceExists({
        currentSessions: [
          makeSession("session-1", { sessionMutationStamp: 1 }),
        ],
        sessionId: "session-1",
        staleSuccessSessionEvidence: undefined,
        state: makeStateResponse(4, [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ]),
      }),
    ).toBe(false);
  });

  it("uses preserved pre-optimistic evidence for settings actions", () => {
    expect(
      staleSessionActionTargetEvidenceExists({
        currentSessions: [
          makeSession("session-1", { sessionMutationStamp: 1 }),
        ],
        sessionId: "session-1",
        staleSuccessSessionEvidence: makeSession("session-1", {
          sessionMutationStamp: 2,
        }),
        state: makeStateResponse(4, [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ]),
      }),
    ).toBe(true);
  });

  it("accepts deletion-style evidence only when the target is already absent locally", () => {
    const stateWithoutTarget = makeStateResponse(4, []);

    expect(
      staleSessionActionTargetEvidenceExists({
        currentSessions: [],
        sessionId: "session-1",
        staleSuccessSessionEvidence: undefined,
        state: stateWithoutTarget,
      }),
    ).toBe(true);
    expect(
      staleSessionActionTargetEvidenceExists({
        currentSessions: [makeSession("session-1")],
        sessionId: "session-1",
        staleSuccessSessionEvidence: undefined,
        state: stateWithoutTarget,
      }),
    ).toBe(false);
  });
});

describe("rejected action state classification", () => {
  it("reports stale success when same-instance rejection has session evidence", () => {
    expect(
      classifyRejectedActionState({
        currentProjects: new Map(),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        currentSessions: [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ],
        options: {
          staleSuccessSessionId: "session-1",
        },
        state: makeStateResponse(4, [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ]),
      }),
    ).toBe("stale-success");
  });

  it("reports stale success when same-instance project creation already exists locally", () => {
    expect(
      classifyRejectedActionState({
        currentProjects: new Map([["project-1", true]]),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        currentSessions: [],
        options: {
          staleSuccessProjectId: "project-1",
        },
        state: makeStateResponse(4),
      }),
    ).toBe("stale-success");
  });

  it("recovers when same-instance project creation is missing local evidence", () => {
    expect(
      classifyRejectedActionState({
        currentProjects: new Map(),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        currentSessions: [],
        options: {
          staleSuccessProjectId: "project-1",
        },
        state: makeStateResponse(4),
      }),
    ).toBe("recovering");
  });

  it("recovers when global revision is stale but target evidence is missing", () => {
    expect(
      classifyRejectedActionState({
        currentProjects: new Map(),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        currentSessions: [
          makeSession("session-1", { sessionMutationStamp: 1 }),
        ],
        options: {
          staleSuccessSessionId: "session-1",
        },
        state: makeStateResponse(4, [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ]),
      }),
    ).toBe("recovering");
  });

  it("recovers when response is not a stale same-instance snapshot", () => {
    expect(
      classifyRejectedActionState({
        currentProjects: new Map(),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        currentSessions: [
          makeSession("session-1", { sessionMutationStamp: 2 }),
        ],
        options: {
          staleSuccessSessionId: "session-1",
        },
        state: {
          ...makeStateResponse(4, [
            makeSession("session-1", { sessionMutationStamp: 2 }),
          ]),
          serverInstanceId: "server-b",
        },
      }),
    ).toBe("recovering");
  });
});
