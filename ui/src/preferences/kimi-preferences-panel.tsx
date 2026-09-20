// Kimi startup preferences. Owns default model selection and setup guidance;
// not credentials or live ACP settings. Uses the shared agent model control.
import { useMemo } from "react";
import type { Session } from "../types";
import { AgentDefaultModelControl, defaultModelOptionsFromSessions } from "./agent-default-model-control";

export function KimiPreferencesPanel({ model, onSelectModel, sessions }: {
  model: string;
  onSelectModel: (value: string) => void;
  sessions: readonly Session[];
}) {
  const options = useMemo(
    () => defaultModelOptionsFromSessions("Kimi", sessions, model).options,
    [sessions, model],
  );
  return (
    <div className="settings-panel-stack">
      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Kimi startup settings</h3>
        <AgentDefaultModelControl agent="Kimi" id="default-kimi-model"
          value={model} onChange={onSelectModel} modelOptions={options} />
        <p className="session-control-hint">
          Install Kimi Code CLI and run <code>kimi login</code> in a terminal.
          Default lets Kimi choose its configured model; live choices arrive after connecting.
          Tool approvals remain manual. TermAl does not change your Kimi credentials.
        </p>
      </article>
    </div>
  );
}
