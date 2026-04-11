import {
  useEffect,
  useRef,
  useState,
} from "react";
import {
  ThemedCombobox,
  SANDBOX_MODE_OPTIONS,
  APPROVAL_POLICY_OPTIONS,
  CLAUDE_APPROVAL_OPTIONS,
  CURSOR_MODE_OPTIONS,
  GEMINI_APPROVAL_OPTIONS,
} from "./preferences-panels";
import {
  claudeEffortComboboxOptions,
  claudeEffortHint,
  codexReasoningEffortComboboxOptions,
  codexReasoningEffortHint,
  currentClaudeEffort,
  currentCodexModelOption,
  currentSessionModelOption,
  manualSessionModelPlaceholder,
  normalizedCodexReasoningEffort,
  normalizedRequestedSessionModel,
  sessionModelCapabilitySummary,
  sessionModelComboboxOptions,
  type ComboboxOption,
} from "./session-model-utils";
import { matchingSessionModelOption } from "./session-model-options";
import type {
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CursorMode,
  GeminiApprovalMode,
  SandboxMode,
  Session,
  SessionModelOption,
  SessionSettingsField,
  SessionSettingsValue,
} from "./types";

export function CodexPromptSettingsCard({
  paneId,
  session,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  sessionNotice,
  onArchiveThread,
  onCompactThread,
  onForkThread,
  onRequestModelOptions,
  onRollbackThread,
  onSessionSettingsChange,
  onUnarchiveThread,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  sessionNotice: string | null;
  onArchiveThread: (sessionId: string) => void;
  onCompactThread: (sessionId: string) => void;
  onForkThread: (sessionId: string, preferredPaneId: string | null) => void;
  onRequestModelOptions: (sessionId: string) => void;
  onRollbackThread: (sessionId: string, numTurns: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onUnarchiveThread: (sessionId: string) => void;
}) {
  const [rollbackTurnsText, setRollbackTurnsText] = useState("1");

  useSessionModelOptionsAutoRefresh({
    isRefreshingModelOptions,
    onRequestModelOptions,
    session,
  });

  const modelOptions = sessionModelComboboxOptions(session.modelOptions, session.model);
  const canChangeModel = (session.modelOptions?.length ?? 0) > 0;
  const currentModelOption = currentCodexModelOption(session);
  const reasoningEffortOptions = codexReasoningEffortComboboxOptions(session);
  const currentReasoningEffort = normalizedCodexReasoningEffort(session);
  const modelCapabilityHint = codexReasoningEffortHint(session);
  const hasLiveThread = Boolean(session.externalSessionId);
  const hasQueuedPrompts = (session.pendingPrompts?.length ?? 0) > 0;
  const threadIsArchived = hasLiveThread && session.codexThreadState === "archived";
  const sessionBusy = session.status === "active" || session.status === "approval";
  const threadActionsDisabled =
    isUpdating ||
    isRefreshingModelOptions ||
    !hasLiveThread ||
    sessionBusy ||
    hasQueuedPrompts;
  const rollbackTurns = Number.parseInt(rollbackTurnsText, 10);
  const hasValidRollbackTurns = Number.isInteger(rollbackTurns) && rollbackTurns > 0;

  useEffect(() => {
    setRollbackTurnsText("1");
  }, [session.id]);

  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Session Settings</div>
      <h3>Codex session</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`codex-model-${paneId}`}>
            Codex model
          </label>
          <ThemedCombobox
            id={`codex-model-${paneId}`}
            className="prompt-settings-select"
            value={session.model}
            options={modelOptions}
            disabled={isUpdating || !canChangeModel}
            onChange={(nextValue) => void onSessionSettingsChange(session.id, "model", nextValue)}
          />
          <SessionModelRefreshAction
            disabled={isUpdating || isRefreshingModelOptions}
            isRefreshing={isRefreshingModelOptions}
            sessionId={session.id}
            onRequestModelOptions={onRequestModelOptions}
          />
          <SessionModelRefreshFeedback
            agent={session.agent}
            isRefreshing={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
          />
          <SessionModelDetails option={currentModelOption} />
          <SessionManualModelControl
            paneId={paneId}
            session={session}
            isUpdating={isUpdating}
            onSessionSettingsChange={onSessionSettingsChange}
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`sandbox-mode-${paneId}`}>
            Next prompt sandbox
          </label>
          <ThemedCombobox
            id={`sandbox-mode-${paneId}`}
            className="prompt-settings-select"
            value={session.sandboxMode ?? "workspace-write"}
            options={SANDBOX_MODE_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(session.id, "sandboxMode", nextValue as SandboxMode)
            }
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`approval-policy-${paneId}`}>
            Next prompt approval
          </label>
          <ThemedCombobox
            id={`approval-policy-${paneId}`}
            className="prompt-settings-select"
            value={session.approvalPolicy ?? "never"}
            options={APPROVAL_POLICY_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "approvalPolicy",
                nextValue as ApprovalPolicy,
              )
            }
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`reasoning-effort-${paneId}`}>
            Reasoning effort
          </label>
          <ThemedCombobox
            id={`reasoning-effort-${paneId}`}
            className="prompt-settings-select"
            value={currentReasoningEffort}
            options={reasoningEffortOptions}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "reasoningEffort",
                nextValue as CodexReasoningEffort,
              )
            }
          />
        </div>
        <div className="session-control-group session-thread-controls">
          <div className="session-control-label">Thread actions</div>
          <div className="session-action-row">
            <button
              className="session-action-button"
              type="button"
              disabled={threadActionsDisabled}
              onClick={() => void onForkThread(session.id, paneId)}
            >
              Fork thread
            </button>
            <button
              className="session-action-button"
              type="button"
              disabled={threadActionsDisabled}
              onClick={() => void onCompactThread(session.id)}
            >
              Compact
            </button>
          </div>
          <div className="session-action-row">
            <button
              className="session-action-button"
              type="button"
              disabled={threadActionsDisabled || threadIsArchived}
              onClick={() => void onArchiveThread(session.id)}
            >
              Archive
            </button>
            <button
              className="session-action-button"
              type="button"
              disabled={threadActionsDisabled || !threadIsArchived}
              onClick={() => void onUnarchiveThread(session.id)}
            >
              Unarchive
            </button>
          </div>
          <div className="session-action-row session-action-row-rollback">
            <label className="session-control-label" htmlFor={`codex-rollback-turns-${paneId}`}>
              Roll back turns
            </label>
            <input
              id={`codex-rollback-turns-${paneId}`}
              className="themed-input session-action-input"
              type="number"
              min={1}
              step={1}
              value={rollbackTurnsText}
              disabled={isUpdating || isRefreshingModelOptions}
              onChange={(event) => setRollbackTurnsText(event.currentTarget.value)}
            />
            <button
              className="session-action-button"
              type="button"
              disabled={threadActionsDisabled || !hasValidRollbackTurns}
              onClick={() => {
                if (!hasValidRollbackTurns) {
                  return;
                }
                void onRollbackThread(session.id, rollbackTurns);
              }}
            >
              Roll back
            </button>
          </div>
          {!hasLiveThread ? (
            <p className="session-control-status">
              Thread actions unlock after the first Codex prompt creates a live thread for this session.
            </p>
          ) : hasQueuedPrompts ? (
            <p className="session-control-status">
              Wait for queued Codex prompts to finish before changing the live thread.
            </p>
          ) : sessionBusy ? (
            <p className="session-control-status">
              Wait for the current Codex turn or approval to finish before changing the live thread.
            </p>
          ) : threadIsArchived ? (
            <p className="session-control-status">
              This Codex thread is archived. Unarchive it before sending another prompt.
            </p>
          ) : (
            <p className="session-control-hint session-thread-hint">
              Fork creates a new TermAl session attached to a new Codex thread. Archive, unarchive,
              compact, and rollback act on the live Codex thread behind this session.
            </p>
          )}
        </div>
        {sessionNotice ? <p className="session-control-notice">{sessionNotice}</p> : null}
        <p className="session-control-hint">
          {isRefreshingModelOptions
            ? "Loading Codex's live model list for this session. Sandbox, approval, and reasoning changes still apply on the next Codex prompt."
            : canChangeModel
              ? "Model, sandbox, approval, and reasoning changes apply on the next Codex prompt. You can still paste a full Codex model id manually if the live list is behind."
              : "TermAl asks Codex for its live model list when this session opens. New sessions begin on Codex's default model. You can still paste a full Codex model id manually. Sandbox, approval, and reasoning changes still apply on the next Codex prompt."}
          {modelCapabilityHint ? ` ${modelCapabilityHint}` : ""}
        </p>
      </div>
    </article>
  );
}

