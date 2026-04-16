import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { ExpandedPromptPanel } from "../ExpandedPromptPanel";
import { matchingSessionModelOption } from "../session-model-options";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "../search-highlight";
import type {
  ApprovalDecision,
  AgentCommand,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CommandMessage,
  CodexReasoningEffort,
  CursorMode,
  DiffMessage,
  GeminiApprovalMode,
  ImageAttachment,
  JsonValue,
  Message,
  McpElicitationAction,
  PendingPrompt,
  SandboxMode,
  Session,
} from "../types";
import type { PaneViewMode } from "../workspace";

type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

type UserInputSubmitHandler = (
  sessionId: string,
  messageId: string,
  answers: Record<string, string[]>,
) => void;

type BoundUserInputSubmitHandler = (
  messageId: string,
  answers: Record<string, string[]>,
) => void;

type McpElicitationSubmitHandler = (
  sessionId: string,
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

type BoundMcpElicitationSubmitHandler = (
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

type CodexAppRequestSubmitHandler = (
  sessionId: string,
  messageId: string,
  result: JsonValue,
) => void;

type BoundCodexAppRequestSubmitHandler = (messageId: string, result: JsonValue) => void;

type RenderMessageCard = (
  message: Message,
  preferImmediateHeavyRender: boolean,
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  onUserInputSubmit: BoundUserInputSubmitHandler,
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler,
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler,
) => JSX.Element | null;

type PromptHistoryState = {
  index: number;
  draft: string;
};

type SessionSettingsField =
  | "model"
  | "sandboxMode"
  | "approvalPolicy"
  | "reasoningEffort"
  | "claudeApprovalMode"
  | "claudeEffort"
  | "cursorMode"
  | "geminiApprovalMode";
type SessionSettingsValue =
  | string
  | SandboxMode
  | ApprovalPolicy
  | ClaudeEffortLevel
  | CodexReasoningEffort
  | ClaudeApprovalMode
  | CursorMode
  | GeminiApprovalMode;

const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES = 80;
const VIRTUALIZED_MESSAGE_OVERSCAN_PX = 960;
const VIRTUALIZED_MESSAGE_GAP_PX = 12;
const DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT = 720;
const DEFAULT_ESTIMATED_MESSAGE_HEIGHT = 180;
/** Maximum number of messages to render initially. Older messages load on scroll-up. */
const RENDER_WINDOW_INITIAL_SIZE = 200;
/** Number of additional messages to prepend when the user scrolls near the top. */
const RENDER_WINDOW_LOAD_MORE = 200;
/** Scroll-top threshold (px) at which older messages are loaded. */
const RENDER_WINDOW_LOAD_MORE_THRESHOLD_PX = 400;
const EMPTY_MATCHED_ITEM_KEYS = new Set<string>();
const STATIC_MODEL_OPTIONS: Readonly<Record<Session["agent"], readonly SessionModelChoice[]>> = {
  Claude: [],
  Codex: [],
  Cursor: [{ detail: "Auto", label: "Auto", value: "auto" }],
  Gemini: [{ detail: "Auto", label: "Auto", value: "auto" }],
};
const SANDBOX_SLASH_OPTIONS = [
  { detail: "Write inside the workspace", label: "workspace-write", value: "workspace-write" },
  { detail: "Read without editing files", label: "read-only", value: "read-only" },
  { detail: "Allow unrestricted file access", label: "danger-full-access", value: "danger-full-access" },
] as const;
const APPROVAL_POLICY_SLASH_OPTIONS = [
  { detail: "Never ask before tools run", label: "never", value: "never" },
  { detail: "Ask whenever a tool needs approval", label: "on-request", value: "on-request" },
  { detail: "Approve trusted commands, ask for the rest", label: "untrusted", value: "untrusted" },
  { detail: "Only ask after failures", label: "on-failure", value: "on-failure" },
] as const;
const CODEX_REASONING_EFFORT_SLASH_OPTIONS = [
  { detail: "Disable reasoning for speed-sensitive turns", label: "none", value: "none" },
  { detail: "Use the lightest reasoning pass", label: "minimal", value: "minimal" },
  { detail: "Keep reasoning light", label: "low", value: "low" },
  { detail: "Use the standard reasoning depth", label: "medium", value: "medium" },
  { detail: "Use deeper reasoning for harder prompts", label: "high", value: "high" },
  { detail: "Use the maximum reasoning depth", label: "xhigh", value: "xhigh" },
] as const;
const CLAUDE_MODE_SLASH_OPTIONS = [
  { detail: "Ask before tool use", label: "ask", value: "ask" },
  { detail: "Continue through tool requests", label: "auto-approve", value: "auto-approve" },
  { detail: "Stay read-only and plan", label: "plan", value: "plan" },
] as const;
const CLAUDE_EFFORT_SLASH_OPTIONS = [
  { detail: "Use Claude's default effort for this session", label: "default", value: "default" },
  { detail: "Keep reasoning light", label: "low", value: "low" },
  { detail: "Use the standard effort level", label: "medium", value: "medium" },
  { detail: "Use deeper reasoning for harder prompts", label: "high", value: "high" },
  { detail: "Use the highest available effort", label: "max", value: "max" },
] as const;
const CURSOR_MODE_SLASH_OPTIONS = [
  { detail: "Allow edits and auto-approve tools", label: "agent", value: "agent" },
  { detail: "Stay read-only and deny tools", label: "plan", value: "plan" },
  { detail: "Show approval cards before tools", label: "ask", value: "ask" },
] as const;
const GEMINI_MODE_SLASH_OPTIONS = [
  { detail: "Ask before tool use", label: "default", value: "default" },
  { detail: "Auto-approve edit tools", label: "auto_edit", value: "auto_edit" },
  { detail: "Approve every tool", label: "yolo", value: "yolo" },
  { detail: "Stay read-only and plan", label: "plan", value: "plan" },
] as const;

type SlashCommandId = "model" | "mode" | "sandbox" | "approvals" | "effort";

const SLASH_COMMANDS: ReadonlyArray<{
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

type SessionModelChoice = {
  detail: string;
  label: string;
  value: string;
};

type SlashPaletteItem =
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

type SlashPaletteState =
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

type SlashChoiceDefinition = {
  detail: string;
  label: string;
  value: string;
};

type SlashChoiceState = {
  emptyMessage: string;
  errorMessage?: string | null;
  hint: string;
  isRefreshing?: boolean;
  items: SlashPaletteItem[];
  refreshActionLabel?: string;
  supportsLiveRefresh?: boolean;
  title: string;
};

function formatSessionModelLabel(model: string): string {
  if (model === "auto") {
    return "Auto";
  }
  if (model === "default") {
    return "Default";
  }
  return model;
}

function isSpaceKey(event: {
  key: string;
  code?: string;
  keyCode?: number;
  which?: number;
}) {
  return (
    event.key === " " ||
    event.key === "Space" ||
    event.key === "Spacebar" ||
    event.code === "Space" ||
    event.keyCode === 32 ||
    event.which === 32
  );
}

const ALL_CODEX_REASONING_EFFORTS = CODEX_REASONING_EFFORT_SLASH_OPTIONS.map(
  (option) => option.value,
) as CodexReasoningEffort[];
const DEFAULT_CLAUDE_EFFORT: ClaudeEffortLevel = "default";
const FALLBACK_CLAUDE_EFFORTS = ["low", "medium", "high"] as ClaudeEffortLevel[];

function codexReasoningEffortChoice(effort: CodexReasoningEffort) {
  return (
    CODEX_REASONING_EFFORT_SLASH_OPTIONS.find((option) => option.value === effort) ?? {
      detail: effort,
      label: effort,
      value: effort,
    }
  );
}

function claudeEffortChoice(effort: ClaudeEffortLevel) {
  return (
    CLAUDE_EFFORT_SLASH_OPTIONS.find((option) => option.value === effort) ?? {
      detail: effort,
      label: effort,
      value: effort,
    }
  );
}

function currentSessionModelCapabilities(session: Session) {
  return matchingSessionModelOption(session.modelOptions, session.model);
}

function supportedCodexReasoningEfforts(session: Session) {
  const option = currentSessionModelCapabilities(session);
  return option?.supportedReasoningEfforts?.length
    ? option.supportedReasoningEfforts
    : ALL_CODEX_REASONING_EFFORTS;
}

function defaultCodexReasoningEffort(session: Session) {
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

function currentClaudeEffort(session: Session): ClaudeEffortLevel {
  return session.claudeEffort ?? DEFAULT_CLAUDE_EFFORT;
}

function supportedClaudeEffortLevels(session: Session) {
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

function claudeEffortChoices(session: Session): SlashChoiceDefinition[] {
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

function codexReasoningEffortChoices(session: Session): SlashChoiceDefinition[] {
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

function sessionModelChoiceDetail(
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

function slashCommandsForSession(session: Session) {
  return SLASH_COMMANDS.filter((command) => command.supports.includes(session.agent));
}

function supportsAgentSlashCommands(_session: Session): boolean {
  return true;
}

function normalizedAgentCommandKind(command: AgentCommand) {
  return command.kind ?? "promptTemplate";
}

function agentCommandLabel(command: AgentCommand) {
  return `/${command.name}`;
}

function agentCommandHasArguments(command: AgentCommand) {
  return normalizedAgentCommandKind(command) === "nativeSlash"
    ? Boolean(command.argumentHint?.trim())
    : command.content.includes("$ARGUMENTS");
}

function matchAgentCommand(command: AgentCommand, query: string) {
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

function parseAgentCommandDraft(draft: string) {
  const match = /^\/(\S+)(?:\s([\s\S]*))?$/.exec(draft);
  if (!match) {
    return null;
  }

  return {
    argumentsText: match[2] ?? "",
    commandName: match[1] ?? "",
  };
}

function supportsLiveSessionModelOptions(session: Session): boolean {
  return (
    session.agent === "Claude" ||
    session.agent === "Codex" ||
    session.agent === "Cursor" ||
    session.agent === "Gemini"
  );
}

function ensureCurrentSessionModelChoice(
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

function sessionModelChoicesForSlashCommand(session: Session): SessionModelChoice[] {
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

function manualSessionModelSlashItem(
  options: readonly SessionModelChoice[],
  session: Session,
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

function matchSlashCommand(commandLabel: string, query: string) {
  return commandLabel.slice(1).toLowerCase().includes(query);
}

function matchSlashChoice(choice: SlashChoiceDefinition, query: string) {
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

function makeSlashChoices(
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

function sessionModeSlashState(session: Session, query: string): SlashChoiceState | null {
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

function codexSandboxSlashState(query: string, currentValue: SandboxMode): SlashChoiceState {
  return {
    emptyMessage: `No sandbox modes match "${query}".`,
    hint: "Enter to set the next Codex prompt sandbox.",
    items: makeSlashChoices(SANDBOX_SLASH_OPTIONS, "sandboxMode", currentValue, query),
    title: "Codex sandbox",
  };
}

function codexApprovalSlashState(query: string, currentValue: ApprovalPolicy): SlashChoiceState {
  return {
    emptyMessage: `No approval policies match "${query}".`,
    hint: "Enter to set the next Codex prompt approval policy.",
    items: makeSlashChoices(APPROVAL_POLICY_SLASH_OPTIONS, "approvalPolicy", currentValue, query),
    title: "Codex approvals",
  };
}

function codexReasoningEffortSlashState(session: Session, query: string): SlashChoiceState {
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

function claudeEffortSlashState(session: Session, query: string): SlashChoiceState {
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

function sessionModelSlashState(
  session: Session,
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

function buildSlashPaletteState(
  session: Session | null,
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

export function AgentSessionPanel({
  paneId,
  viewMode,
  activeSession,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  mountedSessions,
  commandMessages,
  diffMessages,
  scrollContainerRef,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onSessionSettingsChange,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
  renderCommandCard,
  renderDiffCard,
  renderMessageCard,
  renderPromptSettings,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  activeSession: Session | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  mountedSessions: Session[];
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onSessionSettingsChange: (
      sessionId: string,
      field: SessionSettingsField,
      value: SessionSettingsValue,
    ) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
  renderCommandCard: (message: CommandMessage) => JSX.Element | null;
  renderDiffCard: (message: DiffMessage) => JSX.Element | null;
  renderMessageCard: RenderMessageCard;
  renderPromptSettings: (
    paneId: string,
    session: Session,
    isUpdating: boolean,
    onSessionSettingsChange: (
      sessionId: string,
      field: SessionSettingsField,
      value: SessionSettingsValue,
    ) => void,
  ) => JSX.Element | null;
}): JSX.Element {
  return (
    <SessionBody
      paneId={paneId}
      viewMode={viewMode}
      scrollContainerRef={scrollContainerRef}
      activeSession={activeSession}
      isLoading={isLoading}
      isUpdating={isUpdating}
      showWaitingIndicator={showWaitingIndicator}
      waitingIndicatorPrompt={waitingIndicatorPrompt}
      mountedSessions={mountedSessions}
      commandMessages={commandMessages}
      diffMessages={diffMessages}
      onApprovalDecision={onApprovalDecision}
      onUserInputSubmit={onUserInputSubmit}
      onMcpElicitationSubmit={onMcpElicitationSubmit}
      onCodexAppRequestSubmit={onCodexAppRequestSubmit}
      onCancelQueuedPrompt={onCancelQueuedPrompt}
      onSessionSettingsChange={onSessionSettingsChange}
      conversationSearchQuery={conversationSearchQuery}
      conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
      conversationSearchActiveItemKey={conversationSearchActiveItemKey}
      onConversationSearchItemMount={onConversationSearchItemMount}
      renderCommandCard={renderCommandCard}
      renderDiffCard={renderDiffCard}
      renderMessageCard={renderMessageCard}
      renderPromptSettings={renderPromptSettings}
    />
  );
}

export function AgentSessionPanelFooter({
  paneId,
  viewMode,
  isPaneActive,
  activeSession,
  committedDraft,
  draftAttachments,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  isUpdating,
  showNewResponseIndicator,
  footerModeLabel,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  isRefreshingModelOptions,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onSend,
  onSessionSettingsChange,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  isPaneActive: boolean;
  activeSession: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  isUpdating: boolean;
  showNewResponseIndicator: boolean;
  footerModeLabel: string;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  agentCommands: AgentCommand[];
  hasLoadedAgentCommands: boolean;
  isRefreshingAgentCommands: boolean;
  agentCommandsError: string | null;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}): JSX.Element {
  if (viewMode === "session") {
    return (
      <SessionComposer
        paneId={paneId}
        isPaneActive={isPaneActive}
        session={activeSession}
        committedDraft={committedDraft}
        draftAttachments={draftAttachments}
        formatByteSize={formatByteSize}
        isSending={isSending}
        isStopping={isStopping}
        isSessionBusy={isSessionBusy}
        isUpdating={isUpdating}
        showNewResponseIndicator={showNewResponseIndicator}
        onScrollToLatest={onScrollToLatest}
        onDraftCommit={onDraftCommit}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        isRefreshingModelOptions={isRefreshingModelOptions}
        modelOptionsError={modelOptionsError}
        agentCommands={agentCommands}
        hasLoadedAgentCommands={hasLoadedAgentCommands}
        isRefreshingAgentCommands={isRefreshingAgentCommands}
        agentCommandsError={agentCommandsError}
        onRefreshSessionModelOptions={onRefreshSessionModelOptions}
        onRefreshAgentCommands={onRefreshAgentCommands}
        onSend={onSend}
        onSessionSettingsChange={onSessionSettingsChange}
        onStopSession={onStopSession}
        onPaste={onPaste}
      />
    );
  }

  return (
    <footer className="pane-footer-note">
      <p className="composer-hint">
        This tile is in {footerModeLabel.toLowerCase()} mode. Use the Session tab to send prompts.
      </p>
    </footer>
  );
}

const SessionBody = memo(function SessionBody({
  paneId,
  viewMode,
  scrollContainerRef,
  activeSession,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  mountedSessions,
  commandMessages,
  diffMessages,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onSessionSettingsChange,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
  renderCommandCard,
  renderDiffCard,
  renderMessageCard,
  renderPromptSettings,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  scrollContainerRef: RefObject<HTMLElement | null>;
  activeSession: Session | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  mountedSessions: Session[];
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
  renderCommandCard: (message: CommandMessage) => JSX.Element | null;
  renderDiffCard: (message: DiffMessage) => JSX.Element | null;
  renderMessageCard: RenderMessageCard;
  renderPromptSettings: (
    paneId: string,
    session: Session,
    isUpdating: boolean,
    onSessionSettingsChange: (
      sessionId: string,
      field: SessionSettingsField,
      value: SessionSettingsValue,
    ) => void,
  ) => JSX.Element | null;
}): JSX.Element | null {
  // Stabilize render callbacks so children receive a constant function identity.
  // The latest version is always called through the ref, so closures stay fresh
  // even though the wrapper identity never changes.
  const renderMessageCardRef = useRef(renderMessageCard);
  renderMessageCardRef.current = renderMessageCard;
  const stableRenderMessageCard = useCallback<RenderMessageCard>(
    (...args) => renderMessageCardRef.current(...args),
    [],
  );

  const renderCommandCardRef = useRef(renderCommandCard);
  renderCommandCardRef.current = renderCommandCard;
  const stableRenderCommandCard = useCallback(
    (message: CommandMessage) => renderCommandCardRef.current(message),
    [],
  );

  const renderDiffCardRef = useRef(renderDiffCard);
  renderDiffCardRef.current = renderDiffCard;
  const stableRenderDiffCard = useCallback(
    (message: DiffMessage) => renderDiffCardRef.current(message),
    [],
  );

  const renderPromptSettingsRef = useRef(renderPromptSettings);
  renderPromptSettingsRef.current = renderPromptSettings;

  if (!activeSession) {
    return (
      <PanelEmptyState
        title="Ready for a session"
        body="Click a session on the left to open it in the active tile."
      />
    );
  }

  if (viewMode === "session") {
    const activePendingPrompts = activeSession.pendingPrompts ?? [];
    if (activeSession.messages.length === 0 && activePendingPrompts.length === 0 && !showWaitingIndicator) {
      return (
        <PanelEmptyState
          title={isLoading ? "Connecting to backend" : "Live session is ready"}
          body={
            isLoading
              ? "Fetching session state from the Rust backend."
              : `Send a prompt to ${activeSession.agent} and this tile will fill with live cards.`
          }
        />
      );
    }

    return (
      <>
        {mountedSessions.map((session) => (
          <SessionConversationPage
            key={session.id}
            renderMessageCard={stableRenderMessageCard}
            session={session}
            scrollContainerRef={scrollContainerRef}
            isActive={session.id === activeSession.id}
            isLoading={isLoading && session.id === activeSession.id}
            showWaitingIndicator={showWaitingIndicator && session.id === activeSession.id}
            waitingIndicatorPrompt={session.id === activeSession.id ? waitingIndicatorPrompt : null}
            onApprovalDecision={onApprovalDecision}
            onUserInputSubmit={onUserInputSubmit}
            onMcpElicitationSubmit={onMcpElicitationSubmit}
            onCodexAppRequestSubmit={onCodexAppRequestSubmit}
            onCancelQueuedPrompt={onCancelQueuedPrompt}
            conversationSearchQuery={session.id === activeSession.id ? conversationSearchQuery : ""}
            conversationSearchMatchedItemKeys={
              session.id === activeSession.id ? conversationSearchMatchedItemKeys : EMPTY_MATCHED_ITEM_KEYS
            }
            conversationSearchActiveItemKey={
              session.id === activeSession.id ? conversationSearchActiveItemKey : null
            }
            onConversationSearchItemMount={onConversationSearchItemMount}
          />
        ))}
      </>
    );
  }

  if (viewMode === "prompt") {
    return renderPromptSettingsRef.current(paneId, activeSession, isUpdating, onSessionSettingsChange) ?? (
      <PanelEmptyState
        title="No prompt settings"
        body="Prompt controls are only available for supported agent sessions."
      />
    );
  }

  if (viewMode === "commands") {
    return commandMessages.length > 0 ? (
      <>
        {commandMessages.map((message) => (
          <MessageSlot key={message.id}>{stableRenderCommandCard(message)}</MessageSlot>
        ))}
      </>
    ) : (
      <PanelEmptyState
        title="No commands yet"
        body="This tile is filtered to command executions. Send a prompt that runs tools and they will show up here."
      />
    );
  }

  if (viewMode === "diffs") {
    return diffMessages.length > 0 ? (
      <>
        {diffMessages.map((message) => (
          <MessageSlot key={message.id}>{stableRenderDiffCard(message)}</MessageSlot>
        ))}
      </>
    ) : (
      <PanelEmptyState
        title="No diffs yet"
        body="This tile is filtered to file changes. When the agent edits or creates files, the diffs will appear here."
      />
    );
  }

  return null;
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.viewMode === next.viewMode &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.activeSession === next.activeSession &&
  previous.isLoading === next.isLoading &&
  previous.isUpdating === next.isUpdating &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.mountedSessions === next.mountedSessions &&
  previous.commandMessages === next.commandMessages &&
  previous.diffMessages === next.diffMessages &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.conversationSearchQuery === next.conversationSearchQuery &&
  previous.conversationSearchMatchedItemKeys === next.conversationSearchMatchedItemKeys &&
  previous.conversationSearchActiveItemKey === next.conversationSearchActiveItemKey &&
  previous.onConversationSearchItemMount === next.onConversationSearchItemMount
  // Render callbacks (renderMessageCard, renderDiffCard, renderCommandCard,
  // renderPromptSettings) are intentionally excluded — they are inline closures
  // whose identity changes every render. SessionBody wraps them in stable refs
  // so children always call the latest version without triggering re-renders.
);

const SessionConversationPage = memo(function SessionConversationPage({
  renderMessageCard,
  session,
  scrollContainerRef,
  isActive,
  isLoading,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: RenderMessageCard;
  session: Session;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  isLoading: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  const pendingPrompts = session.pendingPrompts ?? [];

  if (session.messages.length === 0 && pendingPrompts.length === 0 && !showWaitingIndicator) {
    return (
      <div className={`session-conversation-page${isActive ? " is-active" : ""}`} hidden={!isActive}>
        <PanelEmptyState
          title={isLoading ? "Connecting to backend" : "Live session is ready"}
          body={
            isLoading
              ? "Fetching session state from the Rust backend."
              : `Send a prompt to ${session.agent} and this tile will fill with live cards.`
          }
        />
      </div>
    );
  }

  return (
    <div className={`session-conversation-page${isActive ? " is-active" : ""}`} hidden={!isActive}>
      <ConversationMessageList
        renderMessageCard={renderMessageCard}
        sessionId={session.id}
        messages={session.messages}
        scrollContainerRef={scrollContainerRef}
        isActive={isActive}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={onUserInputSubmit}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
        onCodexAppRequestSubmit={onCodexAppRequestSubmit}
        conversationSearchQuery={conversationSearchQuery}
        conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
        conversationSearchActiveItemKey={conversationSearchActiveItemKey}
        onConversationSearchItemMount={onConversationSearchItemMount}
      />

      {showWaitingIndicator ? (
        <RunningIndicator agent={session.agent} lastPrompt={waitingIndicatorPrompt} />
      ) : null}

      {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
      {pendingPrompts.map((prompt) => (
        <MessageSlot
          key={prompt.id}
          itemKey={isActive ? `pendingPrompt:${prompt.id}` : undefined}
          isSearchMatch={conversationSearchMatchedItemKeys.has(`pendingPrompt:${prompt.id}`)}
          isSearchActive={conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}`}
          onSearchItemMount={onConversationSearchItemMount}
        >
          <PendingPromptCard
            prompt={prompt}
            onCancel={() => onCancelQueuedPrompt(session.id, prompt.id)}
            searchQuery={
              conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}` ? conversationSearchQuery : ""
            }
            searchHighlightTone={
              conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}` ? "active" : "match"
            }
          />
        </MessageSlot>
      ))}
    </div>
  );
}, (previous, next) =>
  previous.renderMessageCard === next.renderMessageCard &&
  previous.session === next.session &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.isActive === next.isActive &&
  previous.isLoading === next.isLoading &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
  previous.conversationSearchQuery === next.conversationSearchQuery &&
  previous.conversationSearchMatchedItemKeys === next.conversationSearchMatchedItemKeys &&
  previous.conversationSearchActiveItemKey === next.conversationSearchActiveItemKey &&
  previous.onConversationSearchItemMount === next.onConversationSearchItemMount
);

function ConversationMessageList({
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  isActive,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: RenderMessageCard;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  const hasConversationSearch = conversationSearchQuery.trim().length > 0;

  if (hasConversationSearch || messages.length < CONVERSATION_VIRTUALIZATION_MIN_MESSAGES) {
    return (
      <>
        {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
        {messages.map((message, index) => (
          <MessageSlot
            key={message.id}
            itemKey={isActive ? `message:${message.id}` : undefined}
            isSearchMatch={conversationSearchMatchedItemKeys.has(`message:${message.id}`)}
            isSearchActive={conversationSearchActiveItemKey === `message:${message.id}`}
            onSearchItemMount={onConversationSearchItemMount}
          >
            {renderMessageCard(
              message,
              isActive && index >= messages.length - 2,
              (messageId, decision) => onApprovalDecision(sessionId, messageId, decision),
              (messageId, answers) => onUserInputSubmit(sessionId, messageId, answers),
              (messageId, action, content) =>
                onMcpElicitationSubmit(sessionId, messageId, action, content),
              (messageId, result) => onCodexAppRequestSubmit(sessionId, messageId, result),
            )}
          </MessageSlot>
        ))}
      </>
    );
  }

  return (
    <VirtualizedConversationMessageList
      isActive={isActive}
      renderMessageCard={renderMessageCard}
      sessionId={sessionId}
      messages={messages}
      scrollContainerRef={scrollContainerRef}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
        />
  );
}

export function VirtualizedConversationMessageList({
  isActive,
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
}: {
  isActive: boolean;
  renderMessageCard: RenderMessageCard;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
}) {
  const messageHeightsRef = useRef<Record<string, number>>({});
  // Expected behavior: when the viewport is already at the latest message,
  // virtualized height measurements should keep the viewport pinned there.
  const shouldKeepBottomAfterLayoutRef = useRef(false);
  const [viewport, setViewport] = useState({
    height: DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
    scrollTop: 0,
  });
  const [layoutVersion, setLayoutVersion] = useState(0);
  // Post-activation measurement phase: when the list transitions from
  // inactive to active (or mounts directly in the active state with
  // messages already present), the first render uses estimated heights
  // for any messages that were added while the list was inactive. Those
  // estimates drive `layout.tops` and `layout.totalHeight`, so the first
  // paint positions cards at the wrong absolute offsets. We mark the
  // list as "measuring", hide the wrapper via CSS (`visibility: hidden`)
  // so slots still mount and `ResizeObserver` can fire, and clear the
  // flag once the currently-visible slots have real measurements in
  // `messageHeightsRef` — then reveal with the correct scrollTop. A
  // 150 ms timeout guarantees the wrapper is revealed even if some
  // slots take longer than expected to report their first measurement.
  const [isMeasuringPostActivation, setIsMeasuringPostActivation] = useState(
    () => isActive && messages.length > 0,
  );
  const wasActiveRef = useRef(isActive);

  // Render window: only keep the last N messages in the layout to avoid
  // computing positions for thousands of off-screen items. Older messages
  // are loaded on scroll-up.
  const [renderWindowSize, setRenderWindowSize] = useState(
    Math.min(RENDER_WINDOW_INITIAL_SIZE, messages.length),
  );
  const prevMessageCountRef = useRef(messages.length);
  // When new messages arrive (agent responding), keep the window anchored
  // to the bottom by expanding it to cover the new messages.
  if (messages.length > prevMessageCountRef.current) {
    const growth = messages.length - prevMessageCountRef.current;
    if (renderWindowSize + growth <= messages.length) {
      // Expand window by the number of new messages so they're visible.
      // This runs during render (before effects) so the layout is correct
      // on the first paint.
      setRenderWindowSize((prev) => Math.min(prev + growth, messages.length));
    }
  }
  prevMessageCountRef.current = messages.length;

  const windowStartIndex = Math.max(0, messages.length - renderWindowSize);
  const windowedMessages = useMemo(
    () => messages.slice(windowStartIndex),
    [messages, windowStartIndex],
  );
  const hasOlderMessages = windowStartIndex > 0;

  const messageIndexById = useMemo(
    () => new Map(windowedMessages.map((message, index) => [message.id, index])),
    [windowedMessages],
  );
  const messageHeights = useMemo(
    () =>
      windowedMessages.map(
        (message) => messageHeightsRef.current[message.id] ?? estimateConversationMessageHeight(message),
      ),
    [layoutVersion, windowedMessages],
  );
  const layout = useMemo(() => buildVirtualizedMessageLayout(messageHeights), [messageHeights]);
  const layoutTopsRef = useRef(layout.tops);
  layoutTopsRef.current = layout.tops;
  const activeViewport = isActive ? scrollContainerRef.current : null;
  const viewportHeight =
    activeViewport?.clientHeight && activeViewport.clientHeight > 0
      ? activeViewport.clientHeight
      : viewport.height;
  const viewportScrollTop = activeViewport ? activeViewport.scrollTop : viewport.scrollTop;
  const visibleRange = useMemo(
    () =>
      findVirtualizedMessageRange(
        layout.tops,
        messageHeights,
        viewportScrollTop,
        viewportHeight,
        VIRTUALIZED_MESSAGE_OVERSCAN_PX,
      ),
    [layout.tops, messageHeights, viewportHeight, viewportScrollTop],
  );

  useEffect(() => {
    messageHeightsRef.current = Object.fromEntries(
      windowedMessages
        .filter((message) => messageHeightsRef.current[message.id] !== undefined)
        .map((message) => [message.id, messageHeightsRef.current[message.id] as number]),
    );
  }, [windowedMessages]);

  // Enter the measuring phase on inactive → active transitions while
  // mounted. The initial mount case is handled by the `useState`
  // initializer above; this effect covers the case where the same
  // instance is flipped inactive → active as the user switches tabs.
  useLayoutEffect(() => {
    if (!wasActiveRef.current && isActive && messages.length > 0) {
      setIsMeasuringPostActivation(true);
    }
    wasActiveRef.current = isActive;
  }, [isActive, messages.length]);

  // Arm the bottom-pin flag while measuring so the existing re-pin
  // `useLayoutEffect` (declared below) writes the correct scrollTop
  // after each measurement commit. Declared BEFORE the re-pin effect so
  // that it runs first in the commit phase and the flag is set by the
  // time the re-pin reads it.
  useLayoutEffect(() => {
    if (isMeasuringPostActivation) {
      shouldKeepBottomAfterLayoutRef.current = true;
    }
  }, [isMeasuringPostActivation]);

  // Completion check: intentionally runs on every commit while measuring is
  // active. Measurements mutate `messageHeightsRef` synchronously and then
  // trigger ordinary React commits, so an explicit dependency list is more
  // likely to miss a ref-only measurement than to make this safer.
  // When all currently-visible slots have real measurements, write a
  // final scrollTop and reveal by clearing the flag. Reading from
  // `messageHeightsRef.current` is safe because `handleHeightChange`
  // mutates it synchronously before bumping `layoutVersion`, so by the
  // time this effect runs after a measurement-driven commit, the ref
  // already reflects the latest data.
  useLayoutEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }
    if (!isActive) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const visibleMessages = windowedMessages.slice(
      visibleRange.startIndex,
      visibleRange.endIndex,
    );
    if (visibleMessages.length === 0) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const allMeasured = visibleMessages.every(
      (message) => messageHeightsRef.current[message.id] !== undefined,
    );
    if (!allMeasured) {
      return;
    }

    const node = scrollContainerRef.current;
    if (node) {
      const target = Math.max(node.scrollHeight - node.clientHeight, 0);
      if (Math.abs(node.scrollTop - target) >= 1) {
        node.scrollTop = target;
      }
    }
    setIsMeasuringPostActivation(false);
  });

  // Timeout fallback: if the visible slots take longer than 150 ms to
  // report their first measurement (e.g., a heavy syntax-highlighted
  // code block or a deferred markdown renderer), reveal the wrapper
  // anyway so the user sees something rather than a blank region.
  useEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      const node = scrollContainerRef.current;
      if (node) {
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        if (Math.abs(node.scrollTop - target) >= 1) {
          node.scrollTop = target;
        }
      }
      shouldKeepBottomAfterLayoutRef.current = true;
      setIsMeasuringPostActivation(false);
    }, 150);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [isMeasuringPostActivation, scrollContainerRef]);

  useLayoutEffect(() => {
    if (!isActive || !shouldKeepBottomAfterLayoutRef.current) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    // Re-pin to the new bottom on every commit while the flag is set.
    // The flag is cleared only in the scroll handler (see `syncViewport`
    // below) when the user explicitly scrolls away from near-bottom.
    //
    // The round 8 pre-fix version cleared this flag here after observing
    // `isScrollContainerAtBottom(node)`, but that ran in the SAME commit
    // the flag was set in (via `handleHeightChange` → `setLayoutVersion`).
    // At that point the DOM reflected the OLD `scrollHeight` — the
    // wrapper's `style={{ height: layout.totalHeight }}` had not yet
    // committed the new total height — so `isScrollContainerAtBottom`
    // reported true against the stale geometry, cleared the flag, and
    // the *next* commit (the one that actually committed the new
    // height) found the flag cleared and bailed out of the re-pin. The
    // net effect was leaving the user ~10-100 px above the new bottom
    // on the exact measurement-driven growth scenario the round 8 fix
    // was added to handle. Clearing is delegated to scroll events so
    // the pin survives across as many commits as the measurement cycle
    // needs, matching the `TerminalPanel.shouldStickToBottomRef`
    // sticky-until-user-scrolls-away pattern.
    //
    // The `>= 1` guard prevents no-op writes from triggering a scroll
    // event + reflow + ResizeObserver tick cascade. When the layout
    // commit only moved `scrollHeight` by the subpixel rounding delta,
    // the computed target matches the current scrollTop and we do
    // nothing instead of writing a value the browser would round to the
    // same integer anyway.
    const target = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (Math.abs(node.scrollTop - target) >= 1) {
      node.scrollTop = target;
    }
  }, [isActive, layout.totalHeight, scrollContainerRef]);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const syncViewport = () => {
      const nextState = {
        height: node.clientHeight > 0 ? node.clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
        scrollTop: node.scrollTop,
      };

      setViewport((current) =>
        current.height === nextState.height && current.scrollTop === nextState.scrollTop
          ? current
          : nextState,
      );

      // Clear the pin flag once the user has scrolled away from
      // near-bottom. Mirrors `TerminalPanel.shouldStickToBottomRef`'s
      // `onScroll` update: sticky stays on until the user explicitly
      // moves away, then re-arms only when a later `handleHeightChange`
      // observes the viewport back near the bottom. This is the only
      // site that clears the flag — the re-pinning `useLayoutEffect`
      // above intentionally leaves it set so a single measurement
      // growth can survive the same-commit `setLayoutVersion` →
      // follow-up-commit re-render without tearing down the pin.
      if (
        shouldKeepBottomAfterLayoutRef.current &&
        !isScrollContainerNearBottom(node)
      ) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
    };

    syncViewport();
    node.addEventListener("scroll", syncViewport, { passive: true });
    const resizeObserver = new ResizeObserver(syncViewport);
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", syncViewport);
      resizeObserver.disconnect();
    };
  }, [isActive, scrollContainerRef, sessionId]);

  // Load older messages when the user scrolls near the top of the window.
  useEffect(() => {
    if (!isActive || !hasOlderMessages) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    function handleScroll() {
      if (!node || node.scrollTop > RENDER_WINDOW_LOAD_MORE_THRESHOLD_PX) {
        return;
      }

      // Expand the window. Adjust scrollTop so the current view stays
      // in place instead of jumping when new items are prepended.
      const prevScrollHeight = node.scrollHeight;
      setRenderWindowSize((prev) => Math.min(prev + RENDER_WINDOW_LOAD_MORE, messages.length));
      // The layout change happens asynchronously after React re-renders.
      // Use a RAF to read the new scrollHeight and adjust.
      requestAnimationFrame(() => {
        if (!node) {
          return;
        }
        const heightDelta = node.scrollHeight - prevScrollHeight;
        if (heightDelta > 0) {
          node.scrollTop += heightDelta;
        }
      });
    }

    node.addEventListener("scroll", handleScroll, { passive: true });
    return () => {
      node.removeEventListener("scroll", handleScroll);
    };
  }, [hasOlderMessages, isActive, messages.length, scrollContainerRef]);

  // Stabilize handler references so MeasuredMessageCard memo can skip
  // re-renders for messages whose data and position haven't changed.
  const messagesRef = useRef(windowedMessages);
  messagesRef.current = windowedMessages;
  const messageIndexByIdRef = useRef(messageIndexById);
  messageIndexByIdRef.current = messageIndexById;

  const handleHeightChange = useCallback((messageId: string, nextHeight: number) => {
    if (!Number.isFinite(nextHeight) || nextHeight <= 0) {
      return;
    }

    // Round to integer pixels so subpixel drift from `getBoundingClientRect`
    // cannot repeatedly cross the 1-pixel commit threshold below. Storing
    // fractional heights caused a cascading shake: each float-level
    // measurement committed, bumped `layoutVersion`, re-pinned `scrollTop`,
    // reflowed, and then reported a slightly different fractional height on
    // the next ResizeObserver tick. Integer storage eliminates the drift at
    // the source so the re-pin loop cannot be re-entered by noise alone.
    const roundedHeight = Math.round(nextHeight);
    const hadPreviousMeasurement = messageHeightsRef.current[messageId] !== undefined;
    const previousHeight =
      messageHeightsRef.current[messageId] ??
      estimateConversationMessageHeight(messagesRef.current[messageIndexByIdRef.current.get(messageId) ?? 0]);
    if (hadPreviousMeasurement && Math.abs(previousHeight - roundedHeight) < 1) {
      return;
    }

    messageHeightsRef.current[messageId] = roundedHeight;

    const messageIndex = messageIndexByIdRef.current.get(messageId);
    const node = scrollContainerRef.current;
    // If the user/app is already at the latest message, measurements that
    // increase the virtualized height should move the viewport with the bottom.
    const shouldKeepBottom =
      isActive && node
        ? shouldKeepBottomAfterLayoutRef.current ||
          isScrollContainerNearBottom(node)
        : false;
    if (shouldKeepBottom) {
      shouldKeepBottomAfterLayoutRef.current = true;
    }
    if (node && messageIndex !== undefined) {
      if (shouldKeepBottom) {
        // Skip no-op writes so a measurement whose rounded height matches
        // the previous pin target cannot trigger an extra scroll event +
        // reflow + ResizeObserver re-fire cycle.
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        if (Math.abs(node.scrollTop - target) >= 1) {
          node.scrollTop = target;
        }
      } else {
        const nextScrollTop = getAdjustedVirtualizedScrollTopForHeightChange({
          currentScrollTop: node.scrollTop,
          messageTop: layoutTopsRef.current[messageIndex] ?? 0,
          nextHeight: roundedHeight,
          previousHeight,
        });
        if (Math.abs(nextScrollTop - node.scrollTop) >= 1) {
          node.scrollTop = nextScrollTop;
        }
      }
    }

    setLayoutVersion((current) => current + 1);
  }, [isActive, scrollContainerRef]);

  const boundApprovalDecision = useCallback(
    (messageId: string, decision: ApprovalDecision) => onApprovalDecision(sessionId, messageId, decision),
    [sessionId, onApprovalDecision],
  );
  const boundUserInputSubmit = useCallback(
    (messageId: string, answers: Record<string, string[]>) => onUserInputSubmit(sessionId, messageId, answers),
    [sessionId, onUserInputSubmit],
  );
  const boundMcpElicitationSubmit = useCallback(
    (messageId: string, action: McpElicitationAction, content?: JsonValue) =>
      onMcpElicitationSubmit(sessionId, messageId, action, content),
    [sessionId, onMcpElicitationSubmit],
  );
  const boundCodexAppRequestSubmit = useCallback(
    (messageId: string, result: JsonValue) => onCodexAppRequestSubmit(sessionId, messageId, result),
    [sessionId, onCodexAppRequestSubmit],
  );

  if (!isActive) {
    return <div className="virtualized-message-list" style={{ height: layout.totalHeight }} />;
  }

  return (
    <div
      className={`virtualized-message-list${isMeasuringPostActivation ? " is-measuring-post-activation" : ""}`}
      style={{ height: layout.totalHeight }}
    >
      {hasOlderMessages && visibleRange.startIndex === 0 && (
        <div
          className="load-earlier-messages"
          style={{ position: "absolute", top: 0, left: 0, right: 0 }}
        >
          <button
            type="button"
            className="ghost-button load-earlier-messages-button"
            onClick={() =>
              setRenderWindowSize((prev) =>
                Math.min(prev + RENDER_WINDOW_LOAD_MORE, messages.length),
              )
            }
          >
            Load {Math.min(RENDER_WINDOW_LOAD_MORE, windowStartIndex)} earlier
            messages ({windowStartIndex} hidden)
          </button>
        </div>
      )}
      {windowedMessages
        .slice(visibleRange.startIndex, visibleRange.endIndex)
        .map((message, visibleIndex) => {
          const messageIndex = visibleRange.startIndex + visibleIndex;
          return (
            <MeasuredMessageCard
              key={message.id}
              isActive={isActive}
              renderMessageCard={renderMessageCard}
              message={message}
              preferImmediateHeavyRender={isActive && messageIndex >= windowedMessages.length - 2}
              top={layout.tops[messageIndex] ?? 0}
              onApprovalDecision={boundApprovalDecision}
              onUserInputSubmit={boundUserInputSubmit}
              onMcpElicitationSubmit={boundMcpElicitationSubmit}
              onCodexAppRequestSubmit={boundCodexAppRequestSubmit}
              onHeightChange={handleHeightChange}
            />
          );
        })}
    </div>
  );
}

const MeasuredMessageCard = memo(function MeasuredMessageCard({
  isActive,
  renderMessageCard,
  message,
  preferImmediateHeavyRender,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onHeightChange,
  top,
}: {
  isActive: boolean;
  renderMessageCard: RenderMessageCard;
  message: Message;
  preferImmediateHeavyRender: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: BoundUserInputSubmitHandler;
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler;
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler;
  onHeightChange: (messageId: string, nextHeight: number) => void;
  top: number;
}) {
  const slotRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = slotRef.current;
    if (!node) {
      return;
    }

    let frameId = 0;
    const measure = () => {
      frameId = 0;
      onHeightChange(message.id, node.getBoundingClientRect().height);
    };

    measure();
    const resizeObserver = new ResizeObserver(() => {
      if (frameId !== 0) {
        return;
      }

      frameId = window.requestAnimationFrame(measure);
    });
    resizeObserver.observe(node);

    return () => {
      resizeObserver.disconnect();
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [isActive, message, onHeightChange]);

  return (
    <div ref={slotRef} className="virtualized-message-slot" style={{ top }}>
      {renderMessageCard(
        message,
        preferImmediateHeavyRender,
        onApprovalDecision,
        onUserInputSubmit,
        onMcpElicitationSubmit,
        onCodexAppRequestSubmit,
      )}
    </div>
  );
});

const SessionComposer = memo(function SessionComposer({
  paneId,
  isPaneActive,
  session,
  committedDraft,
  draftAttachments,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  showNewResponseIndicator,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onSend,
  onSessionSettingsChange,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  isPaneActive: boolean;
  session: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  agentCommands: AgentCommand[];
  hasLoadedAgentCommands: boolean;
  isRefreshingAgentCommands: boolean;
  agentCommandsError: string | null;
  showNewResponseIndicator: boolean;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const onDraftCommitRef = useRef(onDraftCommit);
  const requestedSlashModelOptionsRef = useRef<string | null>(null);
  const requestedSlashAgentCommandsRef = useRef<string | null>(null);
  const slashOptionsRef = useRef<HTMLDivElement | null>(null);
  const [localDraftsBySessionId, setLocalDraftsBySessionId] = useState<Record<string, string>>({});
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [slashNavModality, setSlashNavModality] = useState<"keyboard" | "mouse">("keyboard");

  const activeSessionId = session?.id ?? null;
  const composerDraft =
    activeSessionId === null ? "" : (localDraftsBySessionId[activeSessionId] ?? committedDraft);
  const slashPalette = useMemo(
    () =>
      buildSlashPaletteState(
        session,
        composerDraft,
        isRefreshingModelOptions,
        modelOptionsError,
        agentCommands,
        hasLoadedAgentCommands,
        isRefreshingAgentCommands,
        agentCommandsError,
      ),
    [
      agentCommands,
      agentCommandsError,
      composerDraft,
      hasLoadedAgentCommands,
      isRefreshingAgentCommands,
      isRefreshingModelOptions,
      modelOptionsError,
      session,
    ],
  );
  const slashPaletteResetKey = slashPalette.kind === "none" ? "none" : slashPalette.resetKey;
  const slashPaletteSupportsModelRefresh =
    slashPalette.kind === "choice" && slashPalette.supportsLiveRefresh;
  const slashPaletteSupportsAgentRefresh =
    slashPalette.kind === "command" && Boolean(slashPalette.supportsRefresh);
  const activeSlashItem =
    slashPalette.kind === "none" || slashPalette.items.length === 0
      ? null
      : (slashPalette.items[Math.min(slashActiveIndex, slashPalette.items.length - 1)] ?? null);
  const composerInputDisabled = !session || isStopping;
  const composerSendDisabled =
    !session ||
    isSending ||
    isStopping ||
    isUpdating ||
    (slashPalette.kind !== "none" && slashPalette.items.length === 0);

  function resizeComposerInput() {
    const textarea = composerInputRef.current;
    if (!textarea) {
      return;
    }

    const computedStyle = window.getComputedStyle(textarea);
    const minHeight = parseFloat(computedStyle.minHeight) || 0;
    const borderHeight =
      (parseFloat(computedStyle.borderTopWidth) || 0) +
      (parseFloat(computedStyle.borderBottomWidth) || 0);
    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    const availablePanelHeight =
      panelSlotElement?.clientHeight ??
      (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const maxHeight = Math.max(
      minHeight,
      availablePanelHeight > 0 ? availablePanelHeight * 0.4 : Number.POSITIVE_INFINITY,
    );

    textarea.style.height = "0px";

    const contentHeight = textarea.scrollHeight + borderHeight;
    const nextHeight = Math.min(Math.max(contentHeight, minHeight), maxHeight);
    textarea.style.height = `${nextHeight}px`;
    textarea.style.overflowY = contentHeight > maxHeight + 1 ? "auto" : "hidden";
  }

  useLayoutEffect(() => {
    resizeComposerInput();
  }, [activeSessionId, composerDraft]);

  useEffect(() => {
    localDraftsRef.current = localDraftsBySessionId;
  }, [localDraftsBySessionId]);

  useEffect(() => {
    onDraftCommitRef.current = onDraftCommit;
  }, [onDraftCommit]);

  useEffect(() => {
    setSlashActiveIndex(slashPalette.kind === "none" ? 0 : slashPalette.defaultActiveIndex);
  }, [activeSessionId, slashPaletteResetKey]);

  useEffect(() => {
    if (
      !session ||
      slashPalette.kind !== "choice" ||
      !slashPaletteSupportsModelRefresh ||
      !supportsLiveSessionModelOptions(session)
    ) {
      return;
    }

    if (session.modelOptions?.length) {
      requestedSlashModelOptionsRef.current = session.id;
      return;
    }

    if (isRefreshingModelOptions || requestedSlashModelOptionsRef.current === session.id) {
      return;
    }

    requestSlashModelOptions();
  }, [
    isRefreshingModelOptions,
    onRefreshSessionModelOptions,
    session,
    slashPalette.kind,
    slashPaletteSupportsModelRefresh,
  ]);

  useEffect(() => {
    if (slashPalette.kind === "none") {
      return;
    }

    const container = slashOptionsRef.current;
    if (!container) {
      return;
    }

    const activeOption = container.querySelector<HTMLButtonElement>(
      '.composer-slash-option.active[role="option"]',
    );
    if (!activeOption) {
      return;
    }

    const containerRect = container.getBoundingClientRect();
    const optionRect = activeOption.getBoundingClientRect();

    if (optionRect.top < containerRect.top) {
      container.scrollTop += optionRect.top - containerRect.top;
    } else if (optionRect.bottom > containerRect.bottom) {
      container.scrollTop += optionRect.bottom - containerRect.bottom;
    }
  }, [slashPalette.kind, slashPaletteResetKey, slashActiveIndex]);

  useEffect(() => {
    if (
      !session ||
      slashPalette.kind !== "command" ||
      !slashPaletteSupportsAgentRefresh ||
      !supportsAgentSlashCommands(session)
    ) {
      return;
    }

    const requestKey = `${session.id}:${session.workdir}:${session.agentCommandsRevision ?? 0}`;
    const requestKeyBase = `${session.id}:${session.workdir}:`;
    const alreadyRequested = requestedSlashAgentCommandsRef.current === requestKey;
    const isSameSessionRequest =
      requestedSlashAgentCommandsRef.current?.startsWith(requestKeyBase) ?? false;
    if (hasLoadedAgentCommands && !alreadyRequested && !isSameSessionRequest) {
      requestedSlashAgentCommandsRef.current = requestKey;
      return;
    }
    if (
      (hasLoadedAgentCommands && alreadyRequested) ||
      isRefreshingAgentCommands ||
      (agentCommandsError && alreadyRequested)
    ) {
      return;
    }

    requestSlashAgentCommands();
  }, [
    agentCommandsError,
    hasLoadedAgentCommands,
    isRefreshingAgentCommands,
    onRefreshAgentCommands,
    session,
    slashPalette.kind,
    slashPaletteSupportsAgentRefresh,
  ]);

  useEffect(() => {
    const textarea = composerInputRef.current;
    if (!textarea || typeof ResizeObserver === "undefined") {
      return;
    }

    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    let previousWidth = textarea.getBoundingClientRect().width;
    let previousAvailablePanelHeight =
      panelSlotElement?.clientHeight ??
      (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const resizeObserver = new ResizeObserver((entries) => {
      const nextWidth =
        entries.find((entry) => entry.target === textarea)?.contentRect.width ??
        textarea.getBoundingClientRect().width;
      const nextAvailablePanelHeight =
        panelSlotElement?.clientHeight ??
        (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
      const widthChanged = Math.abs(nextWidth - previousWidth) >= 1;
      const panelHeightChanged =
        Math.abs(nextAvailablePanelHeight - previousAvailablePanelHeight) >= 1;

      if (!widthChanged && !panelHeightChanged) {
        return;
      }

      previousWidth = nextWidth;
      previousAvailablePanelHeight = nextAvailablePanelHeight;
      resizeComposerInput();
    });

    resizeObserver.observe(textarea);
    if (panelSlotElement instanceof HTMLElement) {
      resizeObserver.observe(panelSlotElement);
    } else if (panelElement instanceof HTMLElement) {
      resizeObserver.observe(panelElement);
    }

    return () => {
      resizeObserver.disconnect();
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    const previousCommitted = committedDraftsRef.current[activeSessionId];
    const localDraft = localDraftsRef.current[activeSessionId];

    committedDraftsRef.current[activeSessionId] = committedDraft;

    if (localDraft !== undefined && localDraft !== previousCommitted) {
      return;
    }

    setLocalDraftsBySessionId((current) => {
      if ((current[activeSessionId] ?? "") === committedDraft) {
        return current;
      }

      return {
        ...current,
        [activeSessionId]: committedDraft,
      };
    });
  }, [activeSessionId, committedDraft]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    return () => {
      const latestDraft = localDraftsRef.current[activeSessionId];
      const committed = committedDraftsRef.current[activeSessionId] ?? "";
      if (latestDraft !== undefined && latestDraft !== committed) {
        committedDraftsRef.current[activeSessionId] = latestDraft;
        onDraftCommitRef.current(activeSessionId, latestDraft);
      }
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId || !isPaneActive || composerInputDisabled) {
      return;
    }

    focusComposerInput();
  }, [activeSessionId, composerInputDisabled, isPaneActive]);

  function resetPromptHistory(sessionId: string) {
    setPromptHistoryStateBySessionId((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });
  }

  function updateLocalDraft(sessionId: string, nextValue: string) {
    localDraftsRef.current = {
      ...localDraftsRef.current,
      [sessionId]: nextValue,
    };

    setLocalDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function commitDraft(sessionId: string, nextValue: string) {
    committedDraftsRef.current[sessionId] = nextValue;
    onDraftCommit(sessionId, nextValue);
  }

  function getComposerDraftValue() {
    return composerInputRef.current?.value ?? composerDraft;
  }

  function focusComposerInput(selectionStart?: number) {
    window.requestAnimationFrame(() => {
      const textarea = composerInputRef.current;
      if (!textarea) {
        return;
      }

      const nextSelectionStart = selectionStart ?? textarea.value.length;
      textarea.focus();
      textarea.setSelectionRange(nextSelectionStart, nextSelectionStart);
    });
  }

  function requestSlashModelOptions(force = false) {
    if (!session || !supportsLiveSessionModelOptions(session)) {
      return;
    }

    if (!force && requestedSlashModelOptionsRef.current === session.id) {
      return;
    }

    requestedSlashModelOptionsRef.current = session.id;
    void onRefreshSessionModelOptions(session.id);
  }

  function requestSlashAgentCommands(force = false) {
    if (!session || !supportsAgentSlashCommands(session)) {
      return;
    }

    const requestKey = `${session.id}:${session.workdir}:${session.agentCommandsRevision ?? 0}`;
    if (!force && requestedSlashAgentCommandsRef.current === requestKey) {
      return;
    }

    requestedSlashAgentCommandsRef.current = requestKey;
    void onRefreshAgentCommands(session.id);
  }

  function handleComposerChange(nextValue: string) {
    if (!activeSessionId) {
      return;
    }

    resetPromptHistory(activeSessionId);
    updateLocalDraft(activeSessionId, nextValue);
  }

  function handleComposerBlur() {
    if (!activeSessionId) {
      return;
    }

    commitDraft(activeSessionId, getComposerDraftValue());
  }

  function applySlashPaletteItem(item: SlashPaletteItem, keepPaletteOpen = false) {
    if (!session || isSending || isStopping) {
      return;
    }

    if (item.kind === "command") {
      resetPromptHistory(session.id);
      const nextDraft = `${item.command} `;
      updateLocalDraft(session.id, nextDraft);
      focusComposerInput(nextDraft.length);
      return;
    }

    if (item.kind === "agent-command") {
      if (isUpdating) {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }

      const agentCommand = item.command;
      const parsedDraft = parseAgentCommandDraft(getComposerDraftValue());
      const matchesSelectedCommand =
        parsedDraft?.commandName.toLowerCase() === item.name.toLowerCase();
      if (item.hasArguments && !matchesSelectedCommand) {
        resetPromptHistory(session.id);
        const nextDraft = `/${item.name} `;
        updateLocalDraft(session.id, nextDraft);
        focusComposerInput(nextDraft.length);
        return;
      }

      const visiblePrompt = (matchesSelectedCommand
        ? getComposerDraftValue()
        : `/${item.name}`).trim();
      const accepted =
        normalizedAgentCommandKind(agentCommand) === "nativeSlash"
          ? onSend(session.id, visiblePrompt)
          : onSend(
              session.id,
              visiblePrompt,
              agentCommand.content.split("$ARGUMENTS").join(
                matchesSelectedCommand ? (parsedDraft?.argumentsText ?? "") : "",
              ),
            );
      if (!accepted) {
        focusComposerInput();
        return;
      }

      resetPromptHistory(session.id);
      updateLocalDraft(session.id, "");
      commitDraft(session.id, "");
      focusComposerInput();
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    resetPromptHistory(session.id);
    void onSessionSettingsChange(session.id, item.field, item.value);
    if (keepPaletteOpen) {
      focusComposerInput(getComposerDraftValue().length);
    } else {
      updateLocalDraft(session.id, "");
      commitDraft(session.id, "");
      focusComposerInput(0);
    }
  }

  function handleComposerSend() {
    if (!session || isSending || isStopping) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (activeSlashItem) {
        if (activeSlashItem.kind === "choice" && isUpdating) {
          focusComposerInput(getComposerDraftValue().length);
          return;
        }
        applySlashPaletteItem(activeSlashItem);
      }
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    const draftToSend = getComposerDraftValue();
    const accepted = onSend(session.id, draftToSend);
    if (!accepted) {
      focusComposerInput();
      return;
    }

    resetPromptHistory(session.id);
    updateLocalDraft(session.id, "");
    commitDraft(session.id, "");
    focusComposerInput();
  }

  function handleComposerKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (!session) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (event.key === "Escape") {
        event.preventDefault();
        resetPromptHistory(session.id);
        updateLocalDraft(session.id, "");
        commitDraft(session.id, "");
        return;
      }

      if ((event.key === "Enter" && !event.shiftKey) || event.key === "Tab") {
        event.preventDefault();
        handleComposerSend();
        return;
      }

      if (
        isSpaceKey(event) &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey
      ) {
        if (activeSlashItem) {
          event.preventDefault();
          if (activeSlashItem.kind === "choice") {
            applySlashPaletteItem(activeSlashItem, true);
          } else {
            applySlashPaletteItem(activeSlashItem);
          }
        }
        return;
      }

      if (
        (event.key === "ArrowUp" || event.key === "ArrowDown") &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey
      ) {
        event.preventDefault();
        setSlashNavModality("keyboard");
        if (slashPalette.items.length === 0) {
          return;
        }

        setSlashActiveIndex((current) => {
          if (event.key === "ArrowUp") {
            return current <= 0 ? slashPalette.items.length - 1 : current - 1;
          }

          return current >= slashPalette.items.length - 1 ? 0 : current + 1;
        });
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      handleComposerSend();
      return;
    }

    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") {
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return;
    }

    const textarea = event.currentTarget;
    if (textarea.selectionStart !== 0 || textarea.selectionEnd !== 0) {
      return;
    }

    const promptHistory = collectUserPromptHistory(session);
    if (promptHistory.length === 0) {
      return;
    }

    const historyState = promptHistoryStateBySessionId[session.id];
    if (event.key === "ArrowDown" && !historyState) {
      return;
    }

    event.preventDefault();

    if (event.key === "ArrowUp") {
      const nextIndex = historyState
        ? Math.max(historyState.index - 1, 0)
        : promptHistory.length - 1;
      const draftSnapshot = historyState?.draft ?? getComposerDraftValue();

      setPromptHistoryStateBySessionId((current) => ({
        ...current,
        [session.id]: {
          index: nextIndex,
          draft: draftSnapshot,
        },
      }));
      updateLocalDraft(session.id, promptHistory[nextIndex]);
    } else {
      const currentHistoryState = historyState;
      if (!currentHistoryState) {
        return;
      }

      if (currentHistoryState.index >= promptHistory.length - 1) {
        resetPromptHistory(session.id);
        updateLocalDraft(session.id, currentHistoryState.draft);
      } else {
        const nextIndex = currentHistoryState.index + 1;
        setPromptHistoryStateBySessionId((current) => ({
          ...current,
          [session.id]: {
            index: nextIndex,
            draft: currentHistoryState.draft,
          },
        }));
        updateLocalDraft(session.id, promptHistory[nextIndex]);
      }
    }

    window.requestAnimationFrame(() => {
      textarea.setSelectionRange(0, 0);
    });
  }

  const slashPaletteErrorMessage =
    slashPalette.kind === "none" ? null : (slashPalette.errorMessage ?? null);
  const slashPaletteIsRefreshing =
    slashPalette.kind === "none" ? false : Boolean(slashPalette.isRefreshing);
  const slashPaletteRefreshActionLabel =
    slashPalette.kind === "none" ? null : (slashPalette.refreshActionLabel ?? null);
  const slashPaletteSupportsRefresh =
    slashPalette.kind === "choice"
      ? slashPalette.supportsLiveRefresh
      : slashPalette.kind === "command"
        ? Boolean(slashPalette.supportsRefresh)
        : false;
  const slashPaletteStatusText =
    slashPalette.kind === "command" ? (slashPalette.statusText ?? null) : null;
  const showSlashPaletteStatus =
    slashPalette.kind !== "none" &&
    (
      slashPaletteSupportsRefresh ||
      Boolean(slashPaletteErrorMessage) ||
      Boolean(slashPaletteStatusText) ||
      (slashPalette.kind === "choice" && isUpdating)
    );

  return (
    <footer className="composer">
      {showNewResponseIndicator ? (
        <button className="new-response-indicator" type="button" onClick={onScrollToLatest}>
          New response
        </button>
      ) : null}
      {draftAttachments.length > 0 ? (
        <div className="composer-attachments" aria-label="Draft image attachments">
          {draftAttachments.map((attachment) => (
            <article key={attachment.id} className="composer-attachment-card">
              <img
                className="composer-attachment-preview"
                src={attachment.previewUrl}
                alt={attachment.fileName}
              />
              <div className="composer-attachment-copy">
                <strong className="composer-attachment-name">{attachment.fileName}</strong>
                <span className="composer-attachment-meta">
                  {formatByteSize(attachment.byteSize)} | {attachment.mediaType}
                </span>
              </div>
              <button
                className="composer-attachment-remove"
                type="button"
                onClick={() => session && onDraftAttachmentRemove(session.id, attachment.id)}
                aria-label={`Remove ${attachment.fileName}`}
                disabled={composerInputDisabled}
              >
                Remove
              </button>
            </article>
          ))}
        </div>
      ) : null}
      <div className="composer-row">
        <textarea
          id={`prompt-${paneId}`}
          ref={composerInputRef}
          className="composer-input"
          aria-label={session ? `Message ${session.name}` : "Message session"}
          value={composerDraft}
          onChange={(event) => handleComposerChange(event.target.value)}
          onBlur={handleComposerBlur}
          disabled={composerInputDisabled}
          onKeyDown={handleComposerKeyDown}
          onPaste={onPaste}
          placeholder={session ? `Send a prompt to ${session.agent}...` : "Open a session..."}
          rows={1}
        />
        <div className="composer-actions">
          {session && (isSessionBusy || isStopping) ? (
            <button
              className="ghost-button composer-stop-button"
              type="button"
              onClick={() => onStopSession(session.id)}
              disabled={isStopping}
            >
              {isStopping ? "Stopping..." : "Stop"}
            </button>
          ) : null}
          <button
            className="send-button"
            type="button"
            onMouseDown={(event) => {
              event.preventDefault();
            }}
            onClick={handleComposerSend}
            disabled={composerSendDisabled}
          >
            {isSending
              ? isSessionBusy
                ? "Queueing..."
                : "Sending..."
              : isSessionBusy
                ? "Queue"
                : "Send"}
          </button>
        </div>
      </div>
      {session ? (
        <p className="composer-hint">
          Paste PNG, JPEG, GIF, or WebP images into the prompt. Drag-and-drop is not supported
          yet.
        </p>
      ) : null}
      {session && slashPalette.kind !== "none" ? (
        <div className="composer-slash-menu" role="listbox" aria-label={slashPalette.title}>
          <div className="composer-slash-header">
            <strong className="composer-slash-title">{slashPalette.title}</strong>
            <span className="composer-slash-hint">{slashPalette.hint}</span>
          </div>
          {showSlashPaletteStatus ? (
            <div className="composer-slash-status">
              {slashPaletteErrorMessage ? (
                <p className="composer-slash-error" role="alert">
                  {slashPaletteErrorMessage}
                </p>
              ) : slashPalette.kind === "choice" ? (
                <p className="composer-slash-status-text" aria-live="polite">
                  {isUpdating ? (
                    <span className="composer-slash-status-inline">
                      <span className="composer-slash-status-spinner" aria-hidden="true" />
                      Applying setting...
                    </span>
                  ) : slashPalette.isRefreshing ? (
                    "Loading live model options..."
                  ) : slashPalette.supportsLiveRefresh ? (
                    "Refresh live models to update this list from the active session."
                  ) : null}
                </p>
              ) : slashPaletteStatusText ? (
                <p className="composer-slash-status-text" aria-live="polite">
                  {slashPaletteIsRefreshing ? (
                    <span className="composer-slash-status-inline">
                      <span className="composer-slash-status-spinner" aria-hidden="true" />
                      {slashPaletteStatusText}
                    </span>
                  ) : (
                    slashPaletteStatusText
                  )}
                </p>
              ) : null}
              {slashPaletteSupportsRefresh ? (
                <button
                  className="ghost-button composer-slash-refresh-button"
                  type="button"
                  onClick={() => {
                    if (slashPalette.kind === "choice") {
                      requestSlashModelOptions(true);
                    } else {
                      requestSlashAgentCommands(true);
                    }
                  }}
                  disabled={
                    (slashPalette.kind === "choice"
                      ? isRefreshingModelOptions
                      : isRefreshingAgentCommands) || isUpdating
                  }
                >
                  {slashPaletteIsRefreshing
                    ? "Loading..."
                    : (slashPaletteRefreshActionLabel ??
                        (slashPalette.kind === "choice"
                          ? "Refresh live models"
                          : "Refresh agent commands"))}
                </button>
              ) : null}
            </div>
          ) : null}
          {slashPalette.items.length > 0 ? (
            <div
              ref={slashOptionsRef}
              className={`composer-slash-options modality-${slashNavModality}`}
            >
              {slashPalette.items.map((item, index) => {
                const isActive = activeSlashItem?.key === item.key && index === slashActiveIndex;

                return (
                  <div key={item.key} className="composer-slash-option-group">
                    {item.sectionLabel ? (
                      <div className="composer-slash-section-label">{item.sectionLabel}</div>
                    ) : null}
                    <button
                      className={`composer-slash-option${isActive ? " active" : ""}`}
                      type="button"
                      role="option"
                      aria-selected={isActive}
                      onMouseDown={(event) => {
                        event.preventDefault();
                      }}
                      onMouseMove={() => {
                        setSlashNavModality("mouse");
                        if (slashActiveIndex !== index) {
                          setSlashActiveIndex(index);
                        }
                      }}
                      onClick={() => applySlashPaletteItem(item)}
                      disabled={(item.kind === "choice" || item.kind === "agent-command") && isUpdating}
                    >
                      <span className="composer-slash-option-copy">
                        <span className="composer-slash-option-label">{item.label}</span>
                        <span className="composer-slash-option-detail">{item.detail}</span>
                      </span>
                      {item.kind === "choice" && item.isCurrent ? (
                        isUpdating ? (
                          <span className="composer-slash-option-badge pending">
                            <span className="composer-slash-option-spinner" aria-hidden="true" />
                            Applying
                          </span>
                        ) : (
                          <span className="composer-slash-option-badge">Current</span>
                        )
                      ) : item.kind === "agent-command" ? (
                        <span className="composer-slash-option-badge">Agent</span>
                      ) : null}
                    </button>
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="composer-slash-empty">
              {slashPalette.emptyMessage}
              {slashPalette.kind === "choice" &&
              slashPalette.supportsLiveRefresh &&
              slashPalette.isRefreshing
                ? " Live options will appear here as soon as they load."
                : slashPalette.kind === "command" && slashPaletteIsRefreshing
                  ? " Agent commands will appear here as soon as they load."
                  : null}
            </p>
          )}
        </div>
      ) : null}
    </footer>
  );
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.isPaneActive === next.isPaneActive &&
  previous.session === next.session &&
  previous.committedDraft === next.committedDraft &&
  previous.draftAttachments === next.draftAttachments &&
  previous.formatByteSize === next.formatByteSize &&
  previous.isSending === next.isSending &&
  previous.isStopping === next.isStopping &&
  previous.isSessionBusy === next.isSessionBusy &&
  previous.isUpdating === next.isUpdating &&
  previous.isRefreshingModelOptions === next.isRefreshingModelOptions &&
  previous.modelOptionsError === next.modelOptionsError &&
  previous.agentCommands === next.agentCommands &&
  previous.hasLoadedAgentCommands === next.hasLoadedAgentCommands &&
  previous.isRefreshingAgentCommands === next.isRefreshingAgentCommands &&
  previous.agentCommandsError === next.agentCommandsError &&
  previous.showNewResponseIndicator === next.showNewResponseIndicator
);

export function RunningIndicator({
  agent,
  lastPrompt,
}: {
  agent: Session["agent"];
  lastPrompt: string | null;
}) {
  const isCommand = Boolean(lastPrompt?.trim().startsWith("/"));

  return (
    <article
      className={`activity-card activity-card-live ${lastPrompt ? "has-tooltip" : ""}`}
      role="status"
      aria-live="polite"
    >
      <div className="activity-spinner" aria-hidden="true" />
      <div className="activity-card-copy">
        <div className="activity-card-heading">
          <div className="card-label">Live turn</div>
          {isCommand ? <span className="message-meta-tag">Command</span> : null}
        </div>
        <h3>{agent} is working</h3>
        <p>{isCommand ? "Executing a command..." : "Waiting for the next chunk of output..."}</p>
      </div>
      {lastPrompt ? (
        <div className="activity-tooltip" role="tooltip">
          <div className="activity-tooltip-label">{isCommand ? "Command" : "Last prompt"}</div>
          <p>{lastPrompt}</p>
        </div>
      ) : null}
    </article>
  );
}

const PendingPromptCard = memo(function PendingPromptCard({
  prompt,
  onCancel,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  prompt: PendingPrompt;
  onCancel: () => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const commandLabel = promptCommandMetaLabel(prompt.text, prompt.expandedText);

  return (
    <article className="message-card bubble bubble-you pending-prompt-card">
      <div className="pending-prompt-header">
        <MessageMeta
          author="you"
          timestamp={prompt.timestamp}
          trailing={
            commandLabel ? <span className="message-meta-tag">{commandLabel}</span> : undefined
          }
        />
        <button
          className="pending-prompt-dismiss"
          type="button"
          onClick={onCancel}
          aria-label="Cancel queued prompt"
          title="Cancel queued prompt"
        >
          x
        </button>
      </div>
      {prompt.attachments && prompt.attachments.length > 0 ? (
        <MessageAttachmentList
          attachments={prompt.attachments}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      ) : null}
      {prompt.text ? (
        <>
          <p className="plain-text-copy">
            {renderHighlightedText(prompt.text, searchQuery, searchHighlightTone)}
          </p>
          {prompt.expandedText ? (
            <ExpandedPromptPanel
              expandedText={prompt.expandedText}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : null}
        </>
      ) : (
        <p className="support-copy">{imageAttachmentSummaryLabel(prompt.attachments?.length ?? 0)}</p>
      )}
    </article>
  );
}, (previous, next) =>
  previous.prompt === next.prompt &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone
);

function promptCommandMetaLabel(text: string, expandedText?: string | null) {
  return expandedText && text.trim().startsWith("/") ? "Command" : null;
}

function MessageMeta({
  author,
  timestamp,
  trailing,
}: {
  author: string;
  timestamp: string;
  trailing?: ReactNode;
}) {
  const isUser = author === "you";

  return (
    <div className="message-meta">
      <span
        className={`message-meta-author ${isUser ? "message-meta-author-user" : "message-meta-author-agent"}`}
      >
        {isUser ? "You" : "Agent"}
      </span>
      <span className="message-meta-end">
        {trailing}
        <span>{timestamp}</span>
      </span>
    </div>
  );
}

function MessageAttachmentList({
  attachments,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  attachments: ImageAttachment[];
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <div className="message-attachment-list">
      {attachments.map((attachment, index) => (
        <div
          key={`${attachment.fileName}-${attachment.byteSize}-${index}`}
          className="message-attachment-chip"
        >
          <strong className="message-attachment-name">
            {renderHighlightedText(attachment.fileName, searchQuery, searchHighlightTone)}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} |{" "}
            {renderHighlightedText(attachment.mediaType, searchQuery, searchHighlightTone)}
          </span>
        </div>
      ))}
    </div>
  );
}

function MessageSlot({
  children,
  itemKey,
  isSearchMatch = false,
  isSearchActive = false,
  onSearchItemMount,
}: {
  children: ReactNode;
  itemKey?: string;
  isSearchMatch?: boolean;
  isSearchActive?: boolean;
  onSearchItemMount?: (itemKey: string, node: HTMLElement | null) => void;
}) {
  if (!itemKey) {
    return <>{children}</>;
  }

  return (
    <div
      className={`message-slot${isSearchMatch ? " session-search-hit" : ""}${isSearchActive ? " session-search-hit-active" : ""}`}
      data-session-search-item-key={itemKey}
      ref={(node) => {
        onSearchItemMount?.(itemKey, node);
      }}
    >
      {children}
    </div>
  );
}

function PanelEmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function buildVirtualizedMessageLayout(itemHeights: number[]) {
  const tops = new Array<number>(itemHeights.length);
  let offset = 0;

  for (let index = 0; index < itemHeights.length; index += 1) {
    const itemHeight =
      Number.isFinite(itemHeights[index]) && itemHeights[index] > 0
        ? itemHeights[index]
        : DEFAULT_ESTIMATED_MESSAGE_HEIGHT;
    tops[index] = offset;
    offset += itemHeight + VIRTUALIZED_MESSAGE_GAP_PX;
  }

  return {
    tops,
    totalHeight: Math.max(offset - VIRTUALIZED_MESSAGE_GAP_PX, 0),
  };
}

function findVirtualizedMessageRange(
  tops: number[],
  itemHeights: number[],
  scrollTop: number,
  viewportHeight: number,
  overscan: number,
) {
  if (itemHeights.length === 0) {
    return {
      startIndex: 0,
      endIndex: 0,
    };
  }

  const startBoundary = Math.max(scrollTop - overscan, 0);
  const endBoundary =
    scrollTop + Math.max(viewportHeight, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT) + overscan;

  let startIndex = 0;
  while (
    startIndex < itemHeights.length - 1 &&
    tops[startIndex] + itemHeights[startIndex] < startBoundary
  ) {
    startIndex += 1;
  }

  let endIndex = startIndex;
  while (endIndex < itemHeights.length && tops[endIndex] < endBoundary) {
    endIndex += 1;
  }

  return {
    startIndex,
    endIndex: Math.max(startIndex + 1, endIndex),
  };
}

export function getScrollContainerBottomGap(
  node: Pick<HTMLElement, "clientHeight" | "scrollHeight" | "scrollTop">,
) {
  return Math.max(node.scrollHeight - node.clientHeight - node.scrollTop, 0);
}

// Intentionally mirrors `syncMessageStackScrollPosition`'s `< 72`
// stickiness threshold in `ui/src/App.tsx`. A previous revision used 96 px,
// which left a 72-96 px band where the parent pane had already recorded
// `shouldStick: false` (because the user scrolled up past its own
// threshold) but a later measurement in the virtualized panel would still
// re-pin the viewport to the latest message — the user would feel
// "snatched back" when a code block tokenized or an image loaded. Any
// change here should be made symmetrically in both files.
export function isScrollContainerNearBottom(
  node: Pick<HTMLElement, "clientHeight" | "scrollHeight" | "scrollTop">,
) {
  return getScrollContainerBottomGap(node) < 72;
}

export function getAdjustedVirtualizedScrollTopForHeightChange({
  currentScrollTop,
  messageTop,
  nextHeight,
  previousHeight,
}: {
  currentScrollTop: number;
  messageTop: number;
  nextHeight: number;
  previousHeight: number;
}) {
  const heightDelta = nextHeight - previousHeight;

  // Never adjust for messages that start at or below the viewport top.
  if (messageTop >= currentScrollTop) {
    return currentScrollTop;
  }

  // Growing a partially visible message should not jump the viewport. Shrinks
  // still adjust so the anchor can move upward, floored at zero.
  if (heightDelta > 0 && messageTop + previousHeight > currentScrollTop) {
    return currentScrollTop;
  }

  return Math.max(currentScrollTop + heightDelta, 0);
}

function estimateConversationMessageHeight(message: Message): number {
  switch (message.type) {
    case "text": {
      const lineCount = message.text.length === 0 ? 1 : message.text.split("\n").length;
      const attachmentHeight = (message.attachments?.length ?? 0) * 54;
      return Math.min(1800, Math.max(92, 78 + lineCount * 24 + attachmentHeight));
    }
    case "thinking":
      return Math.min(900, Math.max(140, 112 + message.lines.length * 28));
    case "command": {
      const commandLineCount = message.command.length === 0 ? 1 : message.command.split("\n").length;
      const outputLineCount = message.output ? message.output.split("\n").length : 3;
      return Math.min(
        1400,
        Math.max(180, 152 + commandLineCount * 22 + Math.min(outputLineCount, 14) * 20),
      );
    }
    case "diff": {
      const diffLineCount = message.diff.length === 0 ? 1 : message.diff.split("\n").length;
      return Math.min(1500, Math.max(180, 156 + Math.min(diffLineCount, 20) * 20));
    }
    case "markdown": {
      const markdownLineCount =
        message.markdown.length === 0 ? 1 : message.markdown.split("\n").length;
      return Math.min(1600, Math.max(140, 124 + markdownLineCount * 24));
    }
    case "parallelAgents": {
      const detailLineCount = message.agents.reduce((count, agent) => {
        return count + (agent.detail?.split("\n").length ?? 1);
      }, 0);
      return Math.min(900, Math.max(168, 136 + message.agents.length * 52 + detailLineCount * 18));
    }
    case "fileChanges":
      return Math.min(900, Math.max(160, 136 + message.files.length * 44));
    case "subagentResult":
      return Math.min(720, Math.max(132, 128 + Math.min(message.summary.split("\n").length, 4) * 24));
    case "approval":
      return Math.max(220, 188 + (message.detail.length === 0 ? 1 : message.detail.split("\n").length) * 22);
    case "userInputRequest":
      return Math.min(
        1200,
        Math.max(
          220,
          188 +
            message.questions.length * 76 +
            (message.detail.length === 0 ? 1 : message.detail.split("\n").length) * 20,
        ),
      );
    case "mcpElicitationRequest": {
      const detailLineCount = message.detail.length === 0 ? 1 : message.detail.split("\n").length;
      const fieldCount =
        message.request.mode === "form"
          ? Object.keys(message.request.requestedSchema.properties).length
          : 1;
      return Math.min(1200, Math.max(220, 192 + detailLineCount * 20 + fieldCount * 64));
    }
    case "codexAppRequest": {
      const detailLineCount = message.detail.length === 0 ? 1 : message.detail.split("\n").length;
      return Math.min(900, Math.max(220, 188 + detailLineCount * 20));
    }
  }
}

function collectUserPromptHistory(session: Session) {
  return session.messages.flatMap((message) => {
    if (message.type !== "text" || message.author !== "you") {
      return [];
    }

    const prompt = message.text.trim();
    return prompt ? [prompt] : [];
  });
}

function imageAttachmentSummaryLabel(count: number) {
  return count === 1 ? "1 image attached" : `${count} images attached`;
}

function formatByteSize(byteSize: number) {
  if (byteSize < 1024) {
    return `${byteSize} B`;
  }

  if (byteSize < 1024 * 1024) {
    return `${(byteSize / 1024).toFixed(1)} KB`;
  }

  return `${(byteSize / (1024 * 1024)).toFixed(1)} MB`;
}
