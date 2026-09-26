// Kimi's UI identity, default-model and session-setting integration boundaries.
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AgentIcon } from "./agent-icon";
import { NEW_SESSION_AGENT_OPTIONS } from "./app-shell-internals";
import { requestedModelForNewSession } from "./app-session-model-requests";
import { buildSessionSettingsPayload } from "./app-session-settings-payload";
import { buildOptimisticSessionSettingsUpdate, rollbackOptimisticSessionSettingsUpdate } from "./app-session-settings-optimism";
import { createComposerDelegationRequest } from "./delegation-commands";
import { kimiApprovalSlashState, sessionModeSlashState, kimiEffortSlashState, SLASH_COMMANDS, sessionModelChoicesForSlashCommand, supportsLiveSessionModelOptions } from "./panels/session-slash-palette";
import { KimiPromptSettingsCard } from "./kimi-prompt-settings-card";
import { reconcileSessions } from "./session-reconcile";
import { AGENT_OPTIONS } from "./panels/orchestrator-template-form-options";
import { KimiPreferencesPanel } from "./preferences/kimi-preferences-panel";
import { buildSessionTooltipRows } from "./panels/session-tab-status-tooltip";
import { describeSessionModelRefreshError, resolveAppPreferences, staticSessionModelOptions } from "./session-model-utils";
import type { Session } from "./types";

const session: Session = {
  id: "kimi-session", name: "Kimi", emoji: "KM", agent: "Kimi", workdir: "/tmp",
  model: "configured-model", status: "idle", preview: "Ready", messages: [],
  modelOptions: [{ value: "configured-model", label: "Configured model" }, { value: "other-model", label: "Other model" }],
};

