// Slash-palette state derivation for the session composer.
//
// What this file owns:
//   - The full slash-command and slash-choice taxonomy: the
//     `SLASH_COMMANDS` registry, per-agent static model options,
//     and the sandbox / approval / reasoning-effort / mode option
//     tables.
//   - The `SlashPaletteItem`, `SlashPaletteState`,
//     `SlashChoiceDefinition`, `SlashChoiceState`, `SlashCommandId`,
//     and `SessionModelChoice` types that describe the palette
//     surface.
//   - `buildSlashPaletteState` — the top-level resolver that turns a
//     (session, draft, agent-commands) triple into a rendered
//     palette state, including the "no session or no leading slash"
//     fast path, the empty-query session-command + agent-command
//     listing, and the per-command choice tabs (`/model`, `/mode`,
//     `/sandbox`, `/approvals`, `/effort`).
//   - Per-agent choice builders (`sessionModeSlashState`,
//     `codexSandboxSlashState`, `codexApprovalSlashState`,
//     `codexReasoningEffortSlashState`, `claudeEffortSlashState`,
//     `sessionModelSlashState`) plus supporting pure helpers
//     (`matchSlashCommand`, `matchSlashChoice`, `makeSlashChoices`,
//     `matchAgentCommand`, `parseAgentCommandDraft`,
//     `ensureCurrentSessionModelChoice`,
//     `sessionModelChoicesForSlashCommand`,
//     `manualSessionModelSlashItem`,
//     `supportsLiveSessionModelOptions`,
//     `supportedCodexReasoningEfforts`,
//     `supportedClaudeEffortLevels`, …).
//
// What this file does NOT own:
//   - The palette UI — rendering lives in the `SessionComposer`
//     component in `./AgentSessionPanel.tsx`.
//   - The `isSpaceKey` keyboard helper — that remains in the
//     composer because only the composer consumes it.
//   - Session-model-option matching — that lives in
//     `../session-model-options` (imported).
//
// Split out of `ui/src/panels/AgentSessionPanel.tsx`. Same types,
// same constant values, same function bodies; consumers import
// directly from here.

import { matchingSessionModelOption } from "../session-model-options";
import type { SessionSummarySnapshot } from "../session-store";
import type {
  AgentCommand,
  ApprovalPolicy,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  SandboxMode,
  Session,
  SessionSettingsField,
} from "../types";

export type SlashPaletteSession = Pick<
  SessionSummarySnapshot,
  | "approvalPolicy"
  | "agent"
  | "agentCommandsRevision"
  | "claudeApprovalMode"
  | "claudeEffort"
  | "cursorMode"
  | "geminiApprovalMode"
  | "id"
  | "model"
  | "modelOptions"
  | "reasoningEffort"
  | "sandboxMode"
  | "workdir"
>;