export function ClaudePromptSettingsCard({
  paneId,
  session,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  onRequestModelOptions,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  onRequestModelOptions: (sessionId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  useSessionModelOptionsAutoRefresh({
    isRefreshingModelOptions,
    onRequestModelOptions,
    session,
  });

  const modelOptions = sessionModelComboboxOptions(session.modelOptions, session.model);
  const currentModelOption = currentSessionModelOption(session);
  const currentClaudeEffortValue = currentClaudeEffort(session);
  const claudeEffortOptions = claudeEffortComboboxOptions(session);
  const modelCapabilityHint = claudeEffortHint(session);

  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Session Settings</div>
      <h3>Claude session</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`claude-model-${paneId}`}>
            Claude model
          </label>
          <ThemedCombobox
            id={`claude-model-${paneId}`}
            className="prompt-settings-select"
            value={session.model}
            options={modelOptions}
            disabled={isUpdating || isRefreshingModelOptions || !session.modelOptions?.length}
            onChange={(nextValue) => void onSessionSettingsChange(session.id, "model", nextValue)}
          />
          <SessionModelRefreshAction
            disabled={isUpdating || isRefreshingModelOptions}
            isRefreshing={isRefreshingModelOptions}
            sessionId={session.id}
            onRequestModelOptions={onRequestModelOptions}
          />
          <SessionModelRefreshFeedback
            agent={session.agent}
            isRefreshing={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
          />
          <SessionModelDetails option={currentModelOption} />
          <SessionManualModelControl
            paneId={paneId}
            session={session}
            isUpdating={isUpdating}
            onSessionSettingsChange={onSessionSettingsChange}
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`claude-approval-mode-${paneId}`}>
            Claude mode
          </label>
          <ThemedCombobox
            id={`claude-approval-mode-${paneId}`}
            className="prompt-settings-select"
            value={session.claudeApprovalMode ?? "ask"}
            options={CLAUDE_APPROVAL_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "claudeApprovalMode",
                nextValue as ClaudeApprovalMode,
              )
            }
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`claude-effort-${paneId}`}>
            Claude effort
          </label>
          <ThemedCombobox
            id={`claude-effort-${paneId}`}
            className="prompt-settings-select"
            value={currentClaudeEffortValue}
            options={claudeEffortOptions}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "claudeEffort",
                nextValue as ClaudeEffortLevel,
              )
            }
          />
        </div>
        <p className="session-control-hint">
          {isRefreshingModelOptions
            ? "Refreshing Claude's live model list from the session."
            : session.modelOptions?.length
              ? "Claude exposes its live model list during session initialization. Model changes are applied live to the session, and you can still paste a full model id if you need something outside the current list. Ask keeps approval cards, Auto-approve continues through tool requests, Plan keeps Claude in read-only analysis mode, and effort changes restart Claude before the next prompt."
              : "Start the Claude session once to load its live model list. New Claude sessions use Claude's default model, and you can still paste a full Claude model id manually."}
          {modelCapabilityHint ? ` ${modelCapabilityHint}` : ""}
        </p>
      </div>
    </article>
  );
}

