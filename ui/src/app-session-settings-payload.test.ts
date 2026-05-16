import { describe, expect, it } from "vitest";
import { buildSessionSettingsPayload } from "./app-session-settings-payload";
import type { Session } from "./types";

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
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

describe("app session settings payload helpers", () => {
  it("builds full Codex payloads with normalized model-dependent defaults", () => {
    const session = makeSession({
      agent: "Codex",
      approvalPolicy: "on-request",
      reasoningEffort: "high",
      sandboxMode: "read-only",
    });

    expect(buildSessionSettingsPayload(session, "model", " gpt-5.5 ")).toEqual({
      model: "gpt-5.5",
      reasoningEffort: "high",
      sandboxMode: "read-only",
      approvalPolicy: "on-request",
    });
    expect(buildSessionSettingsPayload(session, "sandboxMode", "danger-full-access")).toEqual({
      reasoningEffort: "high",
      sandboxMode: "danger-full-access",
      approvalPolicy: "on-request",
    });
    expect(buildSessionSettingsPayload(session, "approvalPolicy", "never")).toEqual({
      reasoningEffort: "high",
      sandboxMode: "read-only",
      approvalPolicy: "never",
    });
    expect(buildSessionSettingsPayload(session, "reasoningEffort", "medium")).toEqual({
      reasoningEffort: "medium",
      sandboxMode: "read-only",
      approvalPolicy: "on-request",
    });
  });

  it("preserves Codex fallbacks and non-Codex-field quirk", () => {
    const session = makeSession({
      agent: "Codex",
      approvalPolicy: undefined,
      reasoningEffort: undefined,
      sandboxMode: undefined,
    });

    expect(buildSessionSettingsPayload(session, "claudeApprovalMode", "ask")).toEqual({
      reasoningEffort: "medium",
      sandboxMode: "workspace-write",
      approvalPolicy: "never",
    });
  });

  it("builds Cursor settings payloads only for supported fields", () => {
    const session = makeSession({ agent: "Cursor", model: "auto" });

    expect(buildSessionSettingsPayload(session, "model", "cursor-fast")).toEqual({
      model: "cursor-fast",
    });
    expect(buildSessionSettingsPayload(session, "cursorMode", "plan")).toEqual({
      cursorMode: "plan",
    });
    expect(buildSessionSettingsPayload(session, "claudeEffort", "high")).toBeNull();
  });

  it("builds Claude settings payloads only for supported fields", () => {
    const session = makeSession({ agent: "Claude", model: "default" });

    expect(buildSessionSettingsPayload(session, "model", "sonnet")).toEqual({
      model: "sonnet",
    });
    expect(buildSessionSettingsPayload(session, "claudeApprovalMode", "plan")).toEqual({
      claudeApprovalMode: "plan",
    });
    expect(buildSessionSettingsPayload(session, "claudeEffort", "max")).toEqual({
      claudeEffort: "max",
    });
    expect(buildSessionSettingsPayload(session, "cursorMode", "ask")).toBeNull();
  });

  it("builds Gemini settings payloads only for supported fields", () => {
    const session = makeSession({ agent: "Gemini", model: "auto" });

    expect(buildSessionSettingsPayload(session, "model", "gemini-2.5-pro")).toEqual({
      model: "gemini-2.5-pro",
    });
    expect(buildSessionSettingsPayload(session, "geminiApprovalMode", "yolo")).toEqual({
      geminiApprovalMode: "yolo",
    });
    expect(buildSessionSettingsPayload(session, "claudeApprovalMode", "ask")).toBeNull();
  });
});