export const STATIC_MODEL_OPTIONS: Readonly<Record<Session["agent"], readonly SessionModelChoice[]>> = {
  Claude: [],
  Codex: [],
  Cursor: [{ detail: "Auto", label: "Auto", value: "auto" }],
  Gemini: [{ detail: "Auto", label: "Auto", value: "auto" }],
};
export const SANDBOX_SLASH_OPTIONS = [
  { detail: "Write inside the workspace", label: "workspace-write", value: "workspace-write" },
  { detail: "Read without editing files", label: "read-only", value: "read-only" },
  { detail: "Allow unrestricted file access", label: "danger-full-access", value: "danger-full-access" },
] as const;
export const APPROVAL_POLICY_SLASH_OPTIONS = [
  { detail: "Never ask before tools run", label: "never", value: "never" },
  { detail: "Ask whenever a tool needs approval", label: "on-request", value: "on-request" },
  { detail: "Approve trusted commands, ask for the rest", label: "untrusted", value: "untrusted" },
  { detail: "Only ask after failures", label: "on-failure", value: "on-failure" },
] as const;
export const CODEX_REASONING_EFFORT_SLASH_OPTIONS = [
  { detail: "Disable reasoning for speed-sensitive turns", label: "none", value: "none" },
  { detail: "Use the lightest reasoning pass", label: "minimal", value: "minimal" },
  { detail: "Keep reasoning light", label: "low", value: "low" },
  { detail: "Use the standard reasoning depth", label: "medium", value: "medium" },
  { detail: "Use deeper reasoning for harder prompts", label: "high", value: "high" },
  { detail: "Use the maximum reasoning depth", label: "xhigh", value: "xhigh" },
] as const;
export const CLAUDE_MODE_SLASH_OPTIONS = [
  { detail: "Ask before tool use", label: "ask", value: "ask" },
  { detail: "Continue through tool requests", label: "auto-approve", value: "auto-approve" },
  { detail: "Stay read-only and plan", label: "plan", value: "plan" },
] as const;
export const CLAUDE_EFFORT_SLASH_OPTIONS = [
  { detail: "Use Claude's default effort for this session", label: "default", value: "default" },
  { detail: "Keep reasoning light", label: "low", value: "low" },
  { detail: "Use the standard effort level", label: "medium", value: "medium" },
  { detail: "Use deeper reasoning for harder prompts", label: "high", value: "high" },
  { detail: "Use the highest available effort", label: "max", value: "max" },
] as const;
export const CURSOR_MODE_SLASH_OPTIONS = [
  { detail: "Allow edits and auto-approve tools", label: "agent", value: "agent" },
  { detail: "Stay read-only and deny tools", label: "plan", value: "plan" },
  { detail: "Show approval cards before tools", label: "ask", value: "ask" },
] as const;
export const GEMINI_MODE_SLASH_OPTIONS = [
  { detail: "Ask before tool use", label: "default", value: "default" },
  { detail: "Auto-approve edit tools", label: "auto_edit", value: "auto_edit" },
  { detail: "Approve every tool", label: "yolo", value: "yolo" },
  { detail: "Stay read-only and plan", label: "plan", value: "plan" },
] as const;

export type SlashCommandId = "model" | "mode" | "sandbox" | "approvals" | "effort";

export const SLASH_COMMANDS: ReadonlyArray<{
  command: string;
  detail: string;
  id: SlashCommandId;
  label: string;
  supports: readonly Session["agent"][];
}> = [
  {
    command: "/model",
    detail: "Change the model for this session",
    id: "model",
    label: "/model",
    supports: ["Claude", "Codex", "Cursor", "Gemini"],
  },
  {
    command: "/mode",
    detail: "Change the session mode for this agent",
    id: "mode",
    label: "/mode",
    supports: ["Claude", "Cursor", "Gemini"],
  },
  {
    command: "/sandbox",
    detail: "Change Codex sandbox for the next prompt",
    id: "sandbox",
    label: "/sandbox",
    supports: ["Codex"],
  },
  {
    command: "/approvals",
    detail: "Change Codex approval policy for the next prompt",
    id: "approvals",
    label: "/approvals",
    supports: ["Codex"],
  },
  {
    command: "/effort",
    detail: "Change the effort for the next prompt",
    id: "effort",
    label: "/effort",
    supports: ["Claude", "Codex"],
  },
] as const;

export type SessionModelChoice = {
  detail: string;
  label: string;
  value: string;
};

export type SlashPaletteItem =
  | {
      command: string;
      detail: string;
      key: string;
      kind: "command";
      label: string;
      sectionLabel?: string;
    }
  | {
      command: AgentCommand;
      detail: string;
      hasArguments: boolean;
      key: string;
      kind: "agent-command";
      label: string;
      name: string;
      sectionLabel?: string;
    }
  | {
      detail: string;
      field: SessionSettingsField;
      isCurrent: boolean;
      key: string;
      kind: "choice";
      label: string;
      sectionLabel?: string;
      value: string;
    };

export type SlashPaletteState =
  | {
      kind: "none";
    }
  | {
      defaultActiveIndex: number;
      emptyMessage: string;
      errorMessage?: string | null;
      hint: string;
      isRefreshing?: boolean;
      items: readonly SlashPaletteItem[];
      kind: "command";
      refreshActionLabel?: string;
      resetKey: string;
      statusText?: string;
      supportsRefresh?: boolean;
      title: string;
    }
  | {
      defaultActiveIndex: number;
      emptyMessage: string;
      errorMessage?: string | null;
      hint: string;
      isRefreshing: boolean;
      items: readonly SlashPaletteItem[];
      kind: "choice";
      refreshActionLabel?: string;
      resetKey: string;
      supportsLiveRefresh: boolean;
      title: string;
    };

export type SlashChoiceDefinition = {
  detail: string;
  label: string;
  value: string;
};

