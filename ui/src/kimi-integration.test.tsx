// Kimi's UI identity, default-model and session-setting integration boundaries.
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AgentIcon } from "./agent-icon";
import { NEW_SESSION_AGENT_OPTIONS } from "./app-shell-internals";
import { requestedModelForNewSession } from "./app-session-model-requests";
import { buildSessionSettingsPayload } from "./app-session-settings-payload";
import { buildOptimisticSessionSettingsUpdate } from "./app-session-settings-optimism";
import { createComposerDelegationRequest } from "./delegation-commands";
import { sessionModelChoicesForSlashCommand, supportsLiveSessionModelOptions } from "./panels/session-slash-palette";
import { AGENT_OPTIONS } from "./panels/orchestrator-template-form-options";
import { KimiPreferencesPanel } from "./preferences/kimi-preferences-panel";
import { describeSessionModelRefreshError, resolveAppPreferences, staticSessionModelOptions } from "./session-model-utils";
import type { Session } from "./types";

const session: Session = {
  id: "kimi-session", name: "Kimi", emoji: "KM", agent: "Kimi", workdir: "/tmp",
  model: "configured-model", status: "idle", preview: "Ready", messages: [],
  modelOptions: [{ value: "configured-model", label: "Configured model" }, { value: "other-model", label: "Other model" }],
};

describe("Kimi integration", () => {
  it("preserves actionable refresh failures instead of blaming authentication", () => {
    expect(describeSessionModelRefreshError("Kimi", "session is active or stopping")).toContain("finish or stop");
    expect(describeSessionModelRefreshError("Kimi", "timed out refreshing Kimi model options")).toContain("in time");
    expect(describeSessionModelRefreshError("Kimi", "did not return a result")).toContain("disconnected");
    expect(describeSessionModelRefreshError("Kimi", "authentication failed")).toContain("kimi login");
    expect(describeSessionModelRefreshError("Kimi", "unexpected runtime type")).toContain("unexpected runtime type");
    expect(describeSessionModelRefreshError("Kimi", "interactive authentication failed")).toContain("kimi login");
    expect(describeSessionModelRefreshError("Kimi", "timed out during interactive setup")).toContain("in time");
    expect(describeSessionModelRefreshError("Kimi", "inactive runtime disconnected")).toContain("inactive runtime disconnected");
  });
  it("is selectable and renders a local icon", () => {
    expect(NEW_SESSION_AGENT_OPTIONS).toContainEqual({ label: "Kimi", value: "Kimi" });
    expect(AGENT_OPTIONS).toContainEqual({ label: "Kimi", value: "Kimi" });
    const { container } = render(<AgentIcon agent="Kimi" />);
    expect(container.querySelector('[data-agent="kimi"]')).toHaveTextContent(/^K$/);
    expect(container.querySelector("img")).toBeNull();
  });

  it("keeps the CLI default and accepts a configured default", () => {
    expect(resolveAppPreferences(null).defaultKimiModel).toBe("default");
    const defaults = { Claude: "default", Codex: "default", Gemini: "default", Cursor: "default", OpenCode: "default", Kimi: "default" };
    expect(requestedModelForNewSession("Kimi", defaults)).toBeUndefined();
    expect(requestedModelForNewSession("Kimi", { ...defaults, Kimi: "other-model" })).toBe("other-model");
    expect(staticSessionModelOptions("Kimi", "auto")).toContainEqual({ label: "Auto", value: "auto" });
  });

  it("uses live /model options and sends only supported settings", () => {
    expect(supportsLiveSessionModelOptions(session)).toBe(true);
    expect(sessionModelChoicesForSlashCommand(session).map(option => option.value)).toEqual(["configured-model", "other-model"]);
    expect(buildSessionSettingsPayload(session, "model", " other-model ")).toEqual({ model: "other-model" });
    expect(buildOptimisticSessionSettingsUpdate(session, "model", " other-model ")?.model).toBe("other-model");
    expect(buildSessionSettingsPayload(session, "geminiApprovalMode", "yolo")).toBeNull();
  });

  it("offers setup guidance without requesting credentials", () => {
    render(<KimiPreferencesPanel model="default" onSelectModel={vi.fn()} sessions={[session]} />);
    expect(screen.getByRole("heading", { name: "Kimi startup settings" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Kimi default model" })).toBeInTheDocument();
    expect(screen.getByText("kimi login")).toBeInTheDocument();
  });

  it("does not advertise unsupported reviewer or shared read-only delegation", () => {
    const request = createComposerDelegationRequest(session, "Investigate the task");
    expect(request.agent).toBe("Kimi");
    expect(request.mode).toBe("explorer");
    expect(request.writePolicy).toEqual({ kind: "isolatedWorktree", ownedPaths: [] });
  });
});
