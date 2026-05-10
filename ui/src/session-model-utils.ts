import type {
  AgentReadiness,
  AgentType,
  AppPreferences,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  Project,
  RemoteConfig,
  Session,
  SessionModelOption,
} from "./types";
import { matchingSessionModelOption } from "./session-model-options";
import {
  LOCAL_REMOTE_ID,
  createBuiltinLocalRemote,
  isLocalRemoteId,
  normalizeRemoteConfigs,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "./remotes";

export type ComboboxOption = {
  label: string;
  value: string;
  description?: string;
  badges?: string[];
};

export const NEW_SESSION_MODEL_OPTIONS: Readonly<Record<AgentType, readonly ComboboxOption[]>> = {
  Claude: [{ label: "Default", value: "default" }],
  Codex: [{ label: "GPT-5.4", value: "gpt-5.4" }],
  Cursor: [{ label: "Auto", value: "auto" }],
  Gemini: [{ label: "Auto", value: "auto" }],
};

export const CODEX_REASONING_EFFORT_OPTIONS = [
  { label: "none", value: "none", description: "Disable reasoning for speed-sensitive turns" },
  { label: "minimal", value: "minimal", description: "Use the lightest reasoning pass" },
  { label: "low", value: "low", description: "Keep reasoning light" },
  { label: "medium", value: "medium", description: "Use the standard reasoning depth" },
  { label: "high", value: "high", description: "Use deeper reasoning for harder prompts" },
  { label: "xhigh", value: "xhigh", description: "Use the maximum reasoning depth" },
] as const;

export const CLAUDE_EFFORT_OPTIONS = [
  {
    label: "default",
    value: "default",
    description: "Use Claude's default effort for this session",
  },
  { label: "low", value: "low", description: "Keep reasoning light" },
  { label: "medium", value: "medium", description: "Use the standard effort level" },
  { label: "high", value: "high", description: "Use deeper reasoning for harder prompts" },
  { label: "max", value: "max", description: "Use the highest available effort" },
] as const;

export const SESSION_SCOPED_MODEL_AGENTS = new Set<AgentType>(["Claude", "Codex", "Cursor", "Gemini"]);

export const ALL_CODEX_REASONING_EFFORTS = CODEX_REASONING_EFFORT_OPTIONS.map(
  (option) => option.value,
) as CodexReasoningEffort[];
export const DEFAULT_CODEX_REASONING_EFFORT: CodexReasoningEffort = "medium";
export const DEFAULT_CLAUDE_APPROVAL_MODE: ClaudeApprovalMode = "ask";
export const DEFAULT_CLAUDE_EFFORT: ClaudeEffortLevel = "default";
export const DEFAULT_MODEL_PREFERENCE = "default";
export const MAX_DEFAULT_MODEL_PREFERENCE_CHARS = 200;
export const FALLBACK_CLAUDE_EFFORTS = ["low", "medium", "high"] as ClaudeEffortLevel[];

export function defaultNewSessionModel(agent: AgentType): string {
  return NEW_SESSION_MODEL_OPTIONS[agent][0]?.value ?? "";
}

export function usesSessionModelPicker(agent: AgentType): boolean {
  return SESSION_SCOPED_MODEL_AGENTS.has(agent);
}

export function isDefaultModelPreference(model: string): boolean {
  const trimmed = model.trim();
  return trimmed.length === 0 || trimmed.toLowerCase() === DEFAULT_MODEL_PREFERENCE;
}

export function createSessionModelHint(agent: AgentType): string {
  switch (agent) {
    case "Claude":
      return "Claude model selection lives on the session itself. TermAl asks Claude for its live model list after the session opens, and you can always enter a full Claude model id manually. New Claude sessions use the configured app default model; set it to default to let Claude choose.";
    case "Codex":
      return "Codex model selection lives on the session itself. TermAl asks Codex for its live model list after the session opens, and you can always enter a full Codex model id manually. New Codex sessions use the configured app default model; set it to default to let Codex choose.";
    case "Cursor":
      return "Cursor model selection lives on the session itself, like Cursor Agent's /model flow. TermAl asks Cursor for its live model list after the session opens, and you can still enter a full model id manually. New Cursor sessions use the configured app default model; set it to default to leave Cursor on Auto.";
    case "Gemini":
      return "Gemini model selection lives on the session itself. TermAl asks Gemini for its live model list after the session opens, and you can still enter a full Gemini model id manually. New Gemini sessions use the configured app default model; set it to default to leave Gemini on Auto.";
  }
}

export function staticSessionModelOptions(agent: AgentType, currentModel: string): ComboboxOption[] {
  const options = [...NEW_SESSION_MODEL_OPTIONS[agent]];
  if (options.some((option) => option.value === currentModel)) {
    return options;
  }

  return [
    ...options,
    {
      label: currentModel === "auto" ? "Auto" : currentModel,
      value: currentModel,
    },
  ];
}

export function formatSessionModelOptionLabel(model: string): string {
  if (model === "auto") {
    return "Auto";
  }
  if (model === "default") {
    return "Default";
  }
  return model;
}

export function sessionModelComboboxOptions(
  modelOptions: Session["modelOptions"] | undefined,
  currentModel: string,
): ComboboxOption[] {
  if (modelOptions?.length) {
    return modelOptions.map((option) => ({
      label: option.label,
      value: option.value,
      description: sessionModelOptionDescription(option) ?? undefined,
      badges: option.badges?.length ? option.badges : undefined,
    }));
  }

  return [
    {
      label: formatSessionModelOptionLabel(currentModel),
      value: currentModel,
    },
  ];
}

export function normalizedRequestedSessionModel(session: Session, requestedModel: string) {
  return matchingSessionModelOption(session.modelOptions, requestedModel)?.value ?? requestedModel.trim();
}

export function currentSessionModelOption(session: Session) {
  return matchingSessionModelOption(session.modelOptions, session.model);
}

export function resolveAppPreferences(preferences?: AppPreferences | null) {
  return {
    defaultCodexModel: preferences?.defaultCodexModel ?? DEFAULT_MODEL_PREFERENCE,
    defaultClaudeModel: preferences?.defaultClaudeModel ?? DEFAULT_MODEL_PREFERENCE,
    defaultCursorModel: preferences?.defaultCursorModel ?? DEFAULT_MODEL_PREFERENCE,
    defaultGeminiModel: preferences?.defaultGeminiModel ?? DEFAULT_MODEL_PREFERENCE,
    defaultCodexReasoningEffort:
      preferences?.defaultCodexReasoningEffort ?? DEFAULT_CODEX_REASONING_EFFORT,
    defaultClaudeApprovalMode:
      preferences?.defaultClaudeApprovalMode ?? DEFAULT_CLAUDE_APPROVAL_MODE,
    defaultClaudeEffort: preferences?.defaultClaudeEffort ?? DEFAULT_CLAUDE_EFFORT,
    remotes: normalizeRemoteConfigs(preferences?.remotes),
  };
}

export function areRemoteConfigsEqual(left: readonly RemoteConfig[], right: readonly RemoteConfig[]) {
  return (
    left.length === right.length &&
    left.every((remote, index) => {
      const candidate = right[index];
      return (
        candidate !== undefined &&
        remote.id === candidate.id &&
        remote.name === candidate.name &&
        remote.transport === candidate.transport &&
        remote.enabled === candidate.enabled &&
        remote.host === candidate.host &&
        remote.port === candidate.port &&
        remote.user === candidate.user
      );
    })
  );
}

export function resolveRemoteConfig(
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
  remoteId?: string | null,
): RemoteConfig {
  return remoteLookup.get(remoteId?.trim() || LOCAL_REMOTE_ID) ?? createBuiltinLocalRemote();
}

export function describeProjectScope(
  project: Project,
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
): string {
  const remoteId = resolveProjectRemoteId(project);
  const remote = resolveRemoteConfig(remoteLookup, remoteId);
  const remoteName = remoteDisplayName(remote, remoteId);
  const connection = remoteConnectionLabel(remote);
  if (isLocalRemoteId(remoteId)) {
    return `${remoteName} - ${project.rootPath}`;
  }

  return `${remoteName} (${connection}) - ${project.rootPath}`;
}

export function remoteBadgeLabel(remote: RemoteConfig): string {
  return remote.transport === "local" ? "Local" : "SSH";
}

export function codexReasoningEffortOption(
  effort: CodexReasoningEffort,
): ComboboxOption {
  return (
    CODEX_REASONING_EFFORT_OPTIONS.find((option) => option.value === effort) ?? {
      label: effort,
      value: effort,
      description: undefined,
    }
  );
}

export function supportedCodexReasoningEffortsForModelOption(option: SessionModelOption | null) {
  return option?.supportedReasoningEfforts?.length
    ? option.supportedReasoningEfforts
    : ALL_CODEX_REASONING_EFFORTS;
}

export function defaultCodexReasoningEffortForModelOption(option: SessionModelOption | null) {
  const supportedEfforts = supportedCodexReasoningEffortsForModelOption(option);
  if (
    option?.defaultReasoningEffort &&
    supportedEfforts.includes(option.defaultReasoningEffort)
  ) {
    return option.defaultReasoningEffort;
  }

  return supportedEfforts[0] ?? null;
}

export function currentCodexModelOption(session: Session) {
  if (session.agent !== "Codex") {
    return null;
  }

  return currentSessionModelOption(session);
}

export function currentClaudeEffort(session: Session): ClaudeEffortLevel {
  return session.claudeEffort ?? DEFAULT_CLAUDE_EFFORT;
}

export function claudeEffortOption(effort: ClaudeEffortLevel): ComboboxOption {
  return (
    CLAUDE_EFFORT_OPTIONS.find((option) => option.value === effort) ?? {
      label: effort,
      value: effort,
      description: undefined,
    }
  );
}

export function supportedClaudeEffortLevelsForModelOption(
  option: SessionModelOption | null,
  currentEffort: ClaudeEffortLevel,
): ClaudeEffortLevel[] {
  if (option?.supportedClaudeEffortLevels?.length) {
    return option.supportedClaudeEffortLevels;
  }

  const supportsEffort = option?.badges?.includes("Effort") ?? false;
  if (!option || supportsEffort) {
    return currentEffort === "max"
      ? [...FALLBACK_CLAUDE_EFFORTS, "max"]
      : [...FALLBACK_CLAUDE_EFFORTS];
  }

  return currentEffort === "max" ? ["max"] : [];
}

export function claudeEffortComboboxOptions(session: Session) {
  const currentModelOption = currentSessionModelOption(session);
  const currentEffort = currentClaudeEffort(session);
  const levels = supportedClaudeEffortLevelsForModelOption(currentModelOption, currentEffort);

  return [claudeEffortOption(DEFAULT_CLAUDE_EFFORT), ...levels.map(claudeEffortOption)];
}

export function claudeEffortHint(session: Session) {
  const currentModelOption = currentSessionModelOption(session);
  const levels = supportedClaudeEffortLevelsForModelOption(
    currentModelOption,
    currentClaudeEffort(session),
  );

  if (!levels.length) {
    return currentModelOption?.badges?.includes("Effort") === false
      ? `${currentModelOption.label} does not advertise Claude effort controls in the live model list.`
      : "Claude effort applies when the session starts or restarts.";
  }

  return `${currentModelOption?.label ?? "This model"} supports ${levels.join(", ")} effort.`;
}

export function normalizedCodexReasoningEffort(
  session: Session,
  model: string = session.model,
): CodexReasoningEffort {
  const currentEffort = session.reasoningEffort ?? "medium";
  const modelOption = matchingSessionModelOption(session.modelOptions, model);
  const supportedEfforts = modelOption?.supportedReasoningEfforts ?? [];
  if (!supportedEfforts.length) {
    return currentEffort;
  }
  if (supportedEfforts.includes(currentEffort)) {
    return currentEffort;
  }

  return defaultCodexReasoningEffortForModelOption(modelOption) ?? currentEffort;
}

export function codexReasoningEffortComboboxOptions(session: Session, model: string = session.model) {
  const modelOption = matchingSessionModelOption(session.modelOptions, model);
  const defaultEffort = defaultCodexReasoningEffortForModelOption(modelOption);

  return supportedCodexReasoningEffortsForModelOption(modelOption).map((effort) => {
    const option = codexReasoningEffortOption(effort);
    return {
      ...option,
      badges: defaultEffort === effort ? ["Default"] : undefined,
    };
  });
}

export function codexReasoningEffortHint(session: Session, model: string = session.model) {
  const modelOption = matchingSessionModelOption(session.modelOptions, model);
  if (!modelOption?.supportedReasoningEfforts?.length) {
    return null;
  }

  const supported = modelOption.supportedReasoningEfforts.join(", ");
  const defaultEffort = defaultCodexReasoningEffortForModelOption(modelOption);
  return defaultEffort
    ? `This model supports ${supported} reasoning. ${defaultEffort} is the default.`
    : `This model supports ${supported} reasoning.`;
}

export function sessionModelCapabilitySummary(option: SessionModelOption | null) {
  const parts = [];

  if (option?.supportedClaudeEffortLevels?.length) {
    parts.push(`Effort: ${option.supportedClaudeEffortLevels.join(", ")}.`);
  }

  if (option?.supportedReasoningEfforts?.length) {
    const supported = option.supportedReasoningEfforts.join(", ");
    const defaultEffort =
      option.defaultReasoningEffort &&
      option.supportedReasoningEfforts.includes(option.defaultReasoningEffort)
        ? option.defaultReasoningEffort
        : null;
    parts.push(
      defaultEffort
        ? `Reasoning: ${supported}. Default ${defaultEffort}.`
        : `Reasoning: ${supported}.`,
    );
  }

  return parts.length > 0 ? parts.join(" ") : null;
}

export function sessionModelOptionDescription(option: SessionModelOption | null) {
  if (!option) {
    return null;
  }

  const parts = [];
  if (option.description) {
    parts.push(option.description);
  }

  const capabilitySummary = sessionModelCapabilitySummary(option);
  if (capabilitySummary) {
    parts.push(capabilitySummary);
  }

  return parts.join(" \u00b7 ");
}

export function manualSessionModelPlaceholder(agent: AgentType): string {
  switch (agent) {
    case "Claude":
      return "claude-sonnet-4-6";
    case "Codex":
      return "gpt-5.4";
    case "Cursor":
      return "gpt-5.3-codex";
    case "Gemini":
      return "gemini-2.5-pro";
  }
}

export function unknownSessionModelConfirmationKey(sessionId: string, model: string) {
  return `${sessionId}:${model}`;
}

export function describeUnknownSessionModelWarning(session: Session) {
  if ((session.modelOptions?.length ?? 0) === 0) {
    return null;
  }
  if (currentSessionModelOption(session)) {
    return null;
  }

  return `${session.agent} is set to ${session.model}, but that model is not in the current live list. Refresh models to verify it, or send the prompt again to continue anyway.`;
}

export function resolveUnknownSessionModelSendAttempt(
  confirmedKeys: ReadonlySet<string>,
  session: Session,
) {
  const warning = describeUnknownSessionModelWarning(session);
  const confirmationKey = unknownSessionModelConfirmationKey(session.id, session.model);
  const nextConfirmedKeys = new Set(confirmedKeys);

  if (!warning) {
    nextConfirmedKeys.delete(confirmationKey);
    return {
      allowSend: true,
      nextConfirmedKeys,
      warning: null,
    };
  }

  if (nextConfirmedKeys.has(confirmationKey)) {
    return {
      allowSend: true,
      nextConfirmedKeys,
      warning: null,
    };
  }

  nextConfirmedKeys.add(confirmationKey);
  return {
    allowSend: false,
    nextConfirmedKeys,
    warning,
  };
}

export function describeSessionModelRefreshError(
  agent: AgentType,
  rawError: string,
  readiness: AgentReadiness | null = null,
) {
  if (readiness?.blocking) {
    return readiness.detail;
  }

  const normalizedError = rawError.toLowerCase();
  if (agent === "Cursor") {
    if (normalizedError.includes("timed out")) {
      return "Cursor did not return its live model list in time. Try Refresh models again, or send a prompt to warm up the session.";
    }
    if (normalizedError.includes("did not return a result")) {
      return "Cursor did not return its live model list. Try Refresh models again after the session finishes connecting.";
    }
    return "Cursor could not refresh its live model list. Verify `cursor-agent` is installed and signed in, then try again.";
  }

  if (agent === "Gemini") {
    if (
      normalizedError.includes("auth") ||
      normalizedError.includes("credential") ||
      normalizedError.includes("api key") ||
      normalizedError.includes("oauth") ||
      normalizedError.includes("vertex")
    ) {
      return readiness?.detail ??
        "Gemini needs valid auth before it can return its live model list. Configure `GEMINI_API_KEY`, Vertex AI, or Google login, then try again.";
    }
    if (normalizedError.includes("timed out")) {
      return "Gemini did not return its live model list in time. Confirm the CLI is authenticated, then try Refresh models again.";
    }
    return "Gemini could not refresh its live model list. Confirm the CLI is installed and authenticated, then try again.";
  }

  if (agent === "Claude") {
    if (normalizedError.includes("timed out")) {
      return "Claude did not return its live model list in time. Try Refresh models again. If this keeps happening, start a new Claude session.";
    }
    if (normalizedError.includes("restart") || normalizedError.includes("start persistent")) {
      return "Claude could not restart cleanly to reload its model list. Try sending a prompt or opening a new Claude session.";
    }
    return "Claude could not refresh its live model list. Try Refresh models again or start a new Claude session.";
  }

  if (agent === "Codex") {
    if (normalizedError.includes("timed out")) {
      return "Codex did not return its live model list in time. Try Refresh models again, or send a prompt to warm up the runtime.";
    }
    if (normalizedError.includes("start persistent")) {
      return "Codex could not start its local runtime to refresh models. Verify the Codex CLI is installed and authenticated, then try again.";
    }
    return "Codex could not refresh its live model list. Try Refresh models again or restart the session.";
  }

  return rawError;
}

export function formatCodexReasoningEffortList(efforts: readonly CodexReasoningEffort[]) {
  if (efforts.length === 0) {
    return "";
  }
  if (efforts.length === 1) {
    return efforts[0];
  }
  if (efforts.length === 2) {
    return `${efforts[0]} and ${efforts[1]}`;
  }

  return `${efforts.slice(0, -1).join(", ")}, and ${efforts[efforts.length - 1]}`;
}

export function describeCodexModelAdjustmentNotice(previousSession: Session, nextSession: Session) {
  if (previousSession.agent !== "Codex" || nextSession.agent !== "Codex") {
    return null;
  }

  const previousEffort = normalizedCodexReasoningEffort(previousSession);
  const nextEffort = normalizedCodexReasoningEffort(nextSession);
  if (previousEffort === nextEffort) {
    return null;
  }

  const currentModelOption = currentCodexModelOption(nextSession);
  const currentModelLabel = currentModelOption?.label ?? nextSession.model;
  const supportedEfforts = currentModelOption?.supportedReasoningEfforts ?? [];

  if (supportedEfforts.length > 0) {
    return `${currentModelLabel} only supports ${formatCodexReasoningEffortList(
      supportedEfforts,
    )} reasoning, so TermAl reset effort from ${previousEffort} to ${nextEffort}.`;
  }

  return `${currentModelLabel} reset Codex reasoning effort from ${previousEffort} to ${nextEffort}.`;
}

export function resolveControlPanelWorkspaceRoot(
  selectedProject: Project | null,
  activeSessionWorkdir: string | null,
) {
  if (!selectedProject) {
    const normalizedWorkdir = activeSessionWorkdir?.trim() ?? "";
    return normalizedWorkdir || null;
  }

  return isLocalRemoteId(resolveProjectRemoteId(selectedProject)) ? selectedProject.rootPath : null;
}
