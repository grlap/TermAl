// Kimi startup preferences. Owns default model, approval and effort selection;
// not credentials or live ACP settings. Uses the shared agent model control.
import { useMemo } from "react";
import type { KimiApprovalMode, Session } from "../types";
import { KIMI_APPROVAL_OPTIONS, isKimiApprovalMode } from "../kimi-settings-options";
import { ThemedCombobox } from "./themed-combobox";
import { AgentDefaultModelControl, defaultModelOptionsFromSessions } from "./agent-default-model-control";

export function KimiPreferencesPanel({ model, onSelectModel, sessions, approvalMode, onSelectApprovalMode, effort, onSelectEffort }: {
  model: string;
  onSelectModel: (value: string) => void;
  sessions: readonly Session[];
  approvalMode: KimiApprovalMode;
  onSelectApprovalMode: (value: KimiApprovalMode) => void;
  effort: string;
  onSelectEffort: (value: string) => void;
}) {
  const options = useMemo(
    () => defaultModelOptionsFromSessions("Kimi", sessions, model).options,
    [sessions, model],
  );
  const advertisedEfforts = useMemo(() => [...new Map(
    sessions.filter(session => session.agent === "Kimi")
      .flatMap(session => session.kimiEffortOptions ?? [])
      .filter(option => option.value !== "auto")
      .map(option => [option.value, option] as const),
  ).values()], [sessions]);
  const unavailableEffort = effort !== "auto" && !advertisedEfforts.some(option => option.value === effort);
  const effortOptions = [
    { value: "auto", label: "CLI current" },
    ...advertisedEfforts.map(option => ({ ...option, description: option.description ?? undefined })),
    ...(unavailableEffort ? [{ value: effort, label: `${effort} (unavailable)`, disabled: true }] : []),
  ];
  return (
    <div className="settings-panel-stack">
      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Kimi startup settings</h3>
        <AgentDefaultModelControl agent="Kimi" id="default-kimi-model"
          value={model} onChange={onSelectModel} modelOptions={options} />
        <div className="session-control-group">
          <label className="session-control-label" htmlFor="default-kimi-approval">Default TermAl approvals</label>
          <ThemedCombobox id="default-kimi-approval" value={approvalMode} options={KIMI_APPROVAL_OPTIONS}
            onChange={value => { if (isKimiApprovalMode(value)) onSelectApprovalMode(value); }} />
          <p className="session-control-hint">Auto-approve permits tool requests from Kimi without approval cards. Questions and plans remain interactive.</p>
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor="default-kimi-effort">Default reasoning effort</label>
          <ThemedCombobox id="default-kimi-effort" value={effort} options={effortOptions} onChange={onSelectEffort} />
          <p className="session-control-hint">Choices come from connected Kimi sessions. CLI current keeps the CLI's current value. Defaults apply only to new sessions.</p>
          {unavailableEffort ? <p className="session-control-hint" role="status">Saved effort {effort} is unavailable; it is kept until you select a replacement.</p> : null}
        </div>
        <p className="session-control-hint">
          Install Kimi Code CLI and run <code>kimi login</code> in a terminal.
          Default lets Kimi choose its configured model; live choices arrive after connecting.
          TermAl does not change your Kimi credentials.
        </p>
      </article>
    </div>
  );
}
