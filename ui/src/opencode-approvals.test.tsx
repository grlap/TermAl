// Owns OpenCode approval configuration controls and optimistic policy transport.
// Does not own dynamic ACP model/mode discovery or other agents' controls.
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { OpenCodePreferencesPanel } from "./preferences-panels";
import { OpenCodePromptSettingsCard } from "./prompt-settings-cards";
import { buildSessionSettingsPayload } from "./app-session-settings-payload";
import { buildOptimisticSessionSettingsUpdate, rollbackOptimisticSessionSettingsUpdate } from "./app-session-settings-optimism";
import { resolveAppPreferences } from "./session-model-utils";
import type { Session } from "./types";

const session: Session = {
  id: "open-1", name: "OpenCode", emoji: "", agent: "OpenCode", workdir: "/tmp",
  model: "provider/model", modelOptions: [{value: "provider/model", label: "Model"}],
  status: "idle", preview: "Ready", messages: [],
};

describe("OpenCode approvals", () => {
  it("defaults to Ask and exposes the persisted app default control", async () => {
    expect(resolveAppPreferences(null).defaultOpenCodeApprovalMode).toBe("ask");
    const change = vi.fn();
    render(<OpenCodePreferencesPanel defaultOpenCodeModel="default" defaultOpenCodeApprovalMode="ask" onSelectModel={vi.fn()}
      onSelectApprovalMode={change} />);
    fireEvent.click(screen.getByRole("combobox", {name: "Approval mode"}));
    fireEvent.click(await screen.findByRole("option", {name: /^Auto-approve/}));
    expect(change).toHaveBeenCalledWith("auto-approve");
  });

  it("exposes a session override and prevents policy changes during a turn", async () => {
    const change = vi.fn();
    const props = {paneId: "pane-1", session, isUpdating: false, isRefreshingModelOptions: false,
      modelOptionsError: null, onRequestModelOptions: vi.fn(), onSessionSettingsChange: change};
    const view = render(<OpenCodePromptSettingsCard {...props} />);
    fireEvent.click(screen.getByRole("combobox", {name: "OpenCode approvals"}));
    fireEvent.click(await screen.findByRole("option", {name: /^Auto-approve/}));
    expect(change).toHaveBeenCalledWith("open-1", "opencodeApprovalMode", "auto-approve");
    view.rerender(<OpenCodePromptSettingsCard {...props} session={{...session, status: "active"}} />);
    expect(screen.getByRole("combobox", {name: "OpenCode approvals"})).toBeDisabled();
  });

  it("sends an approval-only patch and rolls back a failed optimistic update", () => {
    expect(buildSessionSettingsPayload(session, "opencodeApprovalMode", "auto-approve"))
      .toEqual({opencodeApprovalMode: "auto-approve"});
    const optimistic = buildOptimisticSessionSettingsUpdate(session, "opencodeApprovalMode", "auto-approve");
    expect(optimistic.opencodeApprovalMode).toBe("auto-approve");
    expect(optimistic.opencodeMode).toBeUndefined();
    expect(rollbackOptimisticSessionSettingsUpdate(optimistic, session, optimistic)).toEqual(session);
  });
});