describe("Kimi integration", () => {
  it.each([
    ["kimiApprovalMode", "auto-approve"],
    ["kimiMode", "yolo"],
  ] as const)("sends only %s and rolls back without replacing observed mode", (field, value) => {
    const previous: Session = { ...session, kimiApprovalMode: "ask", kimiMode: "default", kimiCurrentMode: "plan" };
    expect(buildSessionSettingsPayload(previous, field, value)).toEqual({ [field]: value });
    const optimistic = buildOptimisticSessionSettingsUpdate(previous, field, value)!;
    expect(optimistic[field]).toBe(value);
    expect(optimistic.kimiCurrentMode).toBe("plan");
    expect(rollbackOptimisticSessionSettingsUpdate(optimistic, previous, optimistic)[field]).toBe(previous[field]);
    const newer = { ...optimistic, [field]: previous[field], kimiCurrentMode: "auto" };
    expect(rollbackOptimisticSessionSettingsUpdate(newer, previous, optimistic)).toBe(newer);
  });

  it.each([
    { kimiApprovalMode: "auto-approve" }, { kimiMode: "yolo" }, { kimiCurrentMode: "plan" },
  ] as const)("adopts isolated Kimi field changes during state reconciliation: %j", change => {
    const next = { ...session, ...change };
    const merged = reconcileSessions([session], [next]);
    expect(merged[0]).toMatchObject(change);
    expect(merged[0]).not.toBe(session);
    expect(reconcileSessions(merged, [{ ...next }])).toBe(merged);
  });

  it("exposes independent approval and mode slash choices", () => {
    expect(SLASH_COMMANDS.filter(command => command.supports.includes("Kimi")).map(command => command.id))
      .toEqual(["model", "mode", "approvals", "effort"]);
    expect(kimiApprovalSlashState(session, "").items).toHaveLength(2);
    expect(sessionModeSlashState(session, "")?.items).toHaveLength(4);
    expect(sessionModeSlashState({ ...session, kimiMode: "auto" }, "")?.hint).toContain("rarely or never asks TermAl");
    expect(kimiApprovalSlashState({ ...session, kimiApprovalMode: "auto-approve" }, "").items)
      .toContainEqual(expect.objectContaining({ value: "auto-approve", field: "kimiApprovalMode", isCurrent: true }));
  });

  it.each(["active", "approval", "stopping"] as const)("blocks Kimi settings during %s", status => {
    const busySession = { ...session, status };
    expect(kimiApprovalSlashState(busySession, "").items).toHaveLength(0);
    expect(sessionModeSlashState(busySession, "")?.items).toHaveLength(0);
    render(<KimiPromptSettingsCard paneId="test" session={busySession} isUpdating={false}
      isRefreshingModelOptions={false} modelOptionsError={null} onRequestModelOptions={vi.fn()}
      onSessionSettingsChange={vi.fn()} />);
    for (const control of screen.getAllByRole("combobox")) expect(control).toBeDisabled();
  });

  it("changes host approvals and Kimi mode independently, with observed mode display only", async () => {
    const change = vi.fn();
    render(<KimiPromptSettingsCard paneId="test" session={{ ...session, kimiMode: "yolo", kimiCurrentMode: "plan" }}
      isUpdating={false} isRefreshingModelOptions={false} modelOptionsError={null}
      onRequestModelOptions={vi.fn()} onSessionSettingsChange={change} />);
    expect(screen.getByText("Observed CLI mode: plan")).toBeInTheDocument();
    expect(screen.getByText(/rarely or never asks TermAl/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("combobox", { name: "TermAl approvals" }));
    fireEvent.click(await screen.findByRole("option", { name: /^Auto-approve/ }));
    expect(change).toHaveBeenLastCalledWith(session.id, "kimiApprovalMode", "auto-approve");
    fireEvent.click(screen.getByRole("combobox", { name: "Kimi mode" }));
    fireEvent.click(await screen.findByRole("option", { name: /^Never ask/ }));
    expect(change).toHaveBeenLastCalledWith(session.id, "kimiMode", "auto");
  });

  it("preserves unavailable default effort and offers advertised replacements", async () => {
    const effortChange = vi.fn();
    render(<KimiPreferencesPanel model="default" onSelectModel={vi.fn()}
      sessions={[{ ...session, kimiEffortOptions: [{ value: "high", label: "Thinking High", description: null }] }]}
      approvalMode="ask" onSelectApprovalMode={vi.fn()} effort="obsolete" onSelectEffort={effortChange} />);
    expect(screen.getByRole("combobox", { name: "Default reasoning effort" })).toHaveTextContent("obsolete (unavailable)");
    expect(effortChange).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("combobox", { name: "Default reasoning effort" }));
    fireEvent.click(await screen.findByRole("option", { name: "Thinking High" }));
    expect(effortChange).toHaveBeenCalledWith("high");
  });

  it("clears model-scoped effort observations optimistically and rolls back without replacing newer observations", () => {
    const previous = { ...session, kimiEffort: "max", kimiCurrentEffort: "high",
      kimiEffortOptions: [{ value: "high", label: "High" }, { value: "max", label: "Max" }] };
    const optimistic = buildOptimisticSessionSettingsUpdate(previous, "model", "other-model")!;
    expect(optimistic.kimiEffort).toBe("max");
    expect(optimistic.kimiEffortOptions).toEqual([]);
    expect(optimistic.kimiCurrentEffort).toBeNull();
    const restored = rollbackOptimisticSessionSettingsUpdate(optimistic, previous, optimistic);
    expect(restored.kimiEffortOptions).toBe(previous.kimiEffortOptions);
    expect(restored.kimiCurrentEffort).toBe("high");
    expect(restored.model).toBe(previous.model);
    const newer = { ...optimistic, kimiCurrentEffort: "low", kimiEffortOptions: [{ value: "low", label: "Low" }] };
    const retained = rollbackOptimisticSessionSettingsUpdate(newer, previous, optimistic);
    expect(retained.kimiEffortOptions).toBe(newer.kimiEffortOptions);
    expect(retained.kimiCurrentEffort).toBe("low");
  });
  it("explains unavailable requested effort and displays observed CLI effort in the palette", () => {
    const state = kimiEffortSlashState({ ...session, kimiEffort: "max", kimiCurrentEffort: "high",
      kimiEffortOptions: [{ value: "high", label: "High" }] }, "");
    expect(state.hint).toContain('Saved effort "max" is unavailable');
    expect(state.items[0]?.label).toBe("CLI current (high)");
  });
  it("shows requested effort or the CLI observation in the session tooltip", () => {
    const rows = (kimiEffort: string | null, kimiCurrentEffort: string | null) =>
      buildSessionTooltipRows({ ...session, kimiEffort, kimiCurrentEffort }, new Map(), new Map());
    expect(rows("max", "high")).toContainEqual({ key: "Reasoning", value: "Max" });
    expect(rows(null, "high")).toContainEqual({ key: "Reasoning", value: "CLI current (High)" });
    expect(rows(null, null)).toContainEqual({ key: "Reasoning", value: "CLI current" });
  });

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
    expect(resolveAppPreferences(null).defaultKimiApprovalMode).toBe("ask");
    expect(resolveAppPreferences(null).defaultKimiEffort).toBe("auto");
    expect(resolveAppPreferences({ defaultKimiApprovalMode: "auto-approve", defaultKimiEffort: "custom" } as Parameters<typeof resolveAppPreferences>[0]))
      .toMatchObject({ defaultKimiApprovalMode: "auto-approve", defaultKimiEffort: "custom" });
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
    render(<KimiPreferencesPanel model="default" onSelectModel={vi.fn()} sessions={[session]}
      approvalMode="ask" onSelectApprovalMode={vi.fn()} effort="auto" onSelectEffort={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "Kimi startup settings" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Kimi default model" })).toBeInTheDocument();
    expect(screen.getByText("kimi login")).toBeInTheDocument();
  });

  it("routes reasoning effort through payload, optimistic rollback and slash choices", () => {
    const previous = { ...session, kimiEffort: "low", kimiCurrentEffort: "low",
      kimiEffortOptions: [{ value: "low", label: "Thinking Low" }, { value: "max", label: "Thinking Max" }] };
    expect(buildSessionSettingsPayload(previous, "kimiEffort", "max")).toEqual({ kimiEffort: "max" });
    const optimistic = buildOptimisticSessionSettingsUpdate(previous, "kimiEffort", "max")!;
    expect(optimistic.kimiEffort).toBe("max");
    expect(optimistic.kimiCurrentEffort).toBe("low");
    expect(rollbackOptimisticSessionSettingsUpdate(optimistic, previous, optimistic)?.kimiEffort).toBe("low");
    expect(SLASH_COMMANDS.find(command => command.id === "effort")?.supports).toContain("Kimi");
    expect(kimiEffortSlashState(previous, "").items).toHaveLength(3);
    expect(kimiEffortSlashState(previous, "missing").emptyMessage).toContain('No Kimi reasoning efforts match "missing"');
    expect(buildSessionSettingsPayload(previous, "kimiEffort", "auto")).toEqual({ kimiEffort: "auto" });
    expect(buildOptimisticSessionSettingsUpdate(previous, "kimiEffort", "auto")?.kimiEffort).toBeNull();
    expect(kimiEffortSlashState({ ...previous, status: "active" }, "").items).toHaveLength(0);
  });

  it("defaults Kimi composer delegation to a read-only reviewer", () => {
    const request = createComposerDelegationRequest(session, "Investigate the task");
    expect(request.agent).toBe("Kimi");
    expect(request.mode).toBe("reviewer");
    expect(request.writePolicy).toEqual({ kind: "readOnly" });
  });
});