function useSessionModelOptionsAutoRefresh({
  isRefreshingModelOptions,
  onRequestModelOptions,
  session,
}: {
  isRefreshingModelOptions: boolean;
  onRequestModelOptions: (sessionId: string) => void;
  session: Session;
}) {
  const requestedSessionIdRef = useRef<string | null>(null);

  useEffect(() => {
    if (session.modelOptions?.length) {
      requestedSessionIdRef.current = session.id;
      return;
    }
    if (isRefreshingModelOptions || requestedSessionIdRef.current === session.id) {
      return;
    }

    requestedSessionIdRef.current = session.id;
    void onRequestModelOptions(session.id);
  }, [isRefreshingModelOptions, onRequestModelOptions, session.id, session.modelOptions]);
}

function SessionModelRefreshAction({
  disabled,
  isRefreshing,
  sessionId,
  onRequestModelOptions,
}: {
  disabled: boolean;
  isRefreshing: boolean;
  sessionId: string;
  onRequestModelOptions: (sessionId: string) => void;
}) {
  return (
    <button
      className="ghost-button session-model-refresh-button"
      type="button"
      onClick={() => void onRequestModelOptions(sessionId)}
      disabled={disabled}
    >
      {isRefreshing ? "Refreshing models..." : "Refresh models"}
    </button>
  );
}

