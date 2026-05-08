import { describe, expect, it } from "vitest";

import { createBuiltinLocalRemote } from "../remotes";
import type { SessionSummarySnapshot } from "../session-store";
import type { Project, RemoteConfig } from "../types";
import { formatSessionTooltipLocationLabel } from "./session-tab-status-tooltip";

function createSessionSummary(
  overrides: Partial<SessionSummarySnapshot> = {},
): SessionSummarySnapshot {
  return {
    agent: "Codex",
    id: "session-1",
    model: "gpt-5.4",
    name: "Delegation",
    status: "idle",
    workdir: "C:\\repo",
    ...overrides,
  };
}

function createRemote(overrides: Partial<RemoteConfig> = {}): RemoteConfig {
  return {
    id: "ssh-lab",
    name: "SSH Lab",
    transport: "ssh",
    enabled: true,
    host: "lab.internal",
    port: 2222,
    user: "alice",
    ...overrides,
  };
}

function createProject(overrides: Partial<Project> = {}): Project {
  return {
    id: "project-1",
    name: "Project",
    rootPath: "C:\\repo",
    remoteId: "local",
    ...overrides,
  };
}

describe("session tab status tooltip location", () => {
  const remoteLookup = new Map<string, RemoteConfig>([
    ["local", createBuiltinLocalRemote()],
    ["ssh-lab", createRemote()],
  ]);

  it("prefers session remote ownership over local project ownership", () => {
    const session = createSessionSummary({
      projectId: "project-1",
      remoteId: "ssh-lab",
    });
    const projectLookup = new Map([["project-1", createProject()]]);

    expect(formatSessionTooltipLocationLabel(session, projectLookup, remoteLookup)).toBe(
      "SSH Lab (alice@lab.internal:2222)",
    );
  });

  it("uses session remote ownership for projectless remote proxy sessions", () => {
    const session = createSessionSummary({
      projectId: null,
      remoteId: "ssh-lab",
    });

    expect(formatSessionTooltipLocationLabel(session, new Map(), remoteLookup)).toBe(
      "SSH Lab (alice@lab.internal:2222)",
    );
  });

  it("uses session remote ownership when project metadata is missing", () => {
    const session = createSessionSummary({
      projectId: "missing-project",
      remoteId: "ssh-lab",
    });

    expect(formatSessionTooltipLocationLabel(session, new Map(), remoteLookup)).toBe(
      "SSH Lab (alice@lab.internal:2222)",
    );
  });

  it("keeps the missing-project label when no session remote owner is available", () => {
    const session = createSessionSummary({ projectId: "missing-project" });

    expect(formatSessionTooltipLocationLabel(session, new Map(), remoteLookup)).toBe(
      "Unknown (missing project)",
    );
  });
});