export type SlashChoiceState = {
  emptyMessage: string;
  errorMessage?: string | null;
  hint: string;
  isRefreshing?: boolean;
  items: SlashPaletteItem[];
  refreshActionLabel?: string;
  supportsLiveRefresh?: boolean;
  title: string;
};

export function formatSessionModelLabel(model: string): string {
  if (model === "auto") {
    return "Auto";
  }
  if (model === "default") {
    return "Default";
  }
  return model;
}

export const ALL_CODEX_REASONING_EFFORTS = CODEX_REASONING_EFFORT_SLASH_OPTIONS.map(
  (option) => option.value,
) as CodexReasoningEffort[];
export const DEFAULT_CLAUDE_EFFORT: ClaudeEffortLevel = "default";
export const FALLBACK_CLAUDE_EFFORTS = ["low", "medium", "high"] as ClaudeEffortLevel[];

export function codexReasoningEffortChoice(effort: CodexReasoningEffort) {
  return (
    CODEX_REASONING_EFFORT_SLASH_OPTIONS.find((option) => option.value === effort) ?? {
      detail: effort,
      label: effort,
      value: effort,
    }
  );
}

export function claudeEffortChoice(effort: ClaudeEffortLevel) {
  return (
    CLAUDE_EFFORT_SLASH_OPTIONS.find((option) => option.value === effort) ?? {
      detail: effort,
      label: effort,
      value: effort,
    }
  );
}

export function currentSessionModelCapabilities(session: SlashPaletteSession) {
  return matchingSessionModelOption(session.modelOptions, session.model);
}

export function supportedCodexReasoningEfforts(session: SlashPaletteSession) {
  const option = currentSessionModelCapabilities(session);
  return option?.supportedReasoningEfforts?.length
    ? option.supportedReasoningEfforts
    : ALL_CODEX_REASONING_EFFORTS;
}

export function defaultCodexReasoningEffort(session: SlashPaletteSession) {
  const option = currentSessionModelCapabilities(session);
  const supportedEfforts = supportedCodexReasoningEfforts(session);
  if (
    option?.defaultReasoningEffort &&
    supportedEfforts.includes(option.defaultReasoningEffort)
  ) {
    return option.defaultReasoningEffort;
  }

  return supportedEfforts[0] ?? "medium";
}

export function currentClaudeEffort(session: SlashPaletteSession): ClaudeEffortLevel {
  return session.claudeEffort ?? DEFAULT_CLAUDE_EFFORT;
}