function SessionModelRefreshFeedback({
  agent,
  isRefreshing,
  modelOptionsError,
}: {
  agent: AgentType;
  isRefreshing: boolean;
  modelOptionsError: string | null;
}) {
  if (modelOptionsError) {
    return (
      <p className="session-control-error" role="alert">
        Could not refresh {agent}'s live model list for this session. {modelOptionsError}
      </p>
    );
  }

  if (!isRefreshing) {
    return null;
  }

  return (
    <p className="session-control-status" aria-live="polite">
      Refreshing {agent}'s live model list from the active session.
    </p>
  );
}

function SessionManualModelControl({
  paneId,
  session,
  isUpdating,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  const [customModel, setCustomModel] = useState(session.model);

  useEffect(() => {
    setCustomModel(session.model);
  }, [session.id, session.model]);

  const trimmedCustomModel = customModel.trim();
  const matchedModelOption = matchingSessionModelOption(session.modelOptions, trimmedCustomModel);
  const normalizedCustomModel = normalizedRequestedSessionModel(session, trimmedCustomModel);
  const hasLiveModelList = (session.modelOptions?.length ?? 0) > 0;
  const canApplyCustomModel =
    trimmedCustomModel.length > 0 && normalizedCustomModel !== session.model && !isUpdating;
  const validationMessage =
    trimmedCustomModel.length === 0
      ? null
      : matchedModelOption &&
          normalizedCustomModel === session.model &&
          trimmedCustomModel !== session.model
        ? `${trimmedCustomModel} already resolves to the current session model.`
      : matchedModelOption && normalizedCustomModel !== trimmedCustomModel
        ? `Matches ${matchedModelOption.label} from the current live list. TermAl will apply ${normalizedCustomModel}.`
        : !matchedModelOption && hasLiveModelList
          ? `${trimmedCustomModel} is not in the current live model list. TermAl will still try it on the next prompt.`
          : null;
  const validationTone =
    validationMessage && !matchedModelOption && hasLiveModelList ? "warning" : "info";

  function applyCustomModel() {
    if (!canApplyCustomModel) {
      return;
    }

    void onSessionSettingsChange(session.id, "model", normalizedCustomModel);
  }

  return (
    <div className="session-model-custom">
      <label className="session-control-label" htmlFor={`${session.agent}-custom-model-${paneId}`}>
        Manual model id
      </label>
      <div className="session-model-custom-row">
        <input
          id={`${session.agent}-custom-model-${paneId}`}
          className="themed-input session-model-custom-input"
          type="text"
          value={customModel}
          placeholder={manualSessionModelPlaceholder(session.agent)}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          disabled={isUpdating}
          onChange={(event) => setCustomModel(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") {
              return;
            }

            event.preventDefault();
            applyCustomModel();
          }}
        />
        <button
          type="button"
          className="ghost-button session-model-custom-apply"
          disabled={!canApplyCustomModel}
          onClick={applyCustomModel}
        >
          Apply
        </button>
      </div>
      {validationMessage ? (
        <p
          className={`session-model-custom-validation ${validationTone === "warning" ? "warning" : "info"}`}
          aria-live="polite"
        >
          {validationMessage}
        </p>
      ) : null}
    </div>
  );
}

function SessionModelDetails({
  option,
}: {
  option: SessionModelOption | null;
}) {
  const capabilitySummary = sessionModelCapabilitySummary(option);
  const description = option?.description ?? null;
  const badges = option?.badges ?? [];
  if (!description && badges.length === 0 && !capabilitySummary) {
    return null;
  }

  return (
    <div className="session-model-details" aria-live="polite">
      {description ? (
        <p className="session-model-description">{description}</p>
      ) : null}
      {badges.length > 0 ? (
        <div className="session-model-badges">
          {badges.map((badge) => (
            <span key={badge} className="session-model-badge">
              {badge}
            </span>
          ))}
        </div>
      ) : null}
      {capabilitySummary ? (
        <p className="session-model-description">{capabilitySummary}</p>
      ) : null}
    </div>
  );
}

