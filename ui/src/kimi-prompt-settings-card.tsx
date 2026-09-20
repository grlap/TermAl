// Model-only session controls for Kimi. Approval/mode policy belongs to the
// host ACP admission path, not to this card or the app-default preferences.
import { ThemedCombobox } from "./preferences/themed-combobox";
import {
  SessionModelRefreshAction,
  SessionModelRefreshFeedback,
  useSessionModelOptionsAutoRefresh,
} from "./prompt-settings-cards";
import { sessionModelComboboxOptions } from "./session-model-utils";
import type {
  Session,
  SessionModelOptionsRefreshRequest,
  SessionSettingsField,
  SessionSettingsValue,
} from "./types";

export function KimiPromptSettingsCard({
  paneId,
  session,
  isUpdating,
  isRefreshingModelOptions,
  isEngramMcpRevocationPending = false,
  modelOptionsError,
  onRequestModelOptions,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  isEngramMcpRevocationPending?: boolean;
  modelOptionsError: string | null;
  onRequestModelOptions: SessionModelOptionsRefreshRequest;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  useSessionModelOptionsAutoRefresh({
    isEngramMcpRevocationPending,
    isRefreshingModelOptions,
    onRequestModelOptions,
    session,
  });
  const busy = ["active", "approval", "stopping"].includes(session.status);
  const disabled = isUpdating || busy || isEngramMcpRevocationPending || isRefreshingModelOptions;
  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Session Settings</div>
      <h3>Kimi session</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`kimi-model-${paneId}`}>
            Kimi model
          </label>
          <ThemedCombobox
            id={`kimi-model-${paneId}`}
            className="prompt-settings-select"
            value={session.model}
            options={sessionModelComboboxOptions(session.modelOptions, session.model)}
            disabled={disabled || !session.modelOptions?.length}
            onChange={(value) => onSessionSettingsChange(session.id, "model", value)}
          />
          <SessionModelRefreshAction
            disabled={disabled}
            isRefreshing={isRefreshingModelOptions}
            sessionId={session.id}
            onRequestModelOptions={onRequestModelOptions}
          />
          <SessionModelRefreshFeedback
            agent="Kimi"
            isRefreshing={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
          />
          {!session.modelOptions?.length ? (
            <p className="session-control-hint">
              Kimi model selection is catalog-only. Refresh models while idle to
              discover advertised choices, even if the saved model is unavailable.
              If the CLI cannot supply a catalog, resolve its setup error first.
            </p>
          ) : null}
        </div>
        <p className="session-control-hint">
          Choose an advertised model while the session is idle. Model changes restart
          the runtime on the next prompt and preserve the conversation. Refresh models
          reconnects the idle session. Tool approvals always remain manual.
        </p>
      </div>
    </article>
  );
}