export function supportedClaudeEffortLevels(session: SlashPaletteSession) {
  const option = currentSessionModelCapabilities(session);
  const currentEffort = currentClaudeEffort(session);
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

export function claudeEffortChoices(session: SlashPaletteSession): SlashChoiceDefinition[] {
  const currentModel = currentSessionModelCapabilities(session);
  const levels = supportedClaudeEffortLevels(session);
  return ([DEFAULT_CLAUDE_EFFORT, ...levels] as ClaudeEffortLevel[]).map((effort) => {
    const option = claudeEffortChoice(effort);
    return {
      detail:
        effort === DEFAULT_CLAUDE_EFFORT
          ? option.detail
          : currentModel
            ? `${option.detail} | ${currentModel.label}`
            : option.detail,
      label: option.label,
      value: option.value,
    };
  });
}

export function codexReasoningEffortChoices(session: SlashPaletteSession): SlashChoiceDefinition[] {
  const currentModel = currentSessionModelCapabilities(session);
  const defaultEffort = defaultCodexReasoningEffort(session);
  return supportedCodexReasoningEfforts(session).map((effort) => {
    const option = codexReasoningEffortChoice(effort);
    return {
      detail:
        defaultEffort === effort
          ? `${option.detail} | Default for ${currentModel?.label ?? "this model"}`
          : option.detail,
      label: option.label,
      value: option.value,
    };
  });
}

export function sessionModelChoiceDetail(
  option: NonNullable<Session["modelOptions"]>[number],
) {
  const detailParts = [];
  if (option.description) {
    detailParts.push(option.description);
  }
  if (option.badges?.length) {
    detailParts.push(option.badges.join(" | "));
  }
  if (option.supportedClaudeEffortLevels?.length) {
    detailParts.push(`Effort ${option.supportedClaudeEffortLevels.join(", ")}`);
  }
  if (option.supportedReasoningEfforts?.length) {
    const defaultEffort =
      option.defaultReasoningEffort &&
      option.supportedReasoningEfforts.includes(option.defaultReasoningEffort)
        ? option.defaultReasoningEffort
        : null;
    detailParts.push(
      defaultEffort
        ? `Reasoning ${option.supportedReasoningEfforts.join(", ")} | Default ${defaultEffort}`
        : `Reasoning ${option.supportedReasoningEfforts.join(", ")}`,
    );
  }

  return detailParts.join(" | ") || option.value;
}

export function slashCommandsForSession(session: SlashPaletteSession) {
  return SLASH_COMMANDS.filter((command) => command.supports.includes(session.agent));
}

export function supportsAgentSlashCommands(_session: SlashPaletteSession): boolean {
  return true;
}

export function normalizedAgentCommandKind(command: AgentCommand) {
  return command.kind ?? "promptTemplate";
}

export function agentCommandLabel(command: AgentCommand) {
  return `/${command.name}`;
}

export function agentCommandHasArguments(command: AgentCommand) {
  return normalizedAgentCommandKind(command) === "nativeSlash"
    ? Boolean(command.argumentHint?.trim())
    : command.content.includes("$ARGUMENTS");
}

export function matchAgentCommand(command: AgentCommand, query: string) {
  if (query.length === 0) {
    return true;
  }

  const normalizedQuery = query.toLowerCase();
  return (
    command.name.toLowerCase().includes(normalizedQuery) ||
    command.description.toLowerCase().includes(normalizedQuery) ||
    command.source.toLowerCase().includes(normalizedQuery)
  );
}

export function parseAgentCommandDraft(draft: string) {
  const match = /^\/(\S+)(?:\s([\s\S]*))?$/.exec(draft);
  if (!match) {
    return null;
  }

  return {
    argumentsText: match[2] ?? "",
    commandName: match[1] ?? "",
  };
}

export function supportsLiveSessionModelOptions(session: SlashPaletteSession): boolean {
  return (
    session.agent === "Claude" ||
    session.agent === "Codex" ||
    session.agent === "Cursor" ||
    session.agent === "Gemini"
  );
}

export function ensureCurrentSessionModelChoice(
  options: readonly SessionModelChoice[],
  currentModel: string,
): SessionModelChoice[] {
  if (options.some((option) => option.value === currentModel)) {
    return [...options];
  }

  return [
    {
      detail: currentModel,
      label: formatSessionModelLabel(currentModel),
      value: currentModel,
    },
    ...options,
  ];
}

export function sessionModelChoicesForSlashCommand(session: SlashPaletteSession): SessionModelChoice[] {
  const baseOptions =
    session.agent === "Claude" ||
    session.agent === "Codex" ||
    session.agent === "Cursor" ||
    session.agent === "Gemini"
      ? session.modelOptions?.length
        ? session.modelOptions.map((option) => ({
            detail: sessionModelChoiceDetail(option),
            label: option.label,
            value: option.value,
          }))
        : STATIC_MODEL_OPTIONS[session.agent]
      : STATIC_MODEL_OPTIONS[session.agent];

  return ensureCurrentSessionModelChoice(baseOptions, session.model);
}

export function manualSessionModelSlashItem(
  options: readonly SessionModelChoice[],
  session: SlashPaletteSession,
  rawQuery: string,
): SlashPaletteItem | null {
  const trimmedQuery = rawQuery.trim();
  if (!trimmedQuery) {
    return null;
  }

  const normalizedQuery = trimmedQuery.toLowerCase();
  const hasExactMatch = options.some(
    (option) =>
      option.value.toLowerCase() === normalizedQuery ||
      option.label.toLowerCase() === normalizedQuery,
  );
  if (hasExactMatch || normalizedQuery === session.model.toLowerCase()) {
    return null;
  }
  const detail =
    (session.modelOptions?.length ?? 0) > 0
      ? `${trimmedQuery} is not in the current live model list. TermAl will still try it on the next prompt.`
      : `Apply ${trimmedQuery} to this ${session.agent} session while the live model list is still loading.`;

  return {
    detail,
    field: "model",
    isCurrent: false,
    key: `model:custom:${trimmedQuery}`,
    kind: "choice",
    label: `Use "${trimmedQuery}"`,
    value: trimmedQuery,
  };
}

export function matchSlashCommand(commandLabel: string, query: string) {
  return commandLabel.slice(1).toLowerCase().includes(query);
}

export function matchSlashChoice(choice: SlashChoiceDefinition, query: string) {
  if (query.length === 0) {
    return true;
  }

  const normalizedQuery = query.toLowerCase();
  return (
    choice.label.toLowerCase().includes(normalizedQuery) ||
    choice.value.toLowerCase().includes(normalizedQuery) ||
    choice.detail.toLowerCase().includes(normalizedQuery)
  );
}

export function makeSlashChoices(
  definitions: readonly SlashChoiceDefinition[],
  field: SessionSettingsField,
  currentValue: string,
  query: string,
) {
  return definitions
    .filter((choice) => matchSlashChoice(choice, query))
    .map<SlashPaletteItem>((choice) => ({
      detail: choice.detail,
      field,
      isCurrent: choice.value === currentValue,
      key: `${field}:${choice.value}`,
      kind: "choice",
      label: choice.label,
      value: choice.value,
    }));
}

export function sessionModeSlashState(session: SlashPaletteSession, query: string): SlashChoiceState | null {
  switch (session.agent) {
    case "Claude": {
      const currentMode = session.claudeApprovalMode ?? "ask";
      return {
        emptyMessage: `No Claude modes match "${query}".`,
        hint: "Enter to apply a Claude mode to this session.",
        items: makeSlashChoices(
          CLAUDE_MODE_SLASH_OPTIONS,
          "claudeApprovalMode",
          currentMode,
          query,
        ),
        title: "Claude modes",
      };
    }
    case "Cursor": {
      const currentMode = session.cursorMode ?? "agent";
      return {
        emptyMessage: `No Cursor modes match "${query}".`,
        hint: "Enter to apply a live Cursor mode.",
        items: makeSlashChoices(CURSOR_MODE_SLASH_OPTIONS, "cursorMode", currentMode, query),
        title: "Cursor modes",
      };
    }
    case "Gemini": {
      const currentMode = session.geminiApprovalMode ?? "default";
      return {
        emptyMessage: `No Gemini modes match "${query}".`,
        hint: "Enter to apply a Gemini approval mode.",
        items: makeSlashChoices(
          GEMINI_MODE_SLASH_OPTIONS,
          "geminiApprovalMode",
          currentMode,
          query,
        ),
        title: "Gemini modes",
      };
    }
    case "Codex":
      return null;
  }
}

export function codexSandboxSlashState(query: string, currentValue: SandboxMode): SlashChoiceState {
  return {
    emptyMessage: `No sandbox modes match "${query}".`,
    hint: "Enter to set the next Codex prompt sandbox.",
    items: makeSlashChoices(SANDBOX_SLASH_OPTIONS, "sandboxMode", currentValue, query),
    title: "Codex sandbox",
  };
}

export function codexApprovalSlashState(query: string, currentValue: ApprovalPolicy): SlashChoiceState {
  return {
    emptyMessage: `No approval policies match "${query}".`,
    hint: "Enter to set the next Codex prompt approval policy.",
    items: makeSlashChoices(APPROVAL_POLICY_SLASH_OPTIONS, "approvalPolicy", currentValue, query),
    title: "Codex approvals",
  };
}

export function codexReasoningEffortSlashState(session: SlashPaletteSession, query: string): SlashChoiceState {
  const currentValue = session.reasoningEffort ?? defaultCodexReasoningEffort(session);
  const currentModel = currentSessionModelCapabilities(session);
  const currentModelSupportHint = currentModel?.supportedReasoningEfforts?.length
    ? ` ${currentModel.label} supports ${currentModel.supportedReasoningEfforts.join(", ")}.`
    : "";
  return {
    emptyMessage: `No reasoning effort options match "${query}".`,
    hint: `Enter to set the next Codex prompt reasoning effort.${currentModelSupportHint}`,
    items: makeSlashChoices(codexReasoningEffortChoices(session), "reasoningEffort", currentValue, query),
    title: "Codex reasoning effort",
  };
}

export function claudeEffortSlashState(session: SlashPaletteSession, query: string): SlashChoiceState {
  const currentValue = currentClaudeEffort(session);
  const currentModel = currentSessionModelCapabilities(session);
  const supportedEfforts = supportedClaudeEffortLevels(session);
  const currentModelSupportHint = supportedEfforts.length
    ? ` ${currentModel?.label ?? "This model"} supports ${supportedEfforts.join(", ")}.`
    : "";
  return {
    emptyMessage: `No Claude effort options match "${query}".`,
    hint: `Enter to restart Claude with a different effort on the next prompt.${currentModelSupportHint}`,
    items: makeSlashChoices(claudeEffortChoices(session), "claudeEffort", currentValue, query),
    title: "Claude effort",
  };
}

export function sessionModelSlashState(
  session: SlashPaletteSession,
  query: string,
  rawQuery: string,
  isRefreshingModelOptions: boolean,
  modelOptionsError: string | null,
): SlashChoiceState {
  const supportsLiveRefresh = supportsLiveSessionModelOptions(session);
  const hasLiveModelList = (session.modelOptions?.length ?? 0) > 0;
  const sessionModelChoices = sessionModelChoicesForSlashCommand(session);
  const items = sessionModelChoices
    .filter((option) =>
      query.length === 0
        ? true
        : option.label.toLowerCase().includes(query) ||
          option.value.toLowerCase().includes(query) ||
          option.detail.toLowerCase().includes(query),
    )
    .map<SlashPaletteItem>((option) => ({
      detail: option.detail,
      field: "model",
      isCurrent: option.value === session.model,
      key: `model:${option.value}`,
      kind: "choice",
      label: option.label,
      value: option.value,
    }));
  const manualItem = manualSessionModelSlashItem(sessionModelChoices, session, rawQuery);
  if (manualItem) {
    items.unshift(manualItem);
  }

  let hint = "Enter to apply a model. Type a full model id to apply it manually. Esc clears the command line.";
  if (supportsLiveRefresh && !hasLiveModelList) {
    hint = isRefreshingModelOptions
      ? `Loading ${session.agent}'s live model list for this session.`
      : modelOptionsError
        ? `Could not load ${session.agent}'s live model list. Retry to fetch the full list.`
        : `Fetching ${session.agent}'s live model list for this session. You can still type a full model id manually.`;
  }

  return {
    emptyMessage:
      query.length > 0
        ? `No ${session.agent} models match "${query}".`
        : `No ${session.agent} models are available right now.`,
    errorMessage: modelOptionsError,
    hint,
    isRefreshing: isRefreshingModelOptions,
    items,
    refreshActionLabel: modelOptionsError ? "Retry live models" : "Refresh live models",
    supportsLiveRefresh,
    title: `${session.agent} models`,
  };
}

export function buildSlashPaletteState(
  session: SlashPaletteSession | null,
  draft: string,
  isRefreshingModelOptions: boolean,
  modelOptionsError: string | null,
  agentCommands: readonly AgentCommand[],
  hasLoadedAgentCommands: boolean,
  isRefreshingAgentCommands: boolean,
  agentCommandsError: string | null,
): SlashPaletteState {
  if (!session || !draft.startsWith("/")) {
    return { kind: "none" };
  }

  const commandMatch = /^\/(\S*)(?:\s+(.*))?$/.exec(draft);
  if (!commandMatch) {
    return { kind: "none" };
  }

  const commandQuery = (commandMatch[1] ?? "").toLowerCase();
  const optionQuery = (commandMatch[2] ?? "").trim().toLowerCase();
  const rawOptionQuery = (commandMatch[2] ?? "").trim();
  const availableCommands = slashCommandsForSession(session);
  const activeCommand =
    commandQuery.length === 0
      ? null
      : (availableCommands.find((command) => command.label.slice(1).toLowerCase() === commandQuery) ??
          null);

  if (!activeCommand) {
    const supportsAgentCommands = supportsAgentSlashCommands(session);
    const agentCommandItems = supportsAgentCommands
      ? agentCommands
          .filter((command) => matchAgentCommand(command, commandQuery))
          .map<SlashPaletteItem>((command, index) => ({
            command,
            detail: command.description || command.source,
            hasArguments: agentCommandHasArguments(command),
            key: `agent:${command.name}`,
            kind: "agent-command",
            label: agentCommandLabel(command),
            name: command.name,
            sectionLabel: index === 0 ? "Agent Commands" : undefined,
          }))
      : [];
    const sessionCommandItems = availableCommands
      .filter((item) => (commandQuery.length === 0 ? true : matchSlashCommand(item.label, commandQuery)))
      .map<SlashPaletteItem>((item, index) => ({
        command: item.command,
        detail: item.detail,
        key: item.command,
        kind: "command",
        label: item.label,
        sectionLabel: index === 0 ? "Session Controls" : undefined,
      }));
    const items = [...agentCommandItems, ...sessionCommandItems];
    const commandSourceLabel =
      session.agent === "Claude"
        ? "Claude slash commands for this session."
        : "project agent commands from .claude/commands.";
    const statusText = supportsAgentCommands
      ? isRefreshingAgentCommands
        ? `Loading ${commandSourceLabel}`
        : !hasLoadedAgentCommands
          ? `Load ${commandSourceLabel}`
          : agentCommands.length === 0
            ? session.agent === "Claude"
              ? "No Claude slash commands are available for this session."
              : "No project agent commands found in .claude/commands."
            : undefined
      : undefined;

    return {
      defaultActiveIndex: 0,
      emptyMessage: `No slash commands match "/${commandQuery}".`,
      errorMessage: agentCommandsError,
      hint: supportsAgentCommands
        ? "Enter to expand a session control or run an agent command. Esc clears the command line."
        : "Enter to expand a session command. Esc clears the command line.",
      isRefreshing: isRefreshingAgentCommands,
      items,
      kind: "command",
      refreshActionLabel: supportsAgentCommands
        ? agentCommandsError
          ? "Retry agent commands"
          : hasLoadedAgentCommands
            ? "Refresh agent commands"
            : "Load agent commands"
        : undefined,
      resetKey: `command:${session.id}:${commandQuery}:${items.map((item) => item.key).join("|")}:${agentCommandsError ?? ""}:${isRefreshingAgentCommands ? "loading" : hasLoadedAgentCommands ? "loaded" : "idle"}`,
      statusText,
      supportsRefresh: supportsAgentCommands,
      title: "Slash commands",
    };
  }

  const choiceState =
    activeCommand.id === "model"
      ? sessionModelSlashState(
          session,
          optionQuery,
          rawOptionQuery,
          isRefreshingModelOptions,
          modelOptionsError,
        )
      : activeCommand.id === "mode"
        ? sessionModeSlashState(session, rawOptionQuery)
        : activeCommand.id === "sandbox"
          ? session.agent === "Codex"
            ? codexSandboxSlashState(rawOptionQuery, session.sandboxMode ?? "workspace-write")
            : null
          : activeCommand.id === "approvals"
            ? session.agent === "Codex"
              ? codexApprovalSlashState(rawOptionQuery, session.approvalPolicy ?? "never")
              : null
            : activeCommand.id === "effort"
              ? session.agent === "Codex"
                ? codexReasoningEffortSlashState(session, rawOptionQuery)
                : session.agent === "Claude"
                  ? claudeEffortSlashState(session, rawOptionQuery)
                  : null
            : null;

  if (!choiceState) {
    return {
      defaultActiveIndex: 0,
      emptyMessage: `${activeCommand.label} is not available for ${session.agent}.`,
      hint: "Choose a different slash command for this session.",
      items: [],
      kind: "command",
      resetKey: `command:${commandQuery}`,
      title: "Slash commands",
    };
  }

  const defaultActiveIndex = Math.max(
    choiceState.items.findIndex((item) => item.kind === "choice" && item.isCurrent),
    0,
  );
  const currentChoiceKeys = choiceState.items
    .filter((item) => item.kind === "choice" && item.isCurrent)
    .map((item) => item.key)
    .join("|");

  return {
    defaultActiveIndex,
    emptyMessage: choiceState.emptyMessage,
    errorMessage: choiceState.errorMessage,
    hint: choiceState.hint,
    isRefreshing: choiceState.isRefreshing ?? false,
    items: choiceState.items,
    kind: "choice",
    refreshActionLabel: choiceState.refreshActionLabel,
    resetKey: `${activeCommand.id}:${session.id}:${optionQuery}:${currentChoiceKeys}:${choiceState.items.map((item) => item.key).join("|")}:${choiceState.errorMessage ?? ""}:${choiceState.isRefreshing ? "loading" : "ready"}`,
    supportsLiveRefresh: choiceState.supportsLiveRefresh ?? false,
    title: choiceState.title,
  };
}