export function CursorPromptSettingsCard({
  paneId,
  session,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  onRequestModelOptions,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  onRequestModelOptions: (sessionId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  useSessionModelOptionsAutoRefresh({
    isRefreshingModelOptions,
    onRequestModelOptions,
    session,
  });

  const modelOptions = sessionModelComboboxOptions(session.modelOptions, session.model);
  const canChangeModel = (session.modelOptions?.length ?? 0) > 0;
  const currentModelOption = currentSessionModelOption(session);

  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Session Mode</div>
      <h3>Cursor session</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`cursor-model-${paneId}`}>
            Cursor model
          </label>
          <ThemedCombobox
            id={`cursor-model-${paneId}`}
            className="prompt-settings-select"
            value={session.model}
            options={modelOptions}
            disabled={isUpdating || !canChangeModel}
            onChange={(nextValue) =>
              void onSessionSettingsChange(session.id, "model", nextValue)
            }
          />
          <SessionModelRefreshAction
            disabled={isUpdating || isRefreshingModelOptions}
            isRefreshing={isRefreshingModelOptions}
            sessionId={session.id}
            onRequestModelOptions={onRequestModelOptions}
          />
          <SessionModelRefreshFeedback
            agent={session.agent}
            isRefreshing={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
          />
          <SessionModelDetails option={currentModelOption} />
          <SessionManualModelControl
            paneId={paneId}
            session={session}
            isUpdating={isUpdating}
            onSessionSettingsChange={onSessionSettingsChange}
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`cursor-mode-${paneId}`}>
            Cursor mode
          </label>
          <ThemedCombobox
            id={`cursor-mode-${paneId}`}
            className="prompt-settings-select"
            value={session.cursorMode ?? "agent"}
            options={CURSOR_MODE_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(session.id, "cursorMode", nextValue as CursorMode)
            }
          />
        </div>
        <p className="session-control-hint">
          {isRefreshingModelOptions
            ? "Loading Cursor's live model list for this session."
            : canChangeModel
              ? "Model and mode changes apply to the live Cursor session. You can still paste a full model id manually if Cursor's list is behind."
              : "TermAl asks Cursor for its live model list when this session opens. New sessions begin on Auto, and you can still paste a full model id manually."}{" "}
          Agent auto-approves tool requests and can edit, Ask keeps approval cards, and Plan
          denies tool requests.
        </p>
      </div>
    </article>
  );
}

export function GeminiPromptSettingsCard({
  paneId,
  session,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  onRequestModelOptions,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  onRequestModelOptions: (sessionId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  useSessionModelOptionsAutoRefresh({
    isRefreshingModelOptions,
    onRequestModelOptions,
    session,
  });

  const modelOptions = sessionModelComboboxOptions(session.modelOptions, session.model);
  const canChangeModel = (session.modelOptions?.length ?? 0) > 0;
  const currentModelOption = currentSessionModelOption(session);

  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Session Settings</div>
      <h3>Gemini session</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`gemini-model-${paneId}`}>
            Gemini model
          </label>
          <ThemedCombobox
            id={`gemini-model-${paneId}`}
            className="prompt-settings-select"
            value={session.model}
            options={modelOptions}
            disabled={isUpdating || !canChangeModel}
            onChange={(nextValue) =>
              void onSessionSettingsChange(session.id, "model", nextValue)
            }
          />
          <SessionModelRefreshAction
            disabled={isUpdating || isRefreshingModelOptions}
            isRefreshing={isRefreshingModelOptions}
            sessionId={session.id}
            onRequestModelOptions={onRequestModelOptions}
          />
          <SessionModelRefreshFeedback
            agent={session.agent}
            isRefreshing={isRefreshingModelOptions}
            modelOptionsError={modelOptionsError}
          />
          <SessionModelDetails option={currentModelOption} />
          <SessionManualModelControl
            paneId={paneId}
            session={session}
            isUpdating={isUpdating}
            onSessionSettingsChange={onSessionSettingsChange}
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`gemini-approval-mode-${paneId}`}>
            Gemini approval mode
          </label>
          <ThemedCombobox
            id={`gemini-approval-mode-${paneId}`}
            className="prompt-settings-select"
            value={session.geminiApprovalMode ?? "default"}
            options={GEMINI_APPROVAL_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "geminiApprovalMode",
                nextValue as GeminiApprovalMode,
              )
            }
          />
        </div>
        <p className="session-control-hint">
          {isRefreshingModelOptions
            ? "Loading Gemini's live model list for this session."
            : canChangeModel
              ? "Model changes apply to the live Gemini session. You can still paste a full Gemini model id manually if the live list is behind."
              : "TermAl asks Gemini for its live model list when this session opens. New sessions begin on Auto, and you can still paste a full Gemini model id manually."}{" "}
          Default prompts for approval, Auto edit approves edit tools, YOLO approves all tools,
          and Plan stays read-only. Approval-mode changes apply on the next Gemini prompt.
        </p>
      </div>
    </article>
  );
}
