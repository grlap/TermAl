import {
  memo,
  startTransition,
  useDeferredValue,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ClipboardEvent as ReactClipboardEvent,
  type DragEvent as ReactDragEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { createPortal, flushSync } from "react-dom";
import ReactMarkdown, { uriTransformer } from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  archiveCodexThread,
  cancelQueuedPrompt,
  compactCodexThread,
  createProject,
  createSession,
  fetchAgentCommands,
  fetchFile,
  fetchGitStatus,
  fetchState,
  forkCodexThread,
  killSession,
  pickProjectRoot,
  refreshSessionModelOptions,
  renameSession,
  rollbackCodexThread,
  saveFile,
  sendMessage,
  stopSession,
  submitApproval,
  submitCodexAppRequest,
  submitMcpElicitation,
  submitUserInput,
  type GitDiffSection,
  type GitDiffResponse,
  type StateResponse,
  unarchiveCodexThread,
  updateAppSettings,
  updateSessionSettings,
} from "./api";
import { AgentIcon } from "./agent-icon";
import { ExpandedPromptPanel } from "./ExpandedPromptPanel";
import { copyTextToClipboard } from "./clipboard";
import { buildDiffPreviewModel } from "./diff-preview";
import { highlightCode } from "./highlight";
import { applyDeltaToSessions } from "./live-updates";
import {
  looksLikeWindowsPath,
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "./path-display";
import {
  LOCAL_REMOTE_ID,
  createBuiltinLocalRemote,
  isLocalRemoteId,
  normalizeRemoteConfigs,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "./remotes";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import { AgentSessionPanel, AgentSessionPanelFooter } from "./panels/AgentSessionPanel";
import {
  ControlPanelSurface,
  type ControlPanelSectionId,
  type ControlPanelSurfaceHandle,
} from "./panels/ControlPanelSurface";
import { DiffPanel } from "./panels/DiffPanel";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { InstructionDebuggerPanel } from "./panels/InstructionDebuggerPanel";
import { PaneTabs } from "./panels/PaneTabs";
import { SourcePanel, type SourceFileState } from "./panels/SourcePanel";
import {
  containsSearchMatch,
  highlightReactNodeText,
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  buildSessionSearchMatchesFromIndex,
  type SessionListSearchResult,
  type SessionSearchMatch,
} from "./session-find";
import type {
  AppPreferences,
  ApprovalDecision,
  ApprovalMessage,
  ApprovalPolicy,
  AgentCommand,
  AgentReadiness,
  AgentType,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CommandMessage,
  CodexAppRequestMessage,
  CodexReasoningEffort,
  CodexState,
  CursorMode,
  DeltaEvent,
  DiffMessage,
  GeminiApprovalMode,
  ImageAttachment,
  JsonValue,
  MarkdownMessage,
  Message,
  McpElicitationAction,
  McpElicitationPrimitiveSchema,
  McpElicitationRequestMessage,
  ParallelAgentsMessage,
  SubagentResultMessage,
  PendingPrompt,
  Project,
  RemoteConfig,
  SandboxMode,
  Session,
  SessionModelOption,
  TextMessage,
  ThinkingMessage,
  UserInputQuestion,
  UserInputRequestMessage,
} from "./types";
import {
  activatePane,
  closeWorkspaceTab,
  dockControlPanelAtWorkspaceEdge,
  ensureControlPanelInWorkspaceState,
  findWorkspacePaneIdForSession,
  getSplitRatio,
  openDiffPreviewInWorkspaceState,
  openFilesystemInWorkspaceState,
  openGitStatusInWorkspaceState,
  openInstructionDebuggerInWorkspaceState,
  openSessionInWorkspaceState,
  openSourceInWorkspaceState,
  placeDraggedTab,
  placeExternalTab,
  reconcileWorkspaceState,
  setPaneSourcePath,
  setPaneViewMode,
  splitPane,
  updateSplitRatio,
  type PaneViewMode,
  type SessionPaneViewMode,
  type TabDropPlacement,
  type WorkspaceNode,
  type WorkspacePane,
  type WorkspaceState,
} from "./workspace";
import {
  getStoredWorkspaceLayout,
  persistWorkspaceLayout,
  type ControlPanelSide,
} from "./workspace-storage";
import { reconcileSessions } from "./session-reconcile";

const TAB_DRAG_STALE_TIMEOUT_MS = 15000;
import {
  clampEditorFontSizePreference,
  clampFontSizePreference,
  DEFAULT_EDITOR_FONT_SIZE_PX,
  DEFAULT_FONT_SIZE_PX,
  MAX_EDITOR_FONT_SIZE_PX,
  MAX_FONT_SIZE_PX,
  MIN_EDITOR_FONT_SIZE_PX,
  MIN_FONT_SIZE_PX,
  applyFontSizePreference,
  THEMES,
  getStoredEditorFontSizePreference,
  getStoredFontSizePreference,
  applyThemePreference,
  persistEditorFontSizePreference,
  persistFontSizePreference,
  getStoredThemePreference,
  persistThemePreference,
  type ThemeId,
} from "./themes";
import type { MonacoAppearance } from "./monaco";
import {
  countSessionsByFilter,
  filterSessionsByListFilter,
  type SessionListFilter,
} from "./session-list-filter";
import { decideDeltaRevisionAction, shouldAdoptStateRevision } from "./state-revision";
import {
  TAB_DRAG_CHANNEL_NAME,
  isWorkspaceTabDragChannelMessage,
  type WorkspaceTabDrag,
  type WorkspaceTabDragChannelMessage,
} from "./tab-drag";

type SessionFlagMap = Record<string, true | undefined>;
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
type SessionErrorMap = Record<string, string | undefined>;
type SessionNoticeMap = Record<string, string | undefined>;
type BackendConnectionState = "connecting" | "connected" | "reconnecting" | "offline";
type SessionAgentCommandMap = Record<string, AgentCommand[] | undefined>;
type PreferencesTabId = "themes" | "appearance" | "remotes" | "codex-prompts" | "claude-approvals";
type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

type PendingSessionRename = {
  clientX: number;
  clientY: number;
  sessionId: string;
};

type ComboboxOption = {
  label: string;
  value: string;
  description?: string;
  badges?: string[];
};

const PENDING_KILL_CLOSE_DELAY_MS = 180;
const PENDING_SESSION_RENAME_CLOSE_DELAY_MS = 300;
const DEFAULT_SPLIT_MIN_RATIO = 0.22;
const DEFAULT_SPLIT_MAX_RATIO = 0.78;
const CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX = 20 * 16;

type SessionConversationItem =
  | {
      author: Message["author"];
      id: string;
      kind: "message";
      message: Message;
    }
  | {
      author: "you";
      id: string;
      kind: "pendingPrompt";
      prompt: PendingPrompt;
    };

const SUPPORTED_PASTED_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const MAX_PASTED_IMAGE_BYTES = 5 * 1024 * 1024;
const DEFERRED_RENDER_ROOT_MARGIN_PX = 960;
const HEAVY_CODE_CHARACTER_THRESHOLD = 1400;
const HEAVY_CODE_LINE_THRESHOLD = 28;
const HEAVY_MARKDOWN_CHARACTER_THRESHOLD = 1800;
const HEAVY_MARKDOWN_LINE_THRESHOLD = 24;
const DEFERRED_PREVIEW_LINE_LIMIT = 12;
const DEFERRED_PREVIEW_CHARACTER_LIMIT = 720;
const MAX_DEFERRED_PLACEHOLDER_HEIGHT = 960;
const MAX_CACHED_SESSION_PAGES_PER_PANE = 3;
const NEW_SESSION_AGENT_OPTIONS = [
  { label: "Claude", value: "Claude" },
  { label: "Codex", value: "Codex" },
  { label: "Cursor", value: "Cursor" },
  { label: "Gemini", value: "Gemini" },
] as const;
const NEW_SESSION_MODEL_OPTIONS: Readonly<Record<AgentType, readonly ComboboxOption[]>> = {
  Claude: [
    { label: "Sonnet", value: "sonnet" },
  ],
  Codex: [{ label: "GPT-5.4", value: "gpt-5.4" }],
  Cursor: [{ label: "Auto", value: "auto" }],
  Gemini: [{ label: "Auto", value: "auto" }],
};
const SANDBOX_MODE_OPTIONS = [
  { label: "workspace-write", value: "workspace-write" },
  { label: "read-only", value: "read-only" },
  { label: "danger-full-access", value: "danger-full-access" },
] as const;
const APPROVAL_POLICY_OPTIONS = [
  { label: "never", value: "never" },
  { label: "on-request", value: "on-request" },
  { label: "untrusted", value: "untrusted" },
  { label: "on-failure", value: "on-failure" },
] as const;
const CODEX_REASONING_EFFORT_OPTIONS = [
  { label: "none", value: "none", description: "Disable reasoning for speed-sensitive turns" },
  { label: "minimal", value: "minimal", description: "Use the lightest reasoning pass" },
  { label: "low", value: "low", description: "Keep reasoning light" },
  { label: "medium", value: "medium", description: "Use the standard reasoning depth" },
  { label: "high", value: "high", description: "Use deeper reasoning for harder prompts" },
  { label: "xhigh", value: "xhigh", description: "Use the maximum reasoning depth" },
] as const;
const CLAUDE_APPROVAL_OPTIONS = [
  { label: "ask", value: "ask" },
  { label: "auto-approve", value: "auto-approve" },
  { label: "plan", value: "plan" },
] as const;
const CLAUDE_EFFORT_OPTIONS = [
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
const CURSOR_MODE_OPTIONS = [
  {
    label: "agent",
    value: "agent",
    description: "Allow edits and auto-approve tool requests",
  },
  {
    label: "plan",
    value: "plan",
    description: "Stay read-only and deny tool requests",
  },
  {
    label: "ask",
    value: "ask",
    description: "Keep approval cards before tool use",
  },
] as const;
const GEMINI_APPROVAL_OPTIONS = [
  { label: "default", value: "default" },
  { label: "auto_edit", value: "auto_edit" },
  { label: "yolo", value: "yolo" },
  { label: "plan", value: "plan" },
] as const;
const PREFERENCES_TABS: ReadonlyArray<{ id: PreferencesTabId; label: string }> = [
  { id: "themes", label: "Themes" },
  { id: "appearance", label: "Editor & UI appearance" },
  { id: "remotes", label: "Remotes" },
  { id: "codex-prompts", label: "Codex defaults" },
  { id: "claude-approvals", label: "Claude defaults" },
];
const ALL_PROJECTS_FILTER_ID = "__all__";
const CREATE_SESSION_WORKSPACE_ID = "__workspace__";
const SESSION_SCOPED_MODEL_AGENTS = new Set<AgentType>(["Claude", "Codex", "Cursor", "Gemini"]);

function defaultNewSessionModel(agent: AgentType): string {
  return NEW_SESSION_MODEL_OPTIONS[agent][0]?.value ?? "";
}

function usesSessionModelPicker(agent: AgentType): boolean {
  return SESSION_SCOPED_MODEL_AGENTS.has(agent);
}

function createSessionModelHint(agent: AgentType): string {
  switch (agent) {
    case "Claude":
      return "Claude model selection lives on the session itself. TermAl asks Claude for its live model list after the session opens, and you can always enter a full Claude model id manually. New Claude sessions start on Sonnet.";
    case "Codex":
      return "Codex model selection lives on the session itself. TermAl asks Codex for its live model list after the session opens, and you can always enter a full Codex model id manually.";
    case "Cursor":
      return "Cursor model selection lives on the session itself, like Cursor Agent's /model flow. TermAl asks Cursor for its live model list after the session opens, and you can still enter a full model id manually. New Cursor sessions start on Auto.";
    case "Gemini":
      return "Gemini model selection lives on the session itself. TermAl asks Gemini for its live model list after the session opens, and you can still enter a full Gemini model id manually. New Gemini sessions start on Auto.";
  }
}

function staticSessionModelOptions(agent: AgentType, currentModel: string): ComboboxOption[] {
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

function formatSessionModelOptionLabel(model: string): string {
  if (model === "auto") {
    return "Auto";
  }
  if (model === "default") {
    return "Default";
  }
  return model;
}

function sessionModelComboboxOptions(
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

function matchingSessionModelOption(
  modelOptions: Session["modelOptions"] | undefined,
  requestedModel: string,
) {
  const trimmedModel = requestedModel.trim();
  if (!trimmedModel) {
    return null;
  }

  const normalizedRequestedModel = trimmedModel.toLowerCase();
  return (
    modelOptions?.find(
      (option) =>
        option.value.toLowerCase() === normalizedRequestedModel ||
        option.label.toLowerCase() === normalizedRequestedModel,
    ) ?? null
  );
}

function normalizedRequestedSessionModel(session: Session, requestedModel: string) {
  return matchingSessionModelOption(session.modelOptions, requestedModel)?.value ?? requestedModel.trim();
}

function currentSessionModelOption(session: Session) {
  return matchingSessionModelOption(session.modelOptions, session.model);
}

const ALL_CODEX_REASONING_EFFORTS = CODEX_REASONING_EFFORT_OPTIONS.map(
  (option) => option.value,
) as CodexReasoningEffort[];
const DEFAULT_CODEX_REASONING_EFFORT: CodexReasoningEffort = "medium";
const DEFAULT_CLAUDE_EFFORT: ClaudeEffortLevel = "default";
const FALLBACK_CLAUDE_EFFORTS = ["low", "medium", "high"] as ClaudeEffortLevel[];

function resolveAppPreferences(preferences?: AppPreferences | null) {
  return {
    defaultCodexReasoningEffort:
      preferences?.defaultCodexReasoningEffort ?? DEFAULT_CODEX_REASONING_EFFORT,
    defaultClaudeEffort: preferences?.defaultClaudeEffort ?? DEFAULT_CLAUDE_EFFORT,
    remotes: normalizeRemoteConfigs(preferences?.remotes),
  };
}

function areRemoteConfigsEqual(left: readonly RemoteConfig[], right: readonly RemoteConfig[]) {
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

function resolveRemoteConfig(
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
  remoteId?: string | null,
): RemoteConfig {
  return remoteLookup.get(remoteId?.trim() || LOCAL_REMOTE_ID) ?? createBuiltinLocalRemote();
}

function describeProjectScope(
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

function remoteBadgeLabel(remote: RemoteConfig): string {
  return remote.transport === "local" ? "Local" : "SSH";
}

function codexReasoningEffortOption(
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

function supportedCodexReasoningEffortsForModelOption(option: SessionModelOption | null) {
  return option?.supportedReasoningEfforts?.length
    ? option.supportedReasoningEfforts
    : ALL_CODEX_REASONING_EFFORTS;
}

function defaultCodexReasoningEffortForModelOption(option: SessionModelOption | null) {
  const supportedEfforts = supportedCodexReasoningEffortsForModelOption(option);
  if (
    option?.defaultReasoningEffort &&
    supportedEfforts.includes(option.defaultReasoningEffort)
  ) {
    return option.defaultReasoningEffort;
  }

  return supportedEfforts[0] ?? null;
}

function currentCodexModelOption(session: Session) {
  if (session.agent !== "Codex") {
    return null;
  }

  return currentSessionModelOption(session);
}

function currentClaudeEffort(session: Session): ClaudeEffortLevel {
  return session.claudeEffort ?? DEFAULT_CLAUDE_EFFORT;
}

function claudeEffortOption(effort: ClaudeEffortLevel): ComboboxOption {
  return (
    CLAUDE_EFFORT_OPTIONS.find((option) => option.value === effort) ?? {
      label: effort,
      value: effort,
      description: undefined,
    }
  );
}

function supportedClaudeEffortLevelsForModelOption(
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

function claudeEffortComboboxOptions(session: Session) {
  const currentModelOption = currentSessionModelOption(session);
  const currentEffort = currentClaudeEffort(session);
  const levels = supportedClaudeEffortLevelsForModelOption(currentModelOption, currentEffort);

  return [claudeEffortOption(DEFAULT_CLAUDE_EFFORT), ...levels.map(claudeEffortOption)];
}

function claudeEffortHint(session: Session) {
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

function normalizedCodexReasoningEffort(
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

function codexReasoningEffortComboboxOptions(session: Session, model: string = session.model) {
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

function codexReasoningEffortHint(session: Session, model: string = session.model) {
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

function sessionModelCapabilitySummary(option: SessionModelOption | null) {
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

function sessionModelOptionDescription(option: SessionModelOption | null) {
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

  return parts.join(" ");
}

function manualSessionModelPlaceholder(agent: AgentType): string {
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

function unknownSessionModelConfirmationKey(sessionId: string, model: string) {
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

function formatCodexReasoningEffortList(efforts: readonly CodexReasoningEffort[]) {
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

function createInitialWorkspaceBootstrap() {
  const storedLayout = getStoredWorkspaceLayout();
  const controlPanelSide: ControlPanelSide = storedLayout?.controlPanelSide ?? "left";
  const workspace = dockControlPanelAtWorkspaceEdge(
    ensureControlPanelInWorkspaceState(
      storedLayout?.workspace ?? {
        root: null,
        panes: [],
        activePaneId: null,
      },
    ),
    controlPanelSide,
  );

  return {
    controlPanelSide,
    workspace,
  };
}

export default function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [codexState, setCodexState] = useState<CodexState>({});
  const [agentReadiness, setAgentReadiness] = useState<AgentReadiness[]>([]);
  const initialWorkspaceBootstrapRef = useRef<ReturnType<typeof createInitialWorkspaceBootstrap> | null>(
    null,
  );
  if (!initialWorkspaceBootstrapRef.current) {
    initialWorkspaceBootstrapRef.current = createInitialWorkspaceBootstrap();
  }
  const initialWorkspaceBootstrap = initialWorkspaceBootstrapRef.current!;
  const [controlPanelSide, setControlPanelSide] = useState<ControlPanelSide>(
    initialWorkspaceBootstrap.controlPanelSide,
  );
  const [workspace, setWorkspace] = useState<WorkspaceState>(initialWorkspaceBootstrap.workspace);
  const [draftsBySessionId, setDraftsBySessionId] = useState<Record<string, string>>({});
  const [draftAttachmentsBySessionId, setDraftAttachmentsBySessionId] = useState<
    Record<string, DraftImageAttachment[]>
  >({});
  const [newSessionAgent, setNewSessionAgent] = useState<AgentType>("Codex");
  const [newSessionModelByAgent, setNewSessionModelByAgent] = useState<Record<AgentType, string>>(
    () => ({
      Claude: defaultNewSessionModel("Claude"),
      Codex: defaultNewSessionModel("Codex"),
      Cursor: defaultNewSessionModel("Cursor"),
      Gemini: defaultNewSessionModel("Gemini"),
    }),
  );
  const [isLoading, setIsLoading] = useState(true);
  const [isCreating, setIsCreating] = useState(false);
  const [sendingSessionIds, setSendingSessionIds] = useState<SessionFlagMap>({});
  const [stoppingSessionIds, setStoppingSessionIds] = useState<SessionFlagMap>({});
  const [killingSessionIds, setKillingSessionIds] = useState<SessionFlagMap>({});
  const [killRevealSessionId, setKillRevealSessionId] = useState<string | null>(null);
  const [pendingKillSessionId, setPendingKillSessionId] = useState<string | null>(null);
  const [updatingSessionIds, setUpdatingSessionIds] = useState<SessionFlagMap>({});
  const [refreshingSessionModelOptionIds, setRefreshingSessionModelOptionIds] =
    useState<SessionFlagMap>({});
  const [sessionModelOptionErrors, setSessionModelOptionErrors] = useState<SessionErrorMap>({});
  const [agentCommandsBySessionId, setAgentCommandsBySessionId] =
    useState<SessionAgentCommandMap>({});
  const [refreshingAgentCommandSessionIds, setRefreshingAgentCommandSessionIds] =
    useState<SessionFlagMap>({});
  const [agentCommandErrors, setAgentCommandErrors] = useState<SessionErrorMap>({});
  const [sessionSettingNotices, setSessionSettingNotices] = useState<SessionNoticeMap>({});
  const [requestError, setRequestError] = useState<string | null>(null);
  const [backendConnectionState, setBackendConnectionState] = useState<BackendConnectionState>(() =>
    readNavigatorOnline() ? "connecting" : "offline",
  );
  const [sessionListFilter, setSessionListFilter] = useState<SessionListFilter>("all");
  const [sessionListSearchQuery, setSessionListSearchQuery] = useState("");
  const [selectedProjectId, setSelectedProjectId] = useState<string>(ALL_PROJECTS_FILTER_ID);
  const [newProjectRootPath, setNewProjectRootPath] = useState("");
  const [newProjectRemoteId, setNewProjectRemoteId] = useState<string>(LOCAL_REMOTE_ID);
  const [isCreatingProject, setIsCreatingProject] = useState(false);
  const [themeId, setThemeId] = useState<ThemeId>(() => getStoredThemePreference());
  const [fontSizePx, setFontSizePx] = useState<number>(() => getStoredFontSizePreference());
  const [editorFontSizePx, setEditorFontSizePx] = useState<number>(() => getStoredEditorFontSizePreference());
  const [defaultCodexSandboxMode, setDefaultCodexSandboxMode] =
    useState<SandboxMode>("workspace-write");
  const [defaultCodexApprovalPolicy, setDefaultCodexApprovalPolicy] =
    useState<ApprovalPolicy>("never");
  const [defaultCodexReasoningEffort, setDefaultCodexReasoningEffort] =
    useState<CodexReasoningEffort>(DEFAULT_CODEX_REASONING_EFFORT);
  const [defaultClaudeApprovalMode, setDefaultClaudeApprovalMode] =
    useState<ClaudeApprovalMode>("ask");
  const [defaultClaudeEffort, setDefaultClaudeEffort] =
    useState<ClaudeEffortLevel>(DEFAULT_CLAUDE_EFFORT);
  const [remoteConfigs, setRemoteConfigs] = useState<RemoteConfig[]>(() =>
    resolveAppPreferences(null).remotes,
  );
  const [defaultCursorMode, setDefaultCursorMode] = useState<CursorMode>("agent");
  const [defaultGeminiApprovalMode, setDefaultGeminiApprovalMode] =
    useState<GeminiApprovalMode>("default");
  const [isCreateSessionOpen, setIsCreateSessionOpen] = useState(false);
  const [createSessionPaneId, setCreateSessionPaneId] = useState<string | null>(null);
  const [createSessionProjectId, setCreateSessionProjectId] = useState<string>(
    CREATE_SESSION_WORKSPACE_ID,
  );
  const [isCreateProjectOpen, setIsCreateProjectOpen] = useState(false);
  const [controlPanelFilesystemRoot, setControlPanelFilesystemRoot] = useState<string | null>(null);
  const [controlPanelGitWorkdir, setControlPanelGitWorkdir] = useState<string | null>(null);
  const [controlPanelGitStatusCount, setControlPanelGitStatusCount] = useState(0);
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<PreferencesTabId>("themes");
  const [pendingSessionRename, setPendingSessionRename] = useState<PendingSessionRename | null>(
    null,
  );
  const [pendingSessionRenameDraft, setPendingSessionRenameDraft] = useState("");
  const [pendingSessionRenameStyle, setPendingSessionRenameStyle] = useState<CSSProperties | null>(
    null,
  );
  const [pendingKillPopoverStyle, setPendingKillPopoverStyle] = useState<CSSProperties | null>(
    null,
  );
  const [pendingScrollToBottomRequest, setPendingScrollToBottomRequest] = useState<{
    sessionId: string;
    token: number;
  } | null>(null);
  const [windowId] = useState(() => crypto.randomUUID());
  const [draggedTab, setDraggedTab] = useState<WorkspaceTabDrag | null>(null);
  const [externalDraggedTab, setExternalDraggedTab] = useState<WorkspaceTabDrag | null>(null);
  const resizeStateRef = useRef<{
    splitId: string;
    direction: "row" | "column";
    startRatio: number;
    minRatio: number;
    maxRatio: number;
    startX: number;
    startY: number;
    size: number;
  } | null>(null);
  const draftAttachmentsRef = useRef<Record<string, DraftImageAttachment[]>>({});
  const dragChannelRef = useRef<BroadcastChannel | null>(null);
  const draggedTabRef = useRef<WorkspaceTabDrag | null>(null);
  const isMountedRef = useRef(true);
  const sessionListSearchInputRef = useRef<HTMLInputElement>(null);
  const pendingSessionRenameTriggerRef = useRef<HTMLElement | null>(null);
  const pendingSessionRenamePopoverRef = useRef<HTMLFormElement | null>(null);
  const pendingSessionRenameInputRef = useRef<HTMLInputElement | null>(null);
  const pendingSessionRenameCloseTimeoutRef = useRef<number | null>(null);
  const pendingKillTriggerRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillPopoverRef = useRef<HTMLDivElement | null>(null);
  const pendingKillConfirmButtonRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillCloseTimeoutRef = useRef<number | null>(null);
  const confirmedUnknownModelSendsRef = useRef<Set<string>>(new Set());
  const refreshingSessionModelOptionIdsRef = useRef<SessionFlagMap>({});
  const refreshingAgentCommandSessionIdsRef = useRef<SessionFlagMap>({});
  const controlPanelSurfaceRef = useRef<ControlPanelSurfaceHandle | null>(null);
  const lastDerivedControlPanelFilesystemRootRef = useRef<string | null>(null);
  const lastDerivedControlPanelGitWorkdirRef = useRef<string | null>(null);
  const sessionsRef = useRef<Session[]>([]);
  const latestStateRevisionRef = useRef<number | null>(null);
  const forceAdoptNextStateEventRef = useRef(false);
  const stateResyncInFlightRef = useRef(false);
  const stateResyncPendingRef = useRef(false);
  const paneShouldStickToBottomRef = useRef<Record<string, boolean | undefined>>({});
  const paneScrollPositionsRef = useRef<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >({});
  const paneContentSignaturesRef = useRef<Record<string, Record<string, string>>>({});

  const projectLookup = useMemo(
    () => new Map(projects.map((project) => [project.id, project])),
    [projects],
  );
  const agentReadinessByAgent = useMemo(
    () => new Map(agentReadiness.map((entry) => [entry.agent, entry])),
    [agentReadiness],
  );
  const sessionLookup = useMemo(
    () => new Map(sessions.map((session) => [session.id, session])),
    [sessions],
  );
  const paneLookup = useMemo(
    () => new Map(workspace.panes.map((pane) => [pane.id, pane])),
    [workspace.panes],
  );
  const activePane =
    workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? workspace.panes[0] ?? null;
  const activeSession = activePane?.activeSessionId
    ? (sessionLookup.get(activePane.activeSessionId) ?? null)
    : null;
  const openSessionIds = useMemo(
    () =>
      new Set(
        workspace.panes.flatMap((pane) =>
          pane.tabs.flatMap((tab) => (tab.kind === "session" ? [tab.sessionId] : [])),
        ),
      ),
    [workspace.panes],
  );
  const workspaceHasOnlyControlPanel = useMemo(
    () =>
      workspace.panes.length === 1 &&
      workspace.panes[0]?.tabs.length === 1 &&
      workspace.panes[0]?.tabs[0]?.kind === "controlPanel",
    [workspace.panes],
  );
  const selectedProject =
    selectedProjectId === ALL_PROJECTS_FILTER_ID
      ? null
      : (projectLookup.get(selectedProjectId) ?? null);
  const remoteLookup = useMemo(
    () => new Map(remoteConfigs.map((remote) => [remote.id, remote])),
    [remoteConfigs],
  );
  const localRemoteConfig = remoteLookup.get(LOCAL_REMOTE_ID) ?? createBuiltinLocalRemote();
  const enabledProjectRemotes = useMemo(
    () => remoteConfigs.filter((remote) => remote.enabled || isLocalRemoteId(remote.id)),
    [remoteConfigs],
  );
  const newProjectSelectedRemote = resolveRemoteConfig(remoteLookup, newProjectRemoteId);
  const newProjectUsesLocalRemote = isLocalRemoteId(newProjectRemoteId);
  const createProjectRemoteOptions = useMemo<readonly ComboboxOption[]>(() => {
    return enabledProjectRemotes.map((remote) => ({
      label: remoteDisplayName(remote, remote.id),
      value: remote.id,
      description: remoteConnectionLabel(remote),
      badges: [remoteBadgeLabel(remote)],
    }));
  }, [enabledProjectRemotes]);
  const newSessionModelOptions = NEW_SESSION_MODEL_OPTIONS[newSessionAgent];
  const newSessionModel =
    newSessionModelByAgent[newSessionAgent] ?? defaultNewSessionModel(newSessionAgent);
  const createSessionSelectedProject =
    createSessionProjectId === CREATE_SESSION_WORKSPACE_ID
      ? null
      : (projectLookup.get(createSessionProjectId) ?? null);
  const createSessionWorkspaceProject =
    createSessionProjectId === CREATE_SESSION_WORKSPACE_ID &&
    activeSession?.projectId &&
    projectLookup.has(activeSession.projectId)
      ? (projectLookup.get(activeSession.projectId) ?? null)
      : null;
  const createSessionEffectiveProject =
    createSessionSelectedProject ??
    (createSessionWorkspaceProject &&
    !isLocalRemoteId(resolveProjectRemoteId(createSessionWorkspaceProject))
      ? createSessionWorkspaceProject
      : null);
  const createSessionSelectedRemote = createSessionEffectiveProject
    ? resolveRemoteConfig(remoteLookup, resolveProjectRemoteId(createSessionEffectiveProject))
    : localRemoteConfig;
  const createSessionProjectOptions = useMemo<readonly ComboboxOption[]>(() => {
    const workspaceLabel = activeSession?.workdir ? "Current workspace" : "Default workspace";

    return [
      { label: workspaceLabel, value: CREATE_SESSION_WORKSPACE_ID },
      ...projects.map((project) => {
        const remote = resolveRemoteConfig(remoteLookup, resolveProjectRemoteId(project));
        return {
          label: project.name,
          value: project.id,
          description: describeProjectScope(project, remoteLookup),
          badges: [remoteBadgeLabel(remote)],
        };
      }),
    ];
  }, [activeSession?.workdir, projects, remoteLookup]);
  const controlPanelProjectOptions = useMemo<readonly ComboboxOption[]>(() => {
    return [
      {
        label: "All projects",
        value: ALL_PROJECTS_FILTER_ID,
        description: "Show every session in this window.",
      },
      ...projects.map((project) => {
        const remote = resolveRemoteConfig(remoteLookup, resolveProjectRemoteId(project));
        return {
          label: project.name,
          value: project.id,
          description: describeProjectScope(project, remoteLookup),
          badges: [remoteBadgeLabel(remote)],
        };
      }),
    ];
  }, [projects, remoteLookup]);
  const createSessionProjectHint = createSessionSelectedProject
    ? describeProjectScope(createSessionSelectedProject, remoteLookup)
    : createSessionEffectiveProject
      ? describeProjectScope(createSessionEffectiveProject, remoteLookup)
      : activeSession?.workdir
        ? `Uses ${activeSession.workdir}`
        : "Uses the app default workspace.";
  const createSessionUsesRemoteProject =
    !!createSessionEffectiveProject &&
    !isLocalRemoteId(resolveProjectRemoteId(createSessionEffectiveProject));
  const createSessionProjectSelectionError =
    createSessionProjectId === CREATE_SESSION_WORKSPACE_ID &&
    !!activeSession?.projectId &&
    !projectLookup.has(activeSession.projectId)
      ? "The current workspace is tied to a project that is no longer available. Choose a project before creating a session."
      : null;
  const createSessionUsesSessionModelPicker = usesSessionModelPicker(newSessionAgent);
  const createSessionAgentReadiness = createSessionUsesRemoteProject
    ? null
    : agentReadinessByAgent.get(newSessionAgent) ?? null;
  const createSessionBlocked = createSessionAgentReadiness?.blocking ?? false;
  const selectedProjectRemoteId = selectedProject ? resolveProjectRemoteId(selectedProject) : null;
  const derivedControlPanelWorkspaceRoot = selectedProject
    ? selectedProjectRemoteId && isLocalRemoteId(selectedProjectRemoteId)
      ? selectedProject.rootPath
      : null
    : activeSession?.workdir ?? sessions[0]?.workdir ?? null;
  const derivedControlPanelFilesystemRoot = derivedControlPanelWorkspaceRoot;
  const derivedControlPanelGitWorkdir = derivedControlPanelWorkspaceRoot;
  const projectScopedSessions = useMemo(() => {
    if (!selectedProject) {
      return sessions;
    }

    return sessions.filter((session) => session.projectId === selectedProject.id);
  }, [selectedProject, sessions]);
  const controlPanelSessionId = useMemo(() => {
    if (!selectedProject) {
      return activeSession?.id ?? sessions[0]?.id ?? null;
    }

    if (activeSession?.projectId === selectedProject.id) {
      return activeSession.id;
    }

    return projectScopedSessions[0]?.id ?? null;
  }, [activeSession?.id, activeSession?.projectId, projectScopedSessions, selectedProject, sessions]);
  const sessionFilterCounts = useMemo(
    () => countSessionsByFilter(projectScopedSessions),
    [projectScopedSessions],
  );
  const statusFilteredSessions = useMemo(() => {
    return filterSessionsByListFilter(projectScopedSessions, sessionListFilter);
  }, [projectScopedSessions, sessionListFilter]);
  const trimmedSessionListSearchQuery = sessionListSearchQuery.trim();
  const deferredSessionListSearchQuery = useDeferredValue(trimmedSessionListSearchQuery);
  const effectiveSessionListSearchQuery =
    trimmedSessionListSearchQuery.length === 0 ? "" : deferredSessionListSearchQuery;
  const hasSessionListSearch = effectiveSessionListSearchQuery.length > 0;
  const sessionListSearchIndex = useMemo(() => {
    if (!hasSessionListSearch) {
      return null;
    }

    return new Map(
      statusFilteredSessions.map((session) => [session.id, buildSessionSearchIndex(session)] as const),
    );
  }, [hasSessionListSearch, statusFilteredSessions]);

  useEffect(() => {
    if (
      createSessionProjectId !== CREATE_SESSION_WORKSPACE_ID &&
      !projectLookup.has(createSessionProjectId)
    ) {
      setCreateSessionProjectId(CREATE_SESSION_WORKSPACE_ID);
    }
  }, [createSessionProjectId, projectLookup]);

  useEffect(() => {
    if (!enabledProjectRemotes.some((remote) => remote.id === newProjectRemoteId)) {
      setNewProjectRemoteId(enabledProjectRemotes[0]?.id ?? LOCAL_REMOTE_ID);
    }
  }, [enabledProjectRemotes, newProjectRemoteId]);

  useEffect(() => {
    const previousDerived = lastDerivedControlPanelFilesystemRootRef.current?.trim() ?? "";
    lastDerivedControlPanelFilesystemRootRef.current = derivedControlPanelFilesystemRoot;

    setControlPanelFilesystemRoot((current) => {
      const trimmedCurrent = current?.trim() ?? "";
      if (!trimmedCurrent || trimmedCurrent === previousDerived) {
        return derivedControlPanelFilesystemRoot;
      }
      return current;
    });
  }, [derivedControlPanelFilesystemRoot]);

  useEffect(() => {
    const previousDerived = lastDerivedControlPanelGitWorkdirRef.current?.trim() ?? "";
    lastDerivedControlPanelGitWorkdirRef.current = derivedControlPanelGitWorkdir;

    setControlPanelGitWorkdir((current) => {
      const trimmedCurrent = current?.trim() ?? "";
      if (!trimmedCurrent || trimmedCurrent === previousDerived) {
        return derivedControlPanelGitWorkdir;
      }
      return current;
    });
  }, [derivedControlPanelGitWorkdir]);

  useEffect(() => {
    const normalizedGitWorkdir = controlPanelGitWorkdir?.trim() ?? "";
    let cancelled = false;

    if (!normalizedGitWorkdir) {
      setControlPanelGitStatusCount(0);
      return;
    }

    void fetchGitStatus(normalizedGitWorkdir, null)
      .then((status) => {
        if (cancelled) {
          return;
        }
        setControlPanelGitStatusCount(status.files.length);
      })
      .catch(() => {
        if (cancelled) {
          return;
        }
        setControlPanelGitStatusCount(0);
      });

    return () => {
      cancelled = true;
    };
  }, [controlPanelGitWorkdir]);

  const sessionListSearchResults = useMemo(() => {
    if (!hasSessionListSearch || !sessionListSearchIndex) {
      return new Map<string, SessionListSearchResult>();
    }

    return new Map(
      statusFilteredSessions.flatMap((session) => {
        const searchIndex = sessionListSearchIndex.get(session.id);
        if (!searchIndex) {
          return [];
        }

        const result = buildSessionListSearchResultFromIndex(
          searchIndex,
          effectiveSessionListSearchQuery,
        );
        return result ? ([[session.id, result]] as const) : [];
      }),
    );
  }, [
    effectiveSessionListSearchQuery,
    hasSessionListSearch,
    sessionListSearchIndex,
    statusFilteredSessions,
  ]);
  const filteredSessions = useMemo(() => {
    if (!hasSessionListSearch) {
      return statusFilteredSessions;
    }

    return statusFilteredSessions.filter((session) => sessionListSearchResults.has(session.id));
  }, [hasSessionListSearch, sessionListSearchResults, statusFilteredSessions]);
  const projectSessionCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const session of sessions) {
      if (!session.projectId) {
        continue;
      }
      counts.set(session.projectId, (counts.get(session.projectId) ?? 0) + 1);
    }
    return counts;
  }, [sessions]);
  const activeTheme = THEMES.find((theme) => theme.id === themeId) ?? THEMES[0];
  const editorAppearance: MonacoAppearance = isHexColorDark(activeTheme.swatches[0]) ? "dark" : "light";
  const activeDraggedTab = draggedTab ?? externalDraggedTab;

  function focusSessionListSearch(selectAll = false) {
    controlPanelSurfaceRef.current?.selectSection("sessions");
    window.requestAnimationFrame(() => {
      const input = sessionListSearchInputRef.current;
      if (!input) {
        return;
      }

      input.focus();
      if (selectAll) {
        input.select();
      }
    });
  }

  function broadcastTabDragMessage(message: WorkspaceTabDragChannelMessage) {
    dragChannelRef.current?.postMessage(message);
  }

  function applyControlPanelLayout(
    nextWorkspace: WorkspaceState,
    side: "left" | "right" = controlPanelSide,
  ) {
    return dockControlPanelAtWorkspaceEdge(
      ensureControlPanelInWorkspaceState(nextWorkspace),
      side,
    );
  }

  function adoptSessions(
    nextSessions: Session[],
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    const previousSessions = sessionsRef.current;
    const previousSessionsById = new Map(previousSessions.map((session) => [session.id, session]));
    const mergedSessions = reconcileSessions(previousSessions, nextSessions);
    const availableSessionIds = new Set(mergedSessions.map((session) => session.id));
    const sessionsWithChangedWorkdir = new Set(
      mergedSessions.flatMap((session) => {
        const previousSession = previousSessionsById.get(session.id);
        return previousSession && previousSession.workdir !== session.workdir ? [session.id] : [];
      }),
    );

    sessionsRef.current = mergedSessions;
    setSessions(mergedSessions);
    setWorkspace((current) => {
      const reconciled = applyControlPanelLayout(reconcileWorkspaceState(current, mergedSessions));
      if (!options?.openSessionId) {
        return reconciled;
      }

      return openSessionInWorkspaceState(reconciled, options.openSessionId, options.paneId ?? null);
    });
    setDraftsBySessionId((current) => pruneSessionValues(current, availableSessionIds));
    setDraftAttachmentsBySessionId((current) =>
      pruneSessionAttachmentValues(current, availableSessionIds),
    );
    setSendingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setStoppingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setKillingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setKillRevealSessionId((current) =>
      current && availableSessionIds.has(current) ? current : null,
    );
    setPendingKillSessionId((current) =>
      current && availableSessionIds.has(current) ? current : null,
    );
    setPendingSessionRename((current) =>
      current && availableSessionIds.has(current.sessionId) ? current : null,
    );
    setUpdatingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setAgentCommandsBySessionId((current) =>
      pruneSessionCommandValues(current, availableSessionIds, sessionsWithChangedWorkdir),
    );
    setRefreshingAgentCommandSessionIds((current) =>
      pruneSessionFlagsWithInvalidation(current, availableSessionIds, sessionsWithChangedWorkdir),
    );
    refreshingAgentCommandSessionIdsRef.current = pruneSessionFlagsWithInvalidation(
      refreshingAgentCommandSessionIdsRef.current,
      availableSessionIds,
      sessionsWithChangedWorkdir,
    );
    setAgentCommandErrors((current) =>
      pruneSessionValues(current, availableSessionIds, sessionsWithChangedWorkdir),
    );
    setSessionSettingNotices((current) => pruneSessionValues(current, availableSessionIds));
    const availableUnknownModelKeys = new Set(
      mergedSessions
        .filter((session) => describeUnknownSessionModelWarning(session))
        .map((session) => unknownSessionModelConfirmationKey(session.id, session.model)),
    );
    confirmedUnknownModelSendsRef.current = new Set(
      [...confirmedUnknownModelSendsRef.current].filter((key) => availableUnknownModelKeys.has(key)),
    );
  }

  function updateSessionLocally(
    sessionId: string,
    update: (session: Session) => Session,
  ) {
    const nextSessions = sessionsRef.current.map((entry) => {
      if (entry.id !== sessionId) {
        return entry;
      }

      return update(entry);
    });
    const hasChanged = nextSessions.some((entry, index) => entry !== sessionsRef.current[index]);
    if (!hasChanged) {
      return;
    }

    sessionsRef.current = nextSessions;
    setSessions(nextSessions);
    setWorkspace((current) =>
      applyControlPanelLayout(reconcileWorkspaceState(current, nextSessions)),
    );
  }

  function buildOptimisticSessionSettingsUpdate(
    session: Session,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) {
    const normalizedModelValue =
      field === "model" ? normalizedRequestedSessionModel(session, value as string) : null;

    switch (session.agent) {
      case "Codex": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextReasoningEffort =
          field === "reasoningEffort"
            ? (value as CodexReasoningEffort)
            : normalizedCodexReasoningEffort(session, nextModel);
        const nextSandboxMode =
          field === "sandboxMode" ? (value as SandboxMode) : session.sandboxMode;
        const nextApprovalPolicy =
          field === "approvalPolicy" ? (value as ApprovalPolicy) : session.approvalPolicy;

        if (
          nextModel === session.model &&
          nextReasoningEffort === session.reasoningEffort &&
          nextSandboxMode === session.sandboxMode &&
          nextApprovalPolicy === session.approvalPolicy
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          reasoningEffort: nextReasoningEffort,
          sandboxMode: nextSandboxMode,
          approvalPolicy: nextApprovalPolicy,
        };
      }
      case "Cursor": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextCursorMode =
          field === "cursorMode" ? (value as CursorMode) : session.cursorMode;

        if (nextModel === session.model && nextCursorMode === session.cursorMode) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          cursorMode: nextCursorMode,
        };
      }
      case "Claude": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextClaudeApprovalMode =
          field === "claudeApprovalMode"
            ? (value as ClaudeApprovalMode)
            : session.claudeApprovalMode;
        const nextClaudeEffort =
          field === "claudeEffort" ? (value as ClaudeEffortLevel) : session.claudeEffort;

        if (
          nextModel === session.model &&
          nextClaudeApprovalMode === session.claudeApprovalMode &&
          nextClaudeEffort === session.claudeEffort
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          claudeApprovalMode: nextClaudeApprovalMode,
          claudeEffort: nextClaudeEffort,
        };
      }
      case "Gemini": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextGeminiApprovalMode =
          field === "geminiApprovalMode"
            ? (value as GeminiApprovalMode)
            : session.geminiApprovalMode;

        if (nextModel === session.model && nextGeminiApprovalMode === session.geminiApprovalMode) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          geminiApprovalMode: nextGeminiApprovalMode,
        };
      }
    }
  }

  function rollbackOptimisticSessionSettingsUpdate(
    currentSession: Session,
    previousSession: Session,
    optimisticSession: Session,
  ) {
    let changed = false;
    const nextSession = { ...currentSession };

    if (currentSession.model === optimisticSession.model && currentSession.model !== previousSession.model) {
      nextSession.model = previousSession.model;
      changed = true;
    }
    if (
      currentSession.approvalPolicy === optimisticSession.approvalPolicy &&
      currentSession.approvalPolicy !== previousSession.approvalPolicy
    ) {
      nextSession.approvalPolicy = previousSession.approvalPolicy;
      changed = true;
    }
    if (
      currentSession.reasoningEffort === optimisticSession.reasoningEffort &&
      currentSession.reasoningEffort !== previousSession.reasoningEffort
    ) {
      nextSession.reasoningEffort = previousSession.reasoningEffort;
      changed = true;
    }
    if (
      currentSession.sandboxMode === optimisticSession.sandboxMode &&
      currentSession.sandboxMode !== previousSession.sandboxMode
    ) {
      nextSession.sandboxMode = previousSession.sandboxMode;
      changed = true;
    }
    if (currentSession.cursorMode === optimisticSession.cursorMode && currentSession.cursorMode !== previousSession.cursorMode) {
      nextSession.cursorMode = previousSession.cursorMode;
      changed = true;
    }
    if (
      currentSession.claudeApprovalMode === optimisticSession.claudeApprovalMode &&
      currentSession.claudeApprovalMode !== previousSession.claudeApprovalMode
    ) {
      nextSession.claudeApprovalMode = previousSession.claudeApprovalMode;
      changed = true;
    }
    if (currentSession.claudeEffort === optimisticSession.claudeEffort && currentSession.claudeEffort !== previousSession.claudeEffort) {
      nextSession.claudeEffort = previousSession.claudeEffort;
      changed = true;
    }
    if (
      currentSession.geminiApprovalMode === optimisticSession.geminiApprovalMode &&
      currentSession.geminiApprovalMode !== previousSession.geminiApprovalMode
    ) {
      nextSession.geminiApprovalMode = previousSession.geminiApprovalMode;
      changed = true;
    }

    return changed ? nextSession : currentSession;
  }

  function syncPreferencesFromState(nextState: StateResponse) {
    const preferences = resolveAppPreferences(nextState.preferences);
    setDefaultCodexReasoningEffort(preferences.defaultCodexReasoningEffort);
    setDefaultClaudeEffort(preferences.defaultClaudeEffort);
    setRemoteConfigs((current) =>
      areRemoteConfigsEqual(current, preferences.remotes) ? current : preferences.remotes,
    );
  }

  function adoptState(
    nextState: StateResponse,
    options?: { force?: boolean; openSessionId?: string; paneId?: string | null },
  ) {
    if (!isMountedRef.current) {
      return false;
    }

    if (
      !options?.force &&
      !shouldAdoptStateRevision(latestStateRevisionRef.current, nextState.revision)
    ) {
      return false;
    }

    latestStateRevisionRef.current = nextState.revision;
    setCodexState(nextState.codex ?? {});
    setAgentReadiness(nextState.agentReadiness ?? []);
    syncPreferencesFromState(nextState);
    setProjects(nextState.projects ?? []);
    adoptSessions(nextState.sessions, options);
    if (options?.openSessionId) {
      const openedSession = nextState.sessions.find((session) => session.id === options.openSessionId);
      setSelectedProjectId(openedSession?.projectId ?? ALL_PROJECTS_FILTER_ID);
    }
    return true;
  }


  async function persistAppPreferences(payload: {
    defaultCodexReasoningEffort?: CodexReasoningEffort;
    defaultClaudeEffort?: ClaudeEffortLevel;
    remotes?: RemoteConfig[];
  }) {
    try {
      const state = await updateAppSettings(payload);
      if (!isMountedRef.current) {
        return;
      }
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      setRequestError(getErrorMessage(error));
      try {
        const state = await fetchState();
        if (!isMountedRef.current) {
          return;
        }
        syncPreferencesFromState(state);
        adoptState(state);
      } catch {
        // Keep the optimistic selection until the next successful sync.
      }
    }
  }

  function handleDefaultCodexReasoningEffortChange(nextValue: CodexReasoningEffort) {
    if (nextValue === defaultCodexReasoningEffort) {
      return;
    }

    setDefaultCodexReasoningEffort(nextValue);
    void persistAppPreferences({ defaultCodexReasoningEffort: nextValue });
  }

  function handleDefaultClaudeEffortChange(nextValue: ClaudeEffortLevel) {
    if (nextValue === defaultClaudeEffort) {
      return;
    }

    setDefaultClaudeEffort(nextValue);
    void persistAppPreferences({ defaultClaudeEffort: nextValue });
  }

  useEffect(() => {
    function handleBrowserOnline() {
      setBackendConnectionState((current) => {
        if (current === "connected") {
          return current;
        }
        return latestStateRevisionRef.current === null ? "connecting" : "reconnecting";
      });
    }

    function handleBrowserOffline() {
      setBackendConnectionState("offline");
    }

    window.addEventListener("online", handleBrowserOnline);
    window.addEventListener("offline", handleBrowserOffline);
    return () => {
      window.removeEventListener("online", handleBrowserOnline);
      window.removeEventListener("offline", handleBrowserOffline);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const eventSource = new EventSource("/api/events");

    function requestStateResync() {
      if (cancelled) {
        return;
      }

      stateResyncPendingRef.current = true;
      if (stateResyncInFlightRef.current) {
        return;
      }

      stateResyncInFlightRef.current = true;
      void (async () => {
        try {
          while (!cancelled && stateResyncPendingRef.current) {
            stateResyncPendingRef.current = false;

            try {
              const state = await fetchState();
              if (cancelled) {
                return;
              }

              adoptState(state);
              setRequestError(null);
            } catch (error) {
              if (!cancelled) {
                setRequestError(getErrorMessage(error));
              }
              break;
            } finally {
              if (!cancelled) {
                setIsLoading(false);
              }
            }
          }
        } finally {
          stateResyncInFlightRef.current = false;
        }
      })();
    }

    function handleStateEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const state = JSON.parse(event.data) as StateResponse;
        const force = forceAdoptNextStateEventRef.current;
        forceAdoptNextStateEventRef.current = false;
        adoptState(state, { force });
        setRequestError(null);
      } catch (error) {
        if (!cancelled) {
          setRequestError(getErrorMessage(error));
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }

    function handleDeltaEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const delta = JSON.parse(event.data) as DeltaEvent;
        const revisionAction = decideDeltaRevisionAction(
          latestStateRevisionRef.current,
          delta.revision,
        );
        if (revisionAction === "ignore") {
          setRequestError(null);
          return;
        }
        if (revisionAction === "resync") {
          requestStateResync();
          return;
        }

        const result = applyDeltaToSessions(sessionsRef.current, delta);
        if (result.kind === "applied") {
          latestStateRevisionRef.current = delta.revision;
          sessionsRef.current = result.sessions;
          startTransition(() => {
            setSessions(result.sessions);
          });
          setRequestError(null);
          return;
        }

        requestStateResync();
      } catch {
        requestStateResync();
      }
    }

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.addEventListener("delta", handleDeltaEvent as EventListener);
    eventSource.onopen = () => {
      if (!cancelled) {
        if (latestStateRevisionRef.current !== null) {
          // A restarted backend can reconnect with the same persisted revision but a
          // more complete authoritative snapshot than the client currently has.
          forceAdoptNextStateEventRef.current = true;
        }
        setBackendConnectionState("connected");
        setRequestError(null);
      }
    };
    eventSource.onerror = () => {
      if (cancelled) {
        return;
      }

      const isOnline = readNavigatorOnline();
      setBackendConnectionState(
        isOnline
          ? latestStateRevisionRef.current === null
            ? "connecting"
            : "reconnecting"
          : "offline",
      );
      if (isOnline && latestStateRevisionRef.current === null) {
        // The SSE endpoint emits a full state snapshot on connect, so once the client has an
        // established revision it is cheaper to wait for the reconnect snapshot than to
        // immediately force a duplicate /api/state fetch.
        requestStateResync();
      }
    };

    return () => {
      cancelled = true;
      eventSource.removeEventListener("state", handleStateEvent as EventListener);
      eventSource.removeEventListener("delta", handleDeltaEvent as EventListener);
      eventSource.close();
    };
  }, []);

  useEffect(() => {
    setSelectedProjectId((current) => {
      if (current === ALL_PROJECTS_FILTER_ID) {
        return current;
      }

      if (projects.some((project) => project.id === current)) {
        return current;
      }

      if (
        activeSession?.projectId &&
        projects.some((project) => project.id === activeSession.projectId)
      ) {
        return activeSession.projectId;
      }

      return projects[0]?.id ?? ALL_PROJECTS_FILTER_ID;
    });
  }, [activeSession?.projectId, projects]);

  useEffect(() => {
    if (activeSession) {
      setNewSessionAgent(activeSession.agent);
    }
  }, [activeSession?.id]);

  useLayoutEffect(() => {
    applyThemePreference(themeId);
    persistThemePreference(themeId);
  }, [themeId]);

  useLayoutEffect(() => {
    applyFontSizePreference(fontSizePx);
    persistFontSizePreference(fontSizePx);
  }, [fontSizePx]);

  useEffect(() => {
    persistEditorFontSizePreference(editorFontSizePx);
  }, [editorFontSizePx]);

  useEffect(() => {
    persistWorkspaceLayout({
      controlPanelSide,
      workspace: applyControlPanelLayout(workspace, controlPanelSide),
    });
  }, [controlPanelSide, workspace]);

  useEffect(() => {
    sessionsRef.current = sessions;
  }, [sessions]);

  useEffect(() => {
    draftAttachmentsRef.current = draftAttachmentsBySessionId;
  }, [draftAttachmentsBySessionId]);

  useEffect(() => {
    if (typeof BroadcastChannel === "undefined") {
      return;
    }

    const channel = new BroadcastChannel(TAB_DRAG_CHANNEL_NAME);
    dragChannelRef.current = channel;
    channel.onmessage = (event: MessageEvent<unknown>) => {
      const message = event.data;
      if (!isWorkspaceTabDragChannelMessage(message)) {
        return;
      }

      switch (message.type) {
        case "drag-start":
          if (message.payload.sourceWindowId !== windowId) {
            setExternalDraggedTab(message.payload);
          }
          break;
        case "drag-end":
          setExternalDraggedTab((current) =>
            current?.dragId === message.dragId ? null : current,
          );
          break;
        case "drop-commit":
          if (message.sourceWindowId !== windowId) {
            break;
          }

          if (draggedTabRef.current?.dragId === message.dragId) {
            draggedTabRef.current = null;
          }
          setDraggedTab((current) => (current?.dragId === message.dragId ? null : current));
          setWorkspace((current) =>
            applyControlPanelLayout(closeWorkspaceTab(current, message.sourcePaneId, message.tabId)),
          );
          break;
      }
    };

    return () => {
      channel.close();
      if (dragChannelRef.current === channel) {
        dragChannelRef.current = null;
      }
    };
  }, [windowId]);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    return () => {
      releaseDraftAttachments(Object.values(draftAttachmentsRef.current).flat());
    };
  }, []);

  useEffect(() => {
    return () => {
      clearPendingKillCloseTimeout();
      clearPendingSessionRenameCloseTimeout();
    };
  }, []);

  useEffect(() => {
    if (!pendingKillSessionId) {
      clearPendingKillCloseTimeout();
      return;
    }

    const focusFrameId = window.requestAnimationFrame(() => {
      pendingKillConfirmButtonRef.current?.focus();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePendingKillConfirmation(true);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrameId);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [pendingKillSessionId]);

  useLayoutEffect(() => {
    if (!pendingKillSessionId) {
      setPendingKillPopoverStyle(null);
      return;
    }

    setPendingKillPopoverStyle({
      left: 0,
      top: 0,
      visibility: "hidden",
    });

    function updatePendingKillPopoverStyle() {
      const trigger = pendingKillTriggerRef.current;
      const popover = pendingKillPopoverRef.current;
      if (!trigger || !popover) {
        return;
      }

      const triggerRect = trigger.getBoundingClientRect();
      const popoverRect = popover.getBoundingClientRect();
      const viewportPadding = 12;
      const preferredLeft = triggerRect.left + triggerRect.width / 2 - popoverRect.width / 2;
      const left = clamp(
        preferredLeft,
        viewportPadding,
        window.innerWidth - popoverRect.width - viewportPadding,
      );
      const preferredTop = triggerRect.top - 10;
      const top = clamp(
        preferredTop,
        viewportPadding,
        window.innerHeight - popoverRect.height - viewportPadding,
      );

      setPendingKillPopoverStyle({
        left,
        top,
      });
    }

    const frameId = window.requestAnimationFrame(updatePendingKillPopoverStyle);
    window.addEventListener("resize", updatePendingKillPopoverStyle);
    window.addEventListener("scroll", updatePendingKillPopoverStyle, true);

    return () => {
      window.cancelAnimationFrame(frameId);
      window.removeEventListener("resize", updatePendingKillPopoverStyle);
      window.removeEventListener("scroll", updatePendingKillPopoverStyle, true);
    };
  }, [pendingKillSessionId]);

  useEffect(() => {
    if (!pendingSessionRename) {
      clearPendingSessionRenameCloseTimeout();
      return;
    }

    const focusFrameId = window.requestAnimationFrame(() => {
      pendingSessionRenameInputRef.current?.focus();
      pendingSessionRenameInputRef.current?.select();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePendingSessionRename(true);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrameId);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [pendingSessionRename]);

  useLayoutEffect(() => {
    if (!pendingSessionRename) {
      setPendingSessionRenameStyle(null);
      return;
    }

    const renameAnchor = pendingSessionRename;

    setPendingSessionRenameStyle({
      left: 0,
      top: 0,
      visibility: "hidden",
    });

    function updatePendingSessionRenameStyle() {
      const popover = pendingSessionRenamePopoverRef.current;
      if (!popover) {
        return;
      }

      const popoverRect = popover.getBoundingClientRect();
      const viewportPadding = 12;
      const left = clamp(
        renameAnchor.clientX - popoverRect.width / 2,
        viewportPadding,
        window.innerWidth - popoverRect.width - viewportPadding,
      );
      const top = clamp(
        renameAnchor.clientY - 18,
        viewportPadding,
        window.innerHeight - popoverRect.height - viewportPadding,
      );

      setPendingSessionRenameStyle({
        left,
        top,
      });
    }

    const frameId = window.requestAnimationFrame(updatePendingSessionRenameStyle);
    window.addEventListener("resize", updatePendingSessionRenameStyle);

    return () => {
      window.cancelAnimationFrame(frameId);
      window.removeEventListener("resize", updatePendingSessionRenameStyle);
    };
  }, [pendingSessionRename]);

  useEffect(() => {
    if (!isSettingsOpen) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsSettingsOpen(false);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isSettingsOpen]);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      const key = event.key.toLowerCase();
      const hasPrimaryModifier = event.metaKey || event.ctrlKey;
      if (
        event.defaultPrevented ||
        key !== "f" ||
        !hasPrimaryModifier ||
        !event.shiftKey ||
        event.altKey ||
        isSettingsOpen ||
        pendingKillSessionId
      ) {
        return;
      }

      event.preventDefault();
      focusSessionListSearch(true);
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isSettingsOpen, pendingKillSessionId]);

  useEffect(() => {
    function handlePointerMove(event: PointerEvent) {
      const resizeState = resizeStateRef.current;
      if (!resizeState) {
        return;
      }

      const delta =
        resizeState.direction === "row"
          ? event.clientX - resizeState.startX
          : event.clientY - resizeState.startY;
      const nextRatio = clamp(
        resizeState.startRatio + delta / Math.max(resizeState.size, 1),
        resizeState.minRatio,
        resizeState.maxRatio,
      );

      setWorkspace((current) => updateSplitRatio(current, resizeState.splitId, nextRatio));
    }

    function handlePointerUp() {
      resizeStateRef.current = null;
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
  }, []);

  function handleSend(
    sessionId: string,
    draftTextOverride?: string,
    expandedTextOverride?: string | null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return false;
    }

    const draftText = draftTextOverride ?? draftsBySessionId[sessionId] ?? "";
    const prompt = draftText.trim();
    const expandedText = expandedTextOverride?.trim() || null;
    const normalizedExpandedText = expandedText && expandedText !== prompt ? expandedText : null;
    const attachments = draftAttachmentsBySessionId[sessionId] ?? [];
    if (!prompt && attachments.length === 0) {
      return false;
    }
    if (
      session.agent === "Codex" &&
      session.externalSessionId &&
      session.codexThreadState === "archived"
    ) {
      setRequestError("This Codex thread is archived. Unarchive it before sending another prompt.");
      return false;
    }
    const unknownModelAttempt = resolveUnknownSessionModelSendAttempt(
      confirmedUnknownModelSendsRef.current,
      session,
    );
    confirmedUnknownModelSendsRef.current = unknownModelAttempt.nextConfirmedKeys;
    if (!unknownModelAttempt.allowSend) {
      setRequestError(unknownModelAttempt.warning);
      return false;
    }

    setSendingSessionIds((current) => setSessionFlag(current, sessionId, true));
    setDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === "") {
        return current;
      }

      return {
        ...current,
        [sessionId]: "",
      };
    });
    setDraftAttachmentsBySessionId((current) => {
      if (!current[sessionId]?.length) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    void (async () => {
      try {
        const state = await sendMessage(
          sessionId,
          prompt,
          attachments.map((attachment) => ({
            data: attachment.base64Data,
            fileName: attachment.fileName,
            mediaType: attachment.mediaType,
          })),
          normalizedExpandedText,
        );
        adoptState(state);
        releaseDraftAttachments(attachments);
        setRequestError(null);
      } catch (error) {
        let restoredDraft = false;
        let restoredAttachments = false;

        setDraftsBySessionId((current) => {
          if (!draftText || (current[sessionId] ?? "") !== "") {
            return current;
          }

          restoredDraft = true;
          return {
            ...current,
            [sessionId]: draftText,
          };
        });
        setDraftAttachmentsBySessionId((current) => {
          if (attachments.length === 0 || (current[sessionId]?.length ?? 0) > 0) {
            return current;
          }

          restoredAttachments = true;
          return {
            ...current,
            [sessionId]: attachments,
          };
        });
        if (!restoredAttachments) {
          releaseDraftAttachments(attachments);
        }
        setRequestError(getErrorMessage(error));
      } finally {
        setSendingSessionIds((current) => setSessionFlag(current, sessionId, false));
      }
    })();

    return true;
  }

  function handleDraftAttachmentsAdd(sessionId: string, attachments: DraftImageAttachment[]) {
    setDraftAttachmentsBySessionId((current) => ({
      ...current,
      [sessionId]: [...(current[sessionId] ?? []), ...attachments],
    }));
  }

  function handleDraftAttachmentRemove(sessionId: string, attachmentId: string) {
    setDraftAttachmentsBySessionId((current) => {
      const existing = current[sessionId];
      if (!existing) {
        return current;
      }

      const removed = existing.filter((attachment) => attachment.id === attachmentId);
      if (removed.length === 0) {
        return current;
      }

      releaseDraftAttachments(removed);
      const nextAttachments = existing.filter((attachment) => attachment.id !== attachmentId);
      if (nextAttachments.length === 0) {
        const nextState = { ...current };
        delete nextState[sessionId];
        return nextState;
      }

      return {
        ...current,
        [sessionId]: nextAttachments,
      };
    });
  }

  function openCreateSessionDialog(preferredPaneId: string | null = null) {
    const defaultProjectId =
      selectedProjectId !== ALL_PROJECTS_FILTER_ID && projectLookup.has(selectedProjectId)
        ? selectedProjectId
        : activeSession?.projectId && projectLookup.has(activeSession.projectId)
          ? activeSession.projectId
          : CREATE_SESSION_WORKSPACE_ID;

    setCreateSessionPaneId(preferredPaneId ?? workspace.activePaneId);
    setCreateSessionProjectId(defaultProjectId);
    setRequestError(null);
    setIsCreateSessionOpen(true);
  }

  async function handleNewSession({
    agent,
    model,
    preferredPaneId = null,
    projectSelectionId = CREATE_SESSION_WORKSPACE_ID,
  }: {
    agent: AgentType;
    model: string;
    preferredPaneId?: string | null;
    projectSelectionId?: string;
  }) {
    const trimmedModel = model.trim();
    if (!trimmedModel && !usesSessionModelPicker(agent)) {
      setRequestError("Choose a model.");
      return false;
    }
    if (
      projectSelectionId === CREATE_SESSION_WORKSPACE_ID &&
      !!activeSession?.projectId &&
      !projectLookup.has(activeSession.projectId)
    ) {
      setRequestError(
        "The current workspace project is unavailable. Choose a project before creating a session.",
      );
      return false;
    }
    const workspaceProject =
      projectSelectionId === CREATE_SESSION_WORKSPACE_ID &&
      activeSession?.projectId &&
      projectLookup.has(activeSession.projectId)
        ? (projectLookup.get(activeSession.projectId) ?? null)
        : null;
    const targetProject =
      projectSelectionId !== CREATE_SESSION_WORKSPACE_ID
        ? (projectLookup.get(projectSelectionId) ?? null)
        : workspaceProject && !isLocalRemoteId(resolveProjectRemoteId(workspaceProject))
          ? workspaceProject
          : null;
    const targetUsesRemoteProject =
      !!targetProject && !isLocalRemoteId(resolveProjectRemoteId(targetProject));
    const readiness = targetUsesRemoteProject
      ? null
      : agentReadinessByAgent.get(agent);
    if (readiness?.blocking) {
      setRequestError(readiness.detail);
      return false;
    }

    setIsCreating(true);
    try {
      const targetPaneId = preferredPaneId ?? workspace.activePaneId;
      const targetProjectId =
        projectSelectionId === CREATE_SESSION_WORKSPACE_ID ? null : projectSelectionId;
      const created = await createSession({
        agent,
        model: usesSessionModelPicker(agent) ? undefined : trimmedModel,
        approvalPolicy:
          agent === "Codex" ? defaultCodexApprovalPolicy : undefined,
        reasoningEffort:
          agent === "Codex" ? defaultCodexReasoningEffort : undefined,
        cursorMode: agent === "Cursor" ? defaultCursorMode : undefined,
        claudeApprovalMode:
          agent === "Claude" ? defaultClaudeApprovalMode : undefined,
        claudeEffort:
          agent === "Claude" ? defaultClaudeEffort : undefined,
        geminiApprovalMode:
          agent === "Gemini" ? defaultGeminiApprovalMode : undefined,
        sandboxMode: agent === "Codex" ? defaultCodexSandboxMode : undefined,
        projectId: targetProjectId ?? targetProject?.id ?? undefined,
        workdir: targetProjectId || targetProject ? undefined : activeSession?.workdir,
      });
      if (!isMountedRef.current) {
        return false;
      }
      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: targetPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(openSessionInWorkspaceState(current, created.sessionId, targetPaneId)),
        );
      }
      if (agent === "Claude" || agent === "Codex" || agent === "Cursor" || agent === "Gemini") {
        await handleRefreshSessionModelOptions(created.sessionId);
        if (!isMountedRef.current) {
          return false;
        }
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      setRequestError(getErrorMessage(error));
      return false;
    } finally {
      if (isMountedRef.current) {
        setIsCreating(false);
      }
    }
  }

  async function handleCreateSessionDialogSubmit() {
    const created = await handleNewSession({
      agent: newSessionAgent,
      model: newSessionModel,
      preferredPaneId: createSessionPaneId,
      projectSelectionId: createSessionProjectId,
    });

    if (created && isMountedRef.current) {
      setIsCreateSessionOpen(false);
    }
  }

  function openCreateProjectDialog() {
    setNewProjectRemoteId(LOCAL_REMOTE_ID);
    setRequestError(null);
    setIsCreateProjectOpen(true);
  }

  async function handleCreateProject() {
    const rootPath = newProjectRootPath.trim();
    if (!rootPath) {
      setRequestError("Enter a project root path.");
      return false;
    }

    setIsCreatingProject(true);
    try {
      const created = await createProject({ rootPath, remoteId: newProjectRemoteId });
      adoptState(created.state);
      setSelectedProjectId(created.projectId);
      setNewProjectRootPath("");
      setNewProjectRemoteId(LOCAL_REMOTE_ID);
      setRequestError(null);
      return true;
    } catch (error) {
      setRequestError(getErrorMessage(error));
      return false;
    } finally {
      setIsCreatingProject(false);
    }
  }

  async function handlePickProjectRoot() {
    if (!newProjectUsesLocalRemote) {
      setRequestError("Remote projects need a path from the remote machine. Enter it manually.");
      return;
    }

    setIsCreatingProject(true);
    try {
      const response = await pickProjectRoot();
      if (response.path) {
        setNewProjectRootPath(response.path);
        setRequestError(null);
      }
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setIsCreatingProject(false);
    }
  }

  async function handleApprovalDecision(
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) {
    try {
      const state = await submitApproval(sessionId, messageId, decision);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    }
  }

  async function handleUserInputSubmit(
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) {
    try {
      const state = await submitUserInput(sessionId, messageId, answers);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    }
  }

  async function handleMcpElicitationSubmit(
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) {
    try {
      const state = await submitMcpElicitation(sessionId, messageId, action, content);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    }
  }

  async function handleCodexAppRequestSubmit(
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) {
    try {
      const state = await submitCodexAppRequest(sessionId, messageId, result);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    }
  }

  async function handleCancelQueuedPrompt(sessionId: string, promptId: string) {
    setSessions((current) => {
      const next = removeQueuedPromptFromSessions(current, sessionId, promptId);
      sessionsRef.current = next;
      return next;
    });
    try {
      const state = await cancelQueuedPrompt(sessionId, promptId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      try {
        const state = await fetchState();
        adoptState(state);
      } catch {
        // Keep the original request error below; state refresh is best-effort.
      }
      setRequestError(getErrorMessage(error));
    }
  }

  async function handleStopSession(sessionId: string) {
    setStoppingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await stopSession(sessionId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setStoppingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
  }

  function handleKillSession(sessionId: string, trigger?: HTMLButtonElement | null) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    closePendingSessionRename();
    clearPendingKillCloseTimeout();
    pendingKillTriggerRef.current = trigger ?? null;
    setPendingKillSessionId((current) => (current === sessionId ? null : sessionId));
  }

  async function executeKillSession(sessionId: string) {
    setKillingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await killSession(sessionId);
      if (!isMountedRef.current) {
        return;
      }

      adoptState(state);
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      setRequestError(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setKillingSessionIds((current) => setSessionFlag(current, sessionId, false));
      }
    }
  }

  async function confirmKillSession() {
    if (!pendingKillSessionId) {
      return;
    }

    const sessionId = pendingKillSessionId;
    setPendingKillSessionId(null);
    setKillRevealSessionId(null);

    await executeKillSession(sessionId);
  }

  function focusPendingKillTrigger() {
    window.requestAnimationFrame(() => {
      pendingKillTriggerRef.current?.focus();
    });
  }

  function clearPendingKillCloseTimeout() {
    if (pendingKillCloseTimeoutRef.current === null) {
      return;
    }

    window.clearTimeout(pendingKillCloseTimeoutRef.current);
    pendingKillCloseTimeoutRef.current = null;
  }

  function schedulePendingKillConfirmationClose() {
    clearPendingKillCloseTimeout();

    const sessionId = pendingKillSessionId;
    if (!sessionId) {
      return;
    }

    pendingKillCloseTimeoutRef.current = window.setTimeout(() => {
      pendingKillCloseTimeoutRef.current = null;
      setPendingKillSessionId((current) => (current === sessionId ? null : current));
      setPendingKillPopoverStyle(null);
    }, PENDING_KILL_CLOSE_DELAY_MS);
  }

  function closePendingKillConfirmation(restoreFocus = false) {
    clearPendingKillCloseTimeout();
    setPendingKillSessionId(null);
    setPendingKillPopoverStyle(null);
    if (restoreFocus) {
      focusPendingKillTrigger();
    }
  }

  function clearPendingSessionRenameCloseTimeout() {
    if (pendingSessionRenameCloseTimeoutRef.current === null) {
      return;
    }

    window.clearTimeout(pendingSessionRenameCloseTimeoutRef.current);
    pendingSessionRenameCloseTimeoutRef.current = null;
  }

  function schedulePendingSessionRenameClose() {
    clearPendingSessionRenameCloseTimeout();

    const pendingRename = pendingSessionRename;
    if (!pendingRename) {
      return;
    }
    if (pendingSessionRenameInputRef.current === document.activeElement) {
      return;
    }

    pendingSessionRenameCloseTimeoutRef.current = window.setTimeout(() => {
      pendingSessionRenameCloseTimeoutRef.current = null;
      setPendingSessionRename((current) =>
        current?.sessionId === pendingRename.sessionId ? null : current,
      );
      setPendingSessionRenameDraft("");
      setPendingSessionRenameStyle(null);
    }, PENDING_SESSION_RENAME_CLOSE_DELAY_MS);
  }

  function handleSessionRenameRequest(
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    closePendingKillConfirmation();
    clearPendingSessionRenameCloseTimeout();
    pendingSessionRenameTriggerRef.current = trigger ?? null;
    setPendingSessionRenameDraft(session.name);
    setPendingSessionRename({
      sessionId,
      clientX,
      clientY,
    });
  }

  function focusPendingSessionRenameTrigger() {
    window.requestAnimationFrame(() => {
      pendingSessionRenameTriggerRef.current?.focus();
    });
  }

  function closePendingSessionRename(restoreFocus = false) {
    clearPendingSessionRenameCloseTimeout();
    setPendingSessionRename(null);
    setPendingSessionRenameDraft("");
    setPendingSessionRenameStyle(null);
    if (restoreFocus) {
      focusPendingSessionRenameTrigger();
    }
  }

  async function confirmSessionRename() {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    const nextName = pendingSessionRenameDraft.trim();
    if (!session) {
      closePendingSessionRename();
      return;
    }
    if (!nextName) {
      return;
    }
    if (nextName === session.name.trim()) {
      closePendingSessionRename(true);
      return;
    }

    setUpdatingSessionIds((current) => setSessionFlag(current, session.id, true));
    try {
      const state = await renameSession(session.id, nextName);
      if (!isMountedRef.current) {
        return;
      }

      adoptState(state);
      setRequestError(null);
      closePendingSessionRename();
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      setRequestError(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) => setSessionFlag(current, session.id, false));
      }
    }
  }

  async function handlePendingSessionRenameNew() {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    if (!session) {
      closePendingSessionRename();
      return;
    }

    setIsCreating(true);
    try {
      const targetPaneId = findWorkspacePaneIdForSession(workspace, session.id) ?? workspace.activePaneId;
      const created = await createSession({
        agent: session.agent,
        model: session.model,
        approvalPolicy:
          session.agent === "Codex"
            ? (session.approvalPolicy ?? defaultCodexApprovalPolicy)
            : undefined,
        reasoningEffort:
          session.agent === "Codex"
            ? (session.reasoningEffort ?? defaultCodexReasoningEffort)
            : undefined,
        cursorMode:
          session.agent === "Cursor" ? (session.cursorMode ?? defaultCursorMode) : undefined,
        claudeApprovalMode:
          session.agent === "Claude"
            ? (session.claudeApprovalMode ?? defaultClaudeApprovalMode)
            : undefined,
        claudeEffort:
          session.agent === "Claude"
            ? (session.claudeEffort ?? defaultClaudeEffort)
            : undefined,
        geminiApprovalMode:
          session.agent === "Gemini"
            ? (session.geminiApprovalMode ?? defaultGeminiApprovalMode)
            : undefined,
        sandboxMode:
          session.agent === "Codex"
            ? (session.sandboxMode ?? defaultCodexSandboxMode)
            : undefined,
        projectId: session.projectId && projectLookup.has(session.projectId) ? session.projectId : undefined,
        workdir: session.workdir,
      });
      if (!isMountedRef.current) {
        return;
      }

      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: targetPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(openSessionInWorkspaceState(current, created.sessionId, targetPaneId)),
        );
      }
      if (
        session.agent === "Claude" ||
        session.agent === "Codex" ||
        session.agent === "Cursor" ||
        session.agent === "Gemini"
      ) {
        await handleRefreshSessionModelOptions(created.sessionId);
      }
      setRequestError(null);
      closePendingSessionRename();
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      setRequestError(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setIsCreating(false);
      }
    }
  }

  async function handlePendingSessionRenameKill() {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    if (!session) {
      closePendingSessionRename();
      return;
    }

    closePendingSessionRename();
    setKillRevealSessionId(null);
    await executeKillSession(session.id);
  }

  async function handleSessionSettingsChange(
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }
    const normalizedModelValue =
      field === "model" ? normalizedRequestedSessionModel(session, value as string) : null;
    const payload =
      session.agent === "Codex"
        ? {
            ...(field === "model" ? { model: normalizedModelValue ?? (value as string) } : {}),
            reasoningEffort:
              field === "reasoningEffort"
                ? (value as CodexReasoningEffort)
                : normalizedCodexReasoningEffort(
                    session,
                    field === "model"
                      ? (normalizedModelValue ?? (value as string))
                      : session.model,
                  ),
            sandboxMode:
              field === "sandboxMode"
                ? (value as SandboxMode)
                : (session.sandboxMode ?? "workspace-write"),
            approvalPolicy:
              field === "approvalPolicy"
                ? (value as ApprovalPolicy)
                : (session.approvalPolicy ?? "never"),
          }
        : session.agent === "Cursor"
          ? field === "model"
            ? {
                model: normalizedModelValue ?? (value as string),
              }
            : field === "cursorMode"
              ? {
                  cursorMode: value as CursorMode,
                }
              : null
          : session.agent === "Claude"
            ? field === "model"
              ? {
                  model: normalizedModelValue ?? (value as string),
                }
              : field === "claudeApprovalMode"
                ? {
                    claudeApprovalMode: value as ClaudeApprovalMode,
                  }
                : field === "claudeEffort"
                  ? {
                      claudeEffort: value as ClaudeEffortLevel,
                    }
                  : null
            : session.agent === "Gemini"
              ? field === "model"
                ? {
                    model: normalizedModelValue ?? (value as string),
                  }
                : field === "geminiApprovalMode"
                  ? {
                      geminiApprovalMode: value as GeminiApprovalMode,
                    }
                  : null
              : null;
    if (!payload) {
      return;
    }

    const optimisticSession = buildOptimisticSessionSettingsUpdate(session, field, value);
    const hasOptimisticUpdate = optimisticSession !== session;

    setRequestError(null);
    if (hasOptimisticUpdate) {
      updateSessionLocally(sessionId, () => optimisticSession);
    }
    setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await updateSessionSettings(sessionId, payload);
      adoptState(state);
      const updatedSession =
        state.sessions.find((entry) => entry.id === sessionId) ?? null;
      const nextNotice =
        session.agent === "Codex" && field === "model" && updatedSession
          ? describeCodexModelAdjustmentNotice(session, updatedSession)
          : null;
      setSessionSettingNotices((current) => {
        if (nextNotice) {
          return {
            ...current,
            [sessionId]: nextNotice,
          };
        }
        if (!current[sessionId]) {
          return current;
        }

        const nextState = { ...current };
        delete nextState[sessionId];
        return nextState;
      });
      setRequestError(null);
    } catch (error) {
      if (hasOptimisticUpdate) {
        updateSessionLocally(sessionId, (current) =>
          rollbackOptimisticSessionSettingsUpdate(current, session, optimisticSession),
        );
      }
      setRequestError(getErrorMessage(error));
    } finally {
      setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
  }

  async function handleRefreshSessionModelOptions(sessionId: string) {
    const previousSession = sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;
    if (refreshingSessionModelOptionIdsRef.current[sessionId]) {
      return;
    }
    const nextRefreshingSessionIds = setSessionFlag(
      refreshingSessionModelOptionIdsRef.current,
      sessionId,
      true,
    );
    refreshingSessionModelOptionIdsRef.current = nextRefreshingSessionIds;
    setRefreshingSessionModelOptionIds(nextRefreshingSessionIds);

    setSessionModelOptionErrors((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    try {
      const state = await refreshSessionModelOptions(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      adoptState(state);
      if (previousSession?.agent === "Codex") {
        const refreshedSession = state.sessions.find((entry) => entry.id === sessionId) ?? null;
        const nextNotice = refreshedSession
          ? describeCodexModelAdjustmentNotice(previousSession, refreshedSession)
          : null;
        if (nextNotice) {
          setSessionSettingNotices((current) => ({
            ...current,
            [sessionId]: nextNotice,
          }));
        }
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      const rawMessage = getErrorMessage(error);
      const session = sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;
      const message = session
        ? describeSessionModelRefreshError(
            session.agent,
            rawMessage,
            agentReadinessByAgent.get(session.agent) ?? null,
          )
        : rawMessage;
      setSessionModelOptionErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
      setRequestError(message);
    } finally {
      if (!isMountedRef.current) {
        return;
      }
      const nextRefreshingSessionIds = setSessionFlag(
        refreshingSessionModelOptionIdsRef.current,
        sessionId,
        false,
      );
      refreshingSessionModelOptionIdsRef.current = nextRefreshingSessionIds;
      setRefreshingSessionModelOptionIds(nextRefreshingSessionIds);
    }
  }

  async function runCodexThreadStateAction(
    sessionId: string,
    request: () => Promise<StateResponse>,
    successNotice: string,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await request();
      if (!isMountedRef.current) {
        return;
      }
      adoptState(state);
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: successNotice,
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      setRequestError(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, false));
      }
    }
  }

  async function handleForkCodexThread(sessionId: string, preferredPaneId: string | null) {
    setRequestError(null);
    setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const created = await forkCodexThread(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: preferredPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(
            openSessionInWorkspaceState(current, created.sessionId, preferredPaneId),
          ),
        );
      }
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: "Forked the live Codex thread into a new session.",
        [created.sessionId]:
          "This session is attached to a forked Codex thread. Earlier Codex history was restored from Codex where available.",
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      setRequestError(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, false));
      }
    }
  }

  async function handleArchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => archiveCodexThread(sessionId),
      "Archived the live Codex thread for this session.",
    );
  }

  async function handleUnarchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => unarchiveCodexThread(sessionId),
      "Restored the archived Codex thread for this session.",
    );
  }

  async function handleCompactCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => compactCodexThread(sessionId),
      "Started Codex context compaction for this session.",
    );
  }

  async function handleRollbackCodexThread(sessionId: string, numTurns: number) {
    const turnLabel = numTurns === 1 ? "turn" : "turns";
    await runCodexThreadStateAction(
      sessionId,
      () => rollbackCodexThread(sessionId, numTurns),
      `Rolled the live Codex thread back by ${numTurns} ${turnLabel}.`,
    );
  }

  async function handleRefreshAgentCommands(sessionId: string) {
    if (refreshingAgentCommandSessionIdsRef.current[sessionId]) {
      return;
    }

    const nextRefreshingSessionIds = setSessionFlag(
      refreshingAgentCommandSessionIdsRef.current,
      sessionId,
      true,
    );
    refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
    setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
    setAgentCommandErrors((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    try {
      const response = await fetchAgentCommands(sessionId);
      setAgentCommandsBySessionId((current) => ({
        ...current,
        [sessionId]: response.commands,
      }));
    } catch (error) {
      const message = getErrorMessage(error);
      setAgentCommandErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
    } finally {
      const nextRefreshingSessionIds = setSessionFlag(
        refreshingAgentCommandSessionIdsRef.current,
        sessionId,
        false,
      );
      refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
      setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
    }
  }

  function handleSidebarSessionClick(sessionId: string, preferredPaneId: string | null = null) {
    const session = sessionLookup.get(sessionId);
    closePendingSessionRename();
    setKillRevealSessionId(null);
    setSelectedProjectId(session?.projectId ?? ALL_PROJECTS_FILTER_ID);
    requestScrollToBottom(sessionId);
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSessionInWorkspaceState(current, sessionId, preferredPaneId ?? current.activePaneId),
      ),
    );
  }

  function handleOpenConversationFromDiff(
    sessionId: string,
    preferredPaneId: string | null = null,
  ) {
    handleSidebarSessionClick(sessionId, preferredPaneId);
  }

  function handleInsertReviewIntoPrompt(
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) {
    const nextPrompt = prompt.trim();
    handleOpenConversationFromDiff(sessionId, preferredPaneId);
    if (!nextPrompt) {
      return;
    }

    setDraftsBySessionId((current) => {
      const existingDraft = current[sessionId] ?? "";
      const nextValue =
        existingDraft.trim().length > 0
          ? `${existingDraft.trimEnd()}\n\n${nextPrompt}`
          : nextPrompt;
      if (existingDraft === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function handleScrollToBottomRequestHandled(token: number) {
    setPendingScrollToBottomRequest((current) =>
      current?.token === token ? null : current,
    );
  }

  const pendingSessionRenameSession = pendingSessionRename
    ? (sessionLookup.get(pendingSessionRename.sessionId) ?? null)
    : null;
  const pendingSessionRenameValue = pendingSessionRenameDraft.trim();
  const isPendingSessionRenameSubmitting = pendingSessionRenameSession
    ? Boolean(updatingSessionIds[pendingSessionRenameSession.id])
    : false;
  const isPendingSessionRenameCreating = pendingSessionRenameSession ? isCreating : false;
  const isPendingSessionRenameKilling = pendingSessionRenameSession
    ? Boolean(killingSessionIds[pendingSessionRenameSession.id])
    : false;
  const pendingKillSession = pendingKillSessionId
    ? (sessionLookup.get(pendingKillSessionId) ?? null)
    : null;

  function handlePaneActivate(paneId: string) {
    setWorkspace((current) => activatePane(current, paneId));
  }

  function handlePaneTabSelect(paneId: string, tabId: string) {
    const pane = paneLookup.get(paneId);
    const tab = pane?.tabs.find((candidate) => candidate.id === tabId);
    if (tab?.kind === "session") {
      requestScrollToBottom(tab.sessionId);
    }

    setWorkspace((current) => activatePane(current, paneId, tabId));
  }

  function handleCloseTab(paneId: string, tabId: string) {
    setWorkspace((current) => applyControlPanelLayout(closeWorkspaceTab(current, paneId, tabId)));
  }

  function handleSplitPane(paneId: string, direction: "row" | "column") {
    setWorkspace((current) => applyControlPanelLayout(splitPane(current, paneId, direction)));
  }

  function handleSplitResizeStart(
    splitId: string,
    direction: "row" | "column",
    event: ReactPointerEvent<HTMLDivElement>,
  ) {
    event.preventDefault();
    event.stopPropagation();

    const container = event.currentTarget.parentElement;
    const ratio = getSplitRatio(workspace.root, splitId);
    if (!container || ratio === null) {
      return;
    }

    const rect = container.getBoundingClientRect();
    const { minRatio, maxRatio } = getWorkspaceSplitResizeBounds(
      workspace.root,
      splitId,
      direction,
      direction === "row" ? rect.width : rect.height,
      paneLookup,
    );
    resizeStateRef.current = {
      splitId,
      direction,
      startRatio: ratio,
      minRatio,
      maxRatio,
      startX: event.clientX,
      startY: event.clientY,
      size: direction === "row" ? rect.width : rect.height,
    };
  }

  function handleDraftChange(sessionId: string, nextValue: string) {
    setDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function handleTabDragStart(drag: WorkspaceTabDrag) {
    draggedTabRef.current = drag;
    setDraggedTab(drag);
    broadcastTabDragMessage({
      type: "drag-start",
      payload: drag,
    });
  }

  function handleTabDragEnd() {
    const endedDrag = draggedTabRef.current;
    draggedTabRef.current = null;
    setDraggedTab(null);
    if (!endedDrag) {
      return;
    }

    broadcastTabDragMessage({
      type: "drag-end",
      dragId: endedDrag.dragId,
      sourceWindowId: endedDrag.sourceWindowId,
    });
  }

  function clearStaleTabDragState() {
    const endedDrag = draggedTabRef.current;
    draggedTabRef.current = null;
    setDraggedTab(null);
    setExternalDraggedTab(null);
    if (!endedDrag) {
      return;
    }

    broadcastTabDragMessage({
      type: "drag-end",
      dragId: endedDrag.dragId,
      sourceWindowId: endedDrag.sourceWindowId,
    });
  }

  function recoverFromLostTabDrag(buttons: number) {
    if (buttons !== 0) {
      return false;
    }

    if (!draggedTabRef.current && !externalDraggedTab) {
      return false;
    }

    clearStaleTabDragState();
    return true;
  }

  function handleTabDrop(targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) {
    if (draggedTab) {
      const drop = draggedTab;
      draggedTabRef.current = null;
      setDraggedTab(null);
      const nextControlPanelSide =
        drop.tab.kind === "controlPanel" && (placement === "left" || placement === "right")
          ? placement
          : controlPanelSide;
      if (nextControlPanelSide !== controlPanelSide) {
        setControlPanelSide(nextControlPanelSide);
      }
      startTransition(() => {
        setWorkspace((current) =>
          applyControlPanelLayout(
            placeDraggedTab(
              current,
              drop.sourcePaneId,
              drop.tabId,
              targetPaneId,
              placement,
              tabIndex,
            ),
            nextControlPanelSide,
          ),
        );
      });
      return;
    }

    if (!externalDraggedTab) {
      return;
    }

    const drop = externalDraggedTab;
    setExternalDraggedTab((current) => (current?.dragId === drop.dragId ? null : current));
    const nextControlPanelSide =
      drop.tab.kind === "controlPanel" && (placement === "left" || placement === "right")
        ? placement
        : controlPanelSide;
    if (nextControlPanelSide !== controlPanelSide) {
      setControlPanelSide(nextControlPanelSide);
    }
    // Only ask the source window to remove its tab after this window has applied the drop.
    flushSync(() => {
      setWorkspace((current) =>
        applyControlPanelLayout(
          placeExternalTab(current, drop.tab, targetPaneId, placement, tabIndex),
          nextControlPanelSide,
        ),
      );
    });
    broadcastTabDragMessage({
      type: "drop-commit",
      dragId: drop.dragId,
      sourceWindowId: drop.sourceWindowId,
      sourcePaneId: drop.sourcePaneId,
      tabId: drop.tabId,
      targetWindowId: windowId,
    });
    broadcastTabDragMessage({
      type: "drag-end",
      dragId: drop.dragId,
      sourceWindowId: drop.sourceWindowId,
    });
  }

  useEffect(() => {
    if (!draggedTab && !externalDraggedTab) {
      return;
    }

    const handleWindowBlur = () => {
      clearStaleTabDragState();
    };
    const handlePageHide = () => {
      clearStaleTabDragState();
    };
    const handleVisibilityChange = () => {
      if (document.visibilityState === "hidden") {
        clearStaleTabDragState();
      }
    };
    const timeoutId = window.setTimeout(() => {
      clearStaleTabDragState();
    }, TAB_DRAG_STALE_TIMEOUT_MS);

    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("pagehide", handlePageHide);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      window.clearTimeout(timeoutId);
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("pagehide", handlePageHide);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [draggedTab, externalDraggedTab]);

  function handlePaneViewModeChange(paneId: string, viewMode: SessionPaneViewMode) {
    if (viewMode === "session") {
      const pane = paneLookup.get(paneId);
      const activeTab = pane?.tabs.find((candidate) => candidate.id === pane.activeTabId);
      if (activeTab?.kind === "session") {
        requestScrollToBottom(activeTab.sessionId);
      }
    }

    setWorkspace((current) => setPaneViewMode(current, paneId, viewMode));
  }

  function requestScrollToBottom(sessionId: string) {
    setPendingScrollToBottomRequest({
      sessionId,
      token: Date.now() + Math.random(),
    });
  }

  function handlePaneSourcePathChange(paneId: string, path: string) {
    setWorkspace((current) => setPaneSourcePath(current, paneId, path));
  }

  function handleOpenSourceTab(
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: {
      line?: number;
      column?: number;
      openInNewTab?: boolean;
    },
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSourceInWorkspaceState(current, path, paneId, originSessionId, originProjectId, {
          line: options?.line,
          column: options?.column,
          openInNewTab: options?.openInNewTab,
        }),
      ),
    );
  }

  function handleOpenDiffPreviewTab(
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openDiffPreviewInWorkspaceState(
          current,
          {
            changeType: message.changeType,
            changeSetId: message.changeSetId ?? `change-${message.id}`,
            diff: message.diff,
            diffMessageId: message.id,
            filePath: message.filePath,
            language: message.language ?? null,
            originSessionId,
            originProjectId,
            summary: message.summary,
          },
          paneId,
        ),
      ),
    );
  }

  function handleOpenGitStatusDiffPreviewTab(
    paneId: string,
    diffPreview: GitDiffResponse,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: {
      openInNewTab?: boolean;
      sectionId?: GitDiffSection;
    },
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openDiffPreviewInWorkspaceState(
          current,
          {
            changeType: diffPreview.changeType,
            changeSetId: diffPreview.changeSetId ?? null,
            diff: diffPreview.diff,
            diffMessageId: diffPreview.diffId,
            filePath: diffPreview.filePath ?? null,
            gitSectionId: options?.sectionId ?? null,
            language: diffPreview.language ?? null,
            originSessionId,
            originProjectId,
            summary: diffPreview.summary,
          },
          paneId,
          options?.openInNewTab
            ? {
                openInNewTab: true,
              }
            : {
                reuseActiveViewerTab: true,
              },
        ),
      ),
    );
  }

  function handleOpenFilesystemTab(
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openFilesystemInWorkspaceState(current, rootPath, paneId, originSessionId, originProjectId),
      ),
    );
  }

  function handleOpenGitStatusTab(
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openGitStatusInWorkspaceState(current, workdir, paneId, originSessionId, originProjectId),
      ),
    );
  }

  function handleOpenInstructionDebuggerTab(
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openInstructionDebuggerInWorkspaceState(
          current,
          workdir,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function renderWorkspaceControlSurface(paneId: string): JSX.Element {
    const surfaceId = paneId;
    const controlPanelProjectFilterId = `control-panel-project-scope-${surfaceId}`;

    function renderControlPanelProjectScope() {
      return (
        <div className="control-panel-scope-control">
          <label className="control-panel-scope-label" htmlFor={controlPanelProjectFilterId}>
            Project
          </label>
          <ThemedCombobox
            id={controlPanelProjectFilterId}
            className="control-panel-scope-combobox"
            value={selectedProjectId}
            options={controlPanelProjectOptions}
            onChange={setSelectedProjectId}
            aria-label="Project"
          />
        </div>
      );
    }

    function renderControlPanelHeaderActions(sectionId: ControlPanelSectionId) {
      switch (sectionId) {
        case "files":
          return (
            <button
              className="control-panel-header-action control-panel-header-open-button"
              type="button"
              onClick={() => handleOpenFilesystemTab(paneId, controlPanelFilesystemRoot, controlPanelSessionId, selectedProject?.id ?? null)}
              disabled={!(controlPanelFilesystemRoot?.trim() ?? "")}
            >
              <span className="control-panel-header-action-icon" aria-hidden="true">
                <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
                  <path
                    d="M3.5 4.25h4l1.15 1.25h4A1.25 1.25 0 0 1 13.9 6.75v5.5a1.25 1.25 0 0 1-1.25 1.25H3.5A1.25 1.25 0 0 1 2.25 12.25v-6.75A1.25 1.25 0 0 1 3.5 4.25Z"
                    fill="none"
                    stroke="currentColor"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth="1.35"
                  />
                  <path
                    d="M8.75 3.25v4.5M6.5 5.5h4.5"
                    fill="none"
                    stroke="currentColor"
                    strokeLinecap="round"
                    strokeWidth="1.35"
                  />
                </svg>
              </span>
              <span>Open tab</span>
            </button>
          );

        case "sessions":
          return (
            <button
              className="control-panel-header-action control-panel-header-new-session-button"
              type="button"
              onClick={() => openCreateSessionDialog(paneId)}
              aria-haspopup="dialog"
              aria-expanded={isCreateSessionOpen}
              aria-controls="create-session-dialog"
              disabled={isCreating}
            >
              <span className="control-panel-header-action-icon control-panel-header-action-icon-play" aria-hidden="true">
                <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
                  <path d="M5 3.5 12.25 8 5 12.5Z" fill="currentColor" />
                </svg>
              </span>
              <span>{isCreating ? "Creating" : "New"}</span>
            </button>
          );

        default:
          return null;
      }
    }

    function renderControlPanelSection(sectionId: ControlPanelSectionId) {
      switch (sectionId) {
        case "files":
          return (
            <section className="control-panel-section-stack control-panel-section-files" aria-label="Files">
              {renderControlPanelProjectScope()}
              <FileSystemPanel
                rootPath={controlPanelFilesystemRoot}
                sessionId={controlPanelSessionId}
                projectId={selectedProject?.id ?? null}
                showPathControls={false}
                onOpenPath={(path, options) =>
                  handleOpenSourceTab(paneId, path, controlPanelSessionId, selectedProject?.id ?? null, options)
                }
                onOpenRootPath={(path) => setControlPanelFilesystemRoot(path.trim() || null)}
              />
            </section>
          );

        case "git":
          return (
            <section className="control-panel-section-stack control-panel-section-git" aria-label="Git status">
              {renderControlPanelProjectScope()}
              <GitStatusPanel
                sessionId={controlPanelSessionId}
                workdir={controlPanelGitWorkdir}
                showPathControls={false}
                onStatusChange={(status) => setControlPanelGitStatusCount(status?.files.length ?? 0)}
                onOpenDiff={(diff, options) =>
                  handleOpenGitStatusDiffPreviewTab(paneId, diff, controlPanelSessionId, selectedProject?.id ?? null, options)
                }
                onOpenWorkdir={(path) => setControlPanelGitWorkdir(path.trim() || null)}
              />
            </section>
          );

        case "projects":
          return (
            <section className="control-panel-section-stack" aria-label="Projects">
              <section className="project-controls" aria-label="Projects">
                <div className="project-controls-header">
                  <div className="session-control-label">Projects</div>
                  <span className="project-count-badge">{projects.length}</span>
                </div>
                <button
                  className="ghost-button project-create-launch"
                  type="button"
                  onClick={openCreateProjectDialog}
                  aria-haspopup="dialog"
                  aria-expanded={isCreateProjectOpen}
                  aria-controls="create-project-dialog"
                  disabled={isCreatingProject}
                >
                  {isCreatingProject ? "Adding..." : "Add project"}
                </button>
                <div className="project-list" role="list">
                  <button
                    className={`project-row ${selectedProjectId === ALL_PROJECTS_FILTER_ID ? "selected" : ""}`}
                    type="button"
                    onClick={() => setSelectedProjectId(ALL_PROJECTS_FILTER_ID)}
                  >
                    <span className="project-row-copy">
                      <strong>All projects</strong>
                      <span className="project-row-path">Show every session in this window.</span>
                    </span>
                    <span className="project-row-count">{sessions.length}</span>
                  </button>
                  {projects.map((project) => {
                    const isSelected = project.id === selectedProjectId;

                    return (
                      <button
                        key={project.id}
                        className={`project-row ${isSelected ? "selected" : ""}`}
                        type="button"
                        onClick={() => setSelectedProjectId(project.id)}
                      >
                        <span className="project-row-copy">
                          <strong>{project.name}</strong>
                          <span className="project-row-path">{describeProjectScope(project, remoteLookup)}</span>
                        </span>
                        <span className="project-row-count">{projectSessionCounts.get(project.id) ?? 0}</span>
                      </button>
                    );
                  })}
                </div>
              </section>
            </section>
          );

        case "sessions":
        default:
          return (
            <section className="control-panel-section-stack control-panel-section-sessions" aria-label="Sessions">
              <section className="session-list-shell" aria-label="Sessions">
                <div className="session-list-tools">
                  {renderControlPanelProjectScope()}
                  <input
                    ref={sessionListSearchInputRef}
                    className="themed-input session-list-search-input"
                    type="search"
                    value={sessionListSearchQuery}
                    placeholder="Search sessions"
                    spellCheck={false}
                    aria-label="Search sessions"
                    title={`Search across visible sessions (${primaryModifierLabel()}+Shift+F)`}
                    onChange={(event) => setSessionListSearchQuery(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.preventDefault();
                        if (sessionListSearchQuery) {
                          setSessionListSearchQuery("");
                        } else {
                          event.currentTarget.blur();
                        }
                      }
                    }}
                  />
                  {hasSessionListSearch ? (
                    <div className="session-list-search-meta" aria-live="polite">
                      {filteredSessions.length === 1
                        ? "1 matching session"
                        : `${filteredSessions.length} matching sessions`}
                    </div>
                  ) : null}
                </div>
                <div className="session-list">
                  {filteredSessions.length > 0 ? (
                    filteredSessions.map((session) => {
                      const isActive = session.id === activeSession?.id;
                      const isOpen = openSessionIds.has(session.id);
                      const isKilling = Boolean(killingSessionIds[session.id]);
                      const isKillConfirmationOpen = pendingKillSessionId === session.id;
                      const isKillVisible =
                        isKilling || isKillConfirmationOpen || killRevealSessionId === session.id;
                      const searchResult = sessionListSearchResults.get(session.id);

                      return (
                        <div
                          key={`${surfaceId}-${session.id}`}
                          className={`session-row-shell ${isActive ? "selected" : ""} ${isOpen ? "open" : ""} ${isKillVisible ? "kill-armed" : ""}`}
                          onMouseLeave={() => {
                            if (!isKilling && !isKillConfirmationOpen) {
                              setKillRevealSessionId((current) => (current === session.id ? null : current));
                            }
                          }}
                          onBlur={(event) => {
                            const nextTarget = event.relatedTarget;
                            if (
                              !isKilling &&
                              !isKillConfirmationOpen &&
                              (!(nextTarget instanceof Node) || !event.currentTarget.contains(nextTarget))
                            ) {
                              setKillRevealSessionId((current) => (current === session.id ? null : current));
                            }
                          }}
                        >
                          <button
                            className={`session-row ${isActive ? "selected" : ""} ${isOpen ? "open" : ""}`}
                            type="button"
                            onClick={() => handleSidebarSessionClick(session.id, paneId)}
                            title={`${session.agent} / ${session.workdir}`}
                            onContextMenu={(event) => {
                              event.preventDefault();
                              handleSessionRenameRequest(
                                session.id,
                                event.clientX,
                                event.clientY,
                                event.currentTarget,
                              );
                            }}
                          >
                            <div className="session-copy">
                              <div className="session-title-line">
                                <strong>{session.name}</strong>
                                {searchResult ? (
                                  <span className="session-search-count">
                                    {searchResult.matchCount} hit{searchResult.matchCount === 1 ? "" : "s"}
                                  </span>
                                ) : null}
                              </div>
                              <div
                                className={`session-preview${searchResult ? " session-preview-search-result" : ""}`}
                                title={searchResult?.snippet ?? session.preview}
                              >
                                {searchResult?.snippet ?? session.preview}
                              </div>
                            </div>
                          </button>
                          <button
                            className="session-row-status-button"
                            type="button"
                            onClick={() =>
                              setKillRevealSessionId((current) =>
                                current === session.id && !isKilling ? null : session.id,
                              )
                            }
                            aria-label={`Show session actions for ${session.name}`}
                          >
                            <span className="status-agent-badge session-row-status-badge" data-status={session.status}>
                              <AgentIcon agent={session.agent} className="session-row-status-icon" />
                            </span>
                          </button>
                          <button
                            className="ghost-button session-row-kill"
                            type="button"
                            onClick={(event) => {
                              handleKillSession(session.id, event.currentTarget);
                            }}
                            disabled={isKilling}
                            aria-expanded={isKillConfirmationOpen}
                            aria-controls={
                              isKillConfirmationOpen ? `kill-session-popover-${session.id}` : undefined
                            }
                            aria-label={`Kill ${session.name}`}
                          >
                            {isKilling ? "Killing" : "Kill"}
                          </button>
                        </div>
                      );
                    })
                  ) : (
                    <div className="session-filter-empty">
                      {sessions.length === 0
                        ? "No sessions yet."
                        : hasSessionListSearch
                          ? selectedProject
                            ? `No sessions match this search in ${selectedProject.name}.`
                            : "No sessions match this search."
                          : selectedProject
                            ? `No ${sessionListFilter === "all" ? "" : `${sessionListFilter} `}sessions in ${selectedProject.name}.`
                            : "No sessions match this filter."}
                    </div>
                  )}
                </div>
              </section>

              <section className="sidebar-status" aria-label="Session filters">
                <div className="session-control-label">Status</div>
                <div className="sidebar-status-chips">
                  <button
                    className={`chip sidebar-status-chip ${sessionListFilter === "all" ? "selected" : ""}`}
                    type="button"
                    onClick={() => setSessionListFilter("all")}
                    aria-pressed={sessionListFilter === "all"}
                  >
                    No filter ({sessionFilterCounts.all})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${sessionListFilter === "working" ? "selected" : ""}`}
                    type="button"
                    onClick={() => setSessionListFilter("working")}
                    aria-pressed={sessionListFilter === "working"}
                  >
                    Working ({sessionFilterCounts.working})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${sessionListFilter === "asking" ? "selected" : ""}`}
                    type="button"
                    onClick={() => setSessionListFilter("asking")}
                    aria-pressed={sessionListFilter === "asking"}
                  >
                    Asking ({sessionFilterCounts.asking})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${sessionListFilter === "completed" ? "selected" : ""}`}
                    type="button"
                    onClick={() => setSessionListFilter("completed")}
                    aria-pressed={sessionListFilter === "completed"}
                  >
                    Completed ({sessionFilterCounts.completed})
                  </button>
                </div>
              </section>
            </section>
          );
      }
    }

    return (
      <div className="sidebar sidebar-panel">
        <ControlPanelSurface
          ref={controlPanelSurfaceRef}
          gitStatusCount={controlPanelGitStatusCount}
          isPreferencesOpen={isSettingsOpen}
          onOpenPreferences={() => setIsSettingsOpen(true)}
          projectCount={projects.length}
          sessionCount={projectScopedSessions.length}
          renderHeaderActions={renderControlPanelHeaderActions}
          renderSection={renderControlPanelSection}
        />
      </div>
    );
  }

  return (
    <div className="shell">
      <div className="background-orbit background-orbit-left" />
      <div className="background-orbit background-orbit-right" />

      <main className="workspace-shell">

        {requestError ? (
          <article className="thread-notice workspace-notice">
            <div className="card-label">Backend</div>
            <p>{requestError}</p>
          </article>
        ) : null}

        <section
          className={`workspace-stage${
            workspaceHasOnlyControlPanel
              ? ` workspace-stage-control-panel-only workspace-stage-control-panel-only-${controlPanelSide}`
              : ""
          }`}
        >
          {workspace.root ? (
            <WorkspaceNodeView
              node={workspace.root}
              codexState={codexState}
              projectLookup={projectLookup}
              paneLookup={paneLookup}
              sessionLookup={sessionLookup}
              activePaneId={workspace.activePaneId}
              isLoading={isLoading}
              draftsBySessionId={draftsBySessionId}
              draftAttachmentsBySessionId={draftAttachmentsBySessionId}
              sendingSessionIds={sendingSessionIds}
              stoppingSessionIds={stoppingSessionIds}
              killingSessionIds={killingSessionIds}
              updatingSessionIds={updatingSessionIds}
              refreshingSessionModelOptionIds={refreshingSessionModelOptionIds}
              sessionModelOptionErrors={sessionModelOptionErrors}
              agentCommandsBySessionId={agentCommandsBySessionId}
              refreshingAgentCommandSessionIds={refreshingAgentCommandSessionIds}
              agentCommandErrors={agentCommandErrors}
              sessionSettingNotices={sessionSettingNotices}
              paneShouldStickToBottomRef={paneShouldStickToBottomRef}
              paneScrollPositionsRef={paneScrollPositionsRef}
              paneContentSignaturesRef={paneContentSignaturesRef}
              pendingScrollToBottomRequest={pendingScrollToBottomRequest}
              windowId={windowId}
              draggedTab={activeDraggedTab}
              editorAppearance={editorAppearance}
              editorFontSizePx={editorFontSizePx}
              onActivatePane={handlePaneActivate}
              onSelectTab={handlePaneTabSelect}
              onCloseTab={handleCloseTab}
              onSplitPane={handleSplitPane}
              onResizeStart={handleSplitResizeStart}
              onTabDragStart={handleTabDragStart}
              onTabDragEnd={handleTabDragEnd}
              onTabDrop={handleTabDrop}
              onPaneViewModeChange={handlePaneViewModeChange}
              onOpenSourceTab={handleOpenSourceTab}
              onOpenDiffPreviewTab={handleOpenDiffPreviewTab}
              onOpenGitStatusDiffPreviewTab={handleOpenGitStatusDiffPreviewTab}
              onOpenFilesystemTab={handleOpenFilesystemTab}
              onOpenGitStatusTab={handleOpenGitStatusTab}
              onOpenInstructionDebuggerTab={handleOpenInstructionDebuggerTab}
              onPaneSourcePathChange={handlePaneSourcePathChange}
              onOpenConversationFromDiff={handleOpenConversationFromDiff}
              onInsertReviewIntoPrompt={handleInsertReviewIntoPrompt}
              onDraftCommit={handleDraftChange}
              onDraftAttachmentsAdd={handleDraftAttachmentsAdd}
              onDraftAttachmentRemove={handleDraftAttachmentRemove}
              onComposerError={setRequestError}
              onSend={handleSend}
              onCancelQueuedPrompt={handleCancelQueuedPrompt}
              onApprovalDecision={handleApprovalDecision}
              onUserInputSubmit={handleUserInputSubmit}
              onMcpElicitationSubmit={handleMcpElicitationSubmit}
              onCodexAppRequestSubmit={handleCodexAppRequestSubmit}
              onStopSession={handleStopSession}
              onKillSession={handleKillSession}
              onRenameSessionRequest={handleSessionRenameRequest}
              onScrollToBottomRequestHandled={handleScrollToBottomRequestHandled}
              onSessionSettingsChange={handleSessionSettingsChange}
              onArchiveCodexThread={handleArchiveCodexThread}
              onCompactCodexThread={handleCompactCodexThread}
              onForkCodexThread={handleForkCodexThread}
              onRefreshSessionModelOptions={handleRefreshSessionModelOptions}
              onRefreshAgentCommands={handleRefreshAgentCommands}
              onRollbackCodexThread={handleRollbackCodexThread}
              onUnarchiveCodexThread={handleUnarchiveCodexThread}
              renderControlPanel={(paneId) => renderWorkspaceControlSurface(paneId)}
              backendConnectionState={backendConnectionState}
            />
          ) : (
            <div className="workspace-empty panel">
              <EmptyState
                title={isLoading ? "Connecting to backend" : "No sessions in the workspace"}
                body={
                  isLoading
                    ? "Fetching session state from the Rust backend."
                    : "Select a session from the left rail or create a new one to start tiling."
                }
              />
            </div>
          )}
        </section>
      </main>
      {pendingKillSession && typeof document !== "undefined"
        ? createPortal(
            <>
              <div
                className="session-kill-popover-backdrop"
                onPointerMove={() => {
                  schedulePendingKillConfirmationClose();
                }}
                onPointerDown={(event) => {
                  event.preventDefault();
                  closePendingKillConfirmation();
                }}
              />
              <div
                ref={pendingKillPopoverRef}
                id={`kill-session-popover-${pendingKillSession.id}`}
                className="session-kill-popover panel"
                style={
                  pendingKillPopoverStyle ?? {
                    left: 0,
                    top: 0,
                    visibility: "hidden",
                  }
                }
                role="dialog"
                aria-label={`Confirm killing ${pendingKillSession.name}`}
                onPointerEnter={() => {
                  clearPendingKillCloseTimeout();
                }}
                onPointerLeave={() => {
                  schedulePendingKillConfirmationClose();
                }}
              >
                <div className="session-kill-popover-actions">
                  <button
                    className="ghost-button session-kill-popover-cancel"
                    type="button"
                    onClick={() => {
                      closePendingKillConfirmation(true);
                    }}
                  >
                    Cancel
                  </button>
                  <button
                    ref={pendingKillConfirmButtonRef}
                    className="send-button session-kill-popover-confirm"
                    type="button"
                    onClick={() => void confirmKillSession()}
                  >
                    Kill
                  </button>
                </div>
              </div>
            </>,
            document.body,
          )
        : null}
      {pendingSessionRenameSession && typeof document !== "undefined"
        ? createPortal(
            <>
              <div
                className="session-rename-popover-backdrop"
                onPointerMove={() => {
                  schedulePendingSessionRenameClose();
                }}
                onPointerDown={(event) => {
                  event.preventDefault();
                  closePendingSessionRename();
                }}
              />
              <form
                ref={pendingSessionRenamePopoverRef}
                className="session-rename-popover panel"
                style={
                  pendingSessionRenameStyle ?? {
                    left: 0,
                    top: 0,
                    visibility: "hidden",
                  }
                }
                onSubmit={(event) => {
                  event.preventDefault();
                  void confirmSessionRename();
                }}
                onPointerEnter={() => {
                  clearPendingSessionRenameCloseTimeout();
                }}
                onPointerLeave={() => {
                  schedulePendingSessionRenameClose();
                }}
              >
                <input
                  ref={pendingSessionRenameInputRef}
                  className="themed-input session-rename-input"
                  type="text"
                  value={pendingSessionRenameDraft}
                  maxLength={120}
                  spellCheck={false}
                  aria-label="Session name"
                  placeholder="Session name"
                  onFocus={() => {
                    clearPendingSessionRenameCloseTimeout();
                  }}
                  onChange={(event) => {
                    clearPendingSessionRenameCloseTimeout();
                    setPendingSessionRenameDraft(event.currentTarget.value);
                  }}
                />
                <div className="session-rename-actions">
                  <button
                    className="ghost-button session-rename-new"
                    type="button"
                    onClick={() => {
                      void handlePendingSessionRenameNew();
                    }}
                    disabled={
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    {isPendingSessionRenameCreating ? "Creating" : "New"}
                  </button>
                  <button
                    className="ghost-button session-rename-kill"
                    type="button"
                    onClick={() => {
                      void handlePendingSessionRenameKill();
                    }}
                    disabled={
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    {isPendingSessionRenameKilling ? "Killing" : "Kill"}
                  </button>
                  <button
                    className="ghost-button session-rename-cancel"
                    type="button"
                    onClick={() => {
                      closePendingSessionRename(true);
                    }}
                    disabled={
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    Cancel
                  </button>
                  <button
                    className="send-button session-rename-save"
                    type="submit"
                    disabled={
                      !pendingSessionRenameValue ||
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    {isPendingSessionRenameSubmitting ? "Saving" : "Save"}
                  </button>
                </div>
              </form>
            </>,
            document.body,
          )
        : null}
      {isCreateSessionOpen ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            if (!isCreating) {
              setRequestError(null);
              setIsCreateSessionOpen(false);
            }
          }}
        >
          <section
            id="create-session-dialog"
            className="dialog-card panel create-session-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-session-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="create-session-dialog-header">
              <div>
                <div className="card-label">Session</div>
                <h2 id="create-session-dialog-title">New session</h2>
                <p className="dialog-copy">
                  Pick the assistant, project, and any startup settings before opening the
                  session. Session-specific controls stay with the session after it starts.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={() => {
                  setRequestError(null);
                  setIsCreateSessionOpen(false);
                }}
                disabled={isCreating}
              >
                <DialogCloseIcon />
              </button>
            </div>

            <form
              className="create-session-dialog-body"
              onSubmit={(event) => {
                event.preventDefault();
                void handleCreateSessionDialogSubmit();
              }}
            >
              {requestError ? (
                <article className="thread-notice create-session-dialog-error">
                  <div className="card-label">Backend</div>
                  <p>{requestError}</p>
                </article>
              ) : null}

              <div className="create-session-field">
                <label className="session-control-label" htmlFor="create-session-agent">
                  Assistant
                </label>
                <ThemedCombobox
                  id="create-session-agent"
                  value={newSessionAgent}
                  options={NEW_SESSION_AGENT_OPTIONS as readonly ComboboxOption[]}
                  onChange={(nextValue) => setNewSessionAgent(nextValue as AgentType)}
                  disabled={isCreating}
                />
              </div>

              {createSessionUsesSessionModelPicker ? (
                <div className="create-session-field">
                  <label className="session-control-label">Model</label>
                  <p className="create-session-field-hint">
                    {createSessionModelHint(newSessionAgent)}
                  </p>
                </div>
              ) : (
                <div className="create-session-field">
                  <label className="session-control-label" htmlFor="create-session-model">
                    Model
                  </label>
                  <ThemedCombobox
                    id="create-session-model"
                    value={newSessionModel}
                    options={newSessionModelOptions}
                    onChange={(nextValue) =>
                      setNewSessionModelByAgent((current) => ({
                        ...current,
                        [newSessionAgent]: nextValue,
                      }))
                    }
                    disabled={isCreating}
                  />
                </div>
              )}

              {newSessionAgent === "Codex" ? (
                <div className="create-session-field">
                  <label className="session-control-label" htmlFor="create-session-codex-reasoning-effort">
                    Codex reasoning effort
                  </label>
                  <ThemedCombobox
                    id="create-session-codex-reasoning-effort"
                    value={defaultCodexReasoningEffort}
                    options={CODEX_REASONING_EFFORT_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      handleDefaultCodexReasoningEffortChange(nextValue as CodexReasoningEffort)
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    New Codex sessions start with this reasoning effort, and you can still change it per session later.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Claude" ? (
                <div className="create-session-field">
                  <label className="session-control-label" htmlFor="create-session-claude-effort">
                    Claude effort
                  </label>
                  <ThemedCombobox
                    id="create-session-claude-effort"
                    value={defaultClaudeEffort}
                    options={CLAUDE_EFFORT_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) => handleDefaultClaudeEffortChange(nextValue as ClaudeEffortLevel)}
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    New Claude sessions start with this effort, and you can still change it per session later.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Cursor" ? (
                <div className="create-session-field">
                  <label className="session-control-label" htmlFor="create-session-cursor-mode">
                    Cursor mode
                  </label>
                  <ThemedCombobox
                    id="create-session-cursor-mode"
                    value={defaultCursorMode}
                    options={CURSOR_MODE_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) => setDefaultCursorMode(nextValue as CursorMode)}
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    Agent auto-approves tool requests and can edit, Ask keeps approval cards, and
                    Plan stays read-only.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Gemini" ? (
                <div className="create-session-field">
                  <label className="session-control-label" htmlFor="create-session-gemini-mode">
                    Gemini approvals
                  </label>
                  <ThemedCombobox
                    id="create-session-gemini-mode"
                    value={defaultGeminiApprovalMode}
                    options={GEMINI_APPROVAL_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      setDefaultGeminiApprovalMode(nextValue as GeminiApprovalMode)
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    Default prompts for approval, Auto edit approves edit tools, YOLO approves all
                    tools, and Plan keeps Gemini read-only.
                  </p>
                </div>
              ) : null}

              <div className="create-session-field">
                <label className="session-control-label" htmlFor="create-session-project">
                  Project
                </label>
                <ThemedCombobox
                  id="create-session-project"
                  value={createSessionProjectId}
                  options={createSessionProjectOptions}
                  onChange={setCreateSessionProjectId}
                  disabled={isCreating}
                />
                <p className="create-session-field-hint">{createSessionProjectHint}</p>
              </div>

              {createSessionProjectSelectionError ? (
                <article className="thread-notice create-session-readiness">
                  <div className="card-label">Remote</div>
                  <p>{createSessionProjectSelectionError}</p>
                </article>
              ) : null}

              {createSessionAgentReadiness ? (
                <article className="thread-notice create-session-readiness">
                  <div className="card-label">
                    {createSessionAgentReadiness.blocking ? "Setup Required" : "Ready"}
                  </div>
                  <p>{createSessionAgentReadiness.detail}</p>
                  {createSessionAgentReadiness.commandPath ? (
                    <p className="create-session-field-hint">
                      Binary: {createSessionAgentReadiness.commandPath}
                    </p>
                  ) : null}
                </article>
              ) : null}

              <div className="dialog-actions create-session-dialog-actions">
                <button
                  className="ghost-button"
                  type="button"
                  onClick={() => {
                    setRequestError(null);
                    setIsCreateSessionOpen(false);
                  }}
                  disabled={isCreating}
                >
                  Cancel
                </button>
                <button
                  className="send-button create-session-submit"
                  type="submit"
                  disabled={isCreating || createSessionBlocked || !!createSessionProjectSelectionError}
                >
                  {isCreating ? "Creating..." : "Create session"}
                </button>
              </div>
            </form>
          </section>
        </div>
      ) : null}
      {isCreateProjectOpen ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            if (!isCreatingProject) {
              setRequestError(null);
              setIsCreateProjectOpen(false);
            }
          }}
        >
          <section
            id="create-project-dialog"
            className="dialog-card panel create-project-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-project-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="create-project-dialog-header">
              <div>
                <div className="card-label">Project</div>
                <h2 id="create-project-dialog-title">Add project</h2>
                <p className="dialog-copy">
                  Choose a local folder or enter a remote root path to add a scoped project.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={() => {
                  setRequestError(null);
                  setIsCreateProjectOpen(false);
                }}
                disabled={isCreatingProject}
              >
                <DialogCloseIcon />
              </button>
            </div>

            <form
              className="create-project-dialog-body"
              onSubmit={(event) => {
                event.preventDefault();
                void (async () => {
                  const created = await handleCreateProject();
                  if (created) {
                    setIsCreateProjectOpen(false);
                  }
                })();
              }}
            >
              {requestError ? (
                <article className="thread-notice create-project-dialog-error">
                  <div className="card-label">Backend</div>
                  <p>{requestError}</p>
                </article>
              ) : null}

              <div className="create-project-field">
                <label className="session-control-label" htmlFor="create-project-remote">
                  Remote
                </label>
                <ThemedCombobox
                  id="create-project-remote"
                  value={newProjectRemoteId}
                  options={createProjectRemoteOptions}
                  onChange={setNewProjectRemoteId}
                  disabled={isCreatingProject}
                />
                <p className="create-session-field-hint">
                  {remoteDisplayName(newProjectSelectedRemote, newProjectRemoteId)} - {remoteConnectionLabel(newProjectSelectedRemote)}
                </p>
              </div>

              <div className="create-project-field">
                <label className="session-control-label" htmlFor="create-project-root">
                  {newProjectUsesLocalRemote ? "Folder" : "Remote root path"}
                </label>
                <input
                  id="create-project-root"
                  className="themed-input project-root-input"
                  type="text"
                  value={newProjectRootPath}
                  placeholder={newProjectUsesLocalRemote ? "/path/to/project" : "/remote/path/to/project"}
                  onChange={(event) => setNewProjectRootPath(event.target.value)}
                  disabled={isCreatingProject}
                />
                <p className="create-session-field-hint">
                  {newProjectUsesLocalRemote
                    ? "Local projects use the folder picker and local filesystem panels immediately."
                    : "Remote projects store the remote path and route files and sessions through the local SSH proxy."}
                </p>
              </div>

              <div className="dialog-actions create-project-dialog-actions">
                <button
                  className="ghost-button"
                  type="button"
                  onClick={() => void handlePickProjectRoot()}
                  disabled={isCreatingProject || !newProjectUsesLocalRemote}
                >
                  Choose folder
                </button>
                <button
                  className="ghost-button"
                  type="button"
                  onClick={() => {
                    setRequestError(null);
                    setIsCreateProjectOpen(false);
                  }}
                  disabled={isCreatingProject}
                >
                  Cancel
                </button>
                <button className="send-button create-project-submit" type="submit" disabled={isCreatingProject}>
                  {isCreatingProject ? "Adding..." : "Add project"}
                </button>
              </div>
            </form>
          </section>
        </div>
      ) : null}
      {isSettingsOpen ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            setIsSettingsOpen(false);
          }}
        >
          <section
            id="settings-dialog"
            className="dialog-card panel settings-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="settings-dialog-header">
              <div>
                <div className="card-label">Preferences</div>
                <h2 id="settings-dialog-title">Settings</h2>
                <p className="dialog-copy settings-dialog-copy">
                  Tune the interface without disturbing active sessions.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={() => {
                  setIsSettingsOpen(false);
                }}
              >
                <DialogCloseIcon />
              </button>
            </div>

            <div className="settings-dialog-body">
              <div className="settings-tab-list" role="tablist" aria-label="Preferences sections">
                {PREFERENCES_TABS.map((tab) => {
                  const isSelected = settingsTab === tab.id;

                  return (
                    <button
                      key={tab.id}
                      id={`settings-tab-${tab.id}`}
                      className={`settings-tab ${isSelected ? "selected" : ""}`}
                      type="button"
                      role="tab"
                      aria-selected={isSelected}
                      aria-controls={`settings-panel-${tab.id}`}
                      onClick={() => setSettingsTab(tab.id)}
                    >
                      {tab.label}
                    </button>
                  );
                })}
              </div>

              <div
                id={`settings-panel-${settingsTab}`}
                className={`settings-tab-panel ${settingsTab === "themes" ? "theme-settings-panel" : ""}`.trim()}
                role="tabpanel"
                aria-labelledby={`settings-tab-${settingsTab}`}
              >
                {settingsTab === "themes" ? (
                  <ThemePicker
                    activeTheme={activeTheme}
                    themeId={themeId}
                    onSelectTheme={setThemeId}
                  />
                ) : settingsTab === "appearance" ? (
                  <AppearancePreferencesPanel
                    editorFontSizePx={editorFontSizePx}
                    fontSizePx={fontSizePx}
                    onSelectEditorFontSize={(nextValue) =>
                      setEditorFontSizePx(clampEditorFontSizePreference(nextValue))
                    }
                    onSelectFontSize={(nextValue) =>
                      setFontSizePx(clampFontSizePreference(nextValue))
                    }
                  />
                ) : settingsTab === "remotes" ? (
                  <RemotePreferencesPanel
                    remotes={remoteConfigs}
                    onSaveRemotes={(nextRemotes) => {
                      void persistAppPreferences({ remotes: nextRemotes });
                    }}
                  />
                ) : settingsTab === "codex-prompts" ? (
                  <CodexPromptPreferencesPanel
                    defaultApprovalPolicy={defaultCodexApprovalPolicy}
                    defaultReasoningEffort={defaultCodexReasoningEffort}
                    defaultSandboxMode={defaultCodexSandboxMode}
                    onSelectApprovalPolicy={setDefaultCodexApprovalPolicy}
                    onSelectReasoningEffort={handleDefaultCodexReasoningEffortChange}
                    onSelectSandboxMode={setDefaultCodexSandboxMode}
                  />
                ) : (
                  <ClaudeApprovalsPreferencesPanel
                    defaultClaudeApprovalMode={defaultClaudeApprovalMode}
                    defaultClaudeEffort={defaultClaudeEffort}
                    onSelectEffort={handleDefaultClaudeEffortChange}
                    onSelectMode={setDefaultClaudeApprovalMode}
                  />
                )}
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}

function ThemePicker({
  activeTheme,
  themeId,
  onSelectTheme,
}: {
  activeTheme: (typeof THEMES)[number];
  themeId: ThemeId;
  onSelectTheme: (themeId: ThemeId) => void;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const dragStateRef = useRef<{
    pointerId: number;
    startPointerY: number;
    startThumbOffset: number;
  } | null>(null);
  const [scrollState, setScrollState] = useState({
    thumbHeight: 0,
    thumbOffset: 0,
    visible: false,
  });
  const [isDraggingScrollbar, setIsDraggingScrollbar] = useState(false);

  useLayoutEffect(() => {
    const listElement = listRef.current;
    if (!listElement) {
      return;
    }

    const updateScrollState = () => {
      const { clientHeight, scrollHeight, scrollTop } = listElement;
      const hasOverflow = scrollHeight - clientHeight > 1;

      if (!hasOverflow) {
        setScrollState((current) =>
          current.visible || current.thumbHeight !== 0 || current.thumbOffset !== 0
            ? { thumbHeight: 0, thumbOffset: 0, visible: false }
            : current,
        );
        return;
      }

      const thumbHeight = Math.max((clientHeight * clientHeight) / scrollHeight, 48);
      const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
      const maxScrollTop = Math.max(scrollHeight - clientHeight, 1);
      const thumbOffset = (scrollTop / maxScrollTop) * maxThumbOffset;

      setScrollState((current) => {
        if (
          current.visible &&
          Math.abs(current.thumbHeight - thumbHeight) < 0.5 &&
          Math.abs(current.thumbOffset - thumbOffset) < 0.5
        ) {
          return current;
        }

        return {
          thumbHeight,
          thumbOffset,
          visible: true,
        };
      });
    };

    updateScrollState();

    listElement.addEventListener("scroll", updateScrollState);
    window.addEventListener("resize", updateScrollState);

    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(() => {
            updateScrollState();
          });

    resizeObserver?.observe(listElement);

    return () => {
      listElement.removeEventListener("scroll", updateScrollState);
      window.removeEventListener("resize", updateScrollState);
      resizeObserver?.disconnect();
    };
  }, []);

  useEffect(() => {
    function handlePointerMove(event: PointerEvent) {
      const dragState = dragStateRef.current;
      const listElement = listRef.current;
      if (!dragState || !listElement || event.pointerId !== dragState.pointerId) {
        return;
      }

      const { clientHeight, scrollHeight } = listElement;
      const thumbHeight = Math.max((clientHeight * clientHeight) / Math.max(scrollHeight, 1), 48);
      const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
      const nextThumbOffset = clamp(
        dragState.startThumbOffset + (event.clientY - dragState.startPointerY),
        0,
        maxThumbOffset,
      );
      const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);

      listElement.scrollTop =
        maxThumbOffset > 0 ? (nextThumbOffset / maxThumbOffset) * maxScrollTop : 0;
    }

    function handlePointerUp(event: PointerEvent) {
      const dragState = dragStateRef.current;
      if (!dragState || event.pointerId !== dragState.pointerId) {
        return;
      }

      dragStateRef.current = null;
      setIsDraggingScrollbar(false);
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };
  }, []);

  function scrollListToThumbOffset(nextThumbOffset: number) {
    const listElement = listRef.current;
    if (!listElement) {
      return;
    }

    const { clientHeight, scrollHeight } = listElement;
    const thumbHeight = Math.max((clientHeight * clientHeight) / Math.max(scrollHeight, 1), 48);
    const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
    const clampedThumbOffset = clamp(nextThumbOffset, 0, maxThumbOffset);
    const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);

    listElement.scrollTop =
      maxThumbOffset > 0 ? (clampedThumbOffset / maxThumbOffset) * maxScrollTop : 0;
  }

  function handleScrollbarThumbPointerDown(event: ReactPointerEvent<HTMLSpanElement>) {
    event.preventDefault();
    event.stopPropagation();

    dragStateRef.current = {
      pointerId: event.pointerId,
      startPointerY: event.clientY,
      startThumbOffset: scrollState.thumbOffset,
    };
    setIsDraggingScrollbar(true);
  }

  function handleScrollbarTrackPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();

    const trackRect = event.currentTarget.getBoundingClientRect();
    const nextThumbOffset = event.clientY - trackRect.top - scrollState.thumbHeight / 2;
    const { clientHeight } = event.currentTarget;
    const clampedThumbOffset = clamp(
      nextThumbOffset,
      0,
      Math.max(clientHeight - scrollState.thumbHeight, 0),
    );

    scrollListToThumbOffset(clampedThumbOffset);
    dragStateRef.current = {
      pointerId: event.pointerId,
      startPointerY: event.clientY,
      startThumbOffset: clampedThumbOffset,
    };
    setIsDraggingScrollbar(true);
  }

  return (
    <section className="theme-panel">
      <div className="theme-panel-header">
        <div>
          <p className="session-control-label">Appearance</p>
          <p className="theme-panel-copy">{activeTheme.description}</p>
        </div>
        <span className="theme-active-badge">{activeTheme.name}</span>
      </div>

      <div className="theme-option-list-shell">
        <div ref={listRef} className="theme-option-list" role="radiogroup" aria-label="UI theme">
          {THEMES.map((theme) => {
            const isSelected = theme.id === themeId;

            return (
              <button
                key={theme.id}
                className={`theme-option ${isSelected ? "selected" : ""}`}
                type="button"
                role="radio"
                aria-checked={isSelected}
                onClick={() => onSelectTheme(theme.id)}
              >
                <span className="theme-option-main">
                  <span className="theme-option-title-row">
                    <strong className="theme-option-title">{theme.name}</strong>
                    {isSelected ? <span className="theme-option-status">Live</span> : null}
                  </span>
                  <span className="theme-option-copy">{theme.description}</span>
                </span>
                <span className="theme-option-preview" aria-hidden="true">
                  {theme.swatches.map((swatch) => (
                    <span
                      key={`${theme.id}-${swatch}`}
                      className="theme-option-swatch"
                      style={{ background: swatch }}
                    />
                  ))}
                </span>
              </button>
            );
          })}
        </div>
        {scrollState.visible ? (
          <div
            className={`theme-option-scrollbar ${isDraggingScrollbar ? "dragging" : ""}`}
            aria-hidden="true"
            onPointerDown={handleScrollbarTrackPointerDown}
          >
            <span
              className="theme-option-scrollbar-thumb"
              onPointerDown={handleScrollbarThumbPointerDown}
              style={{
                height: `${scrollState.thumbHeight}px`,
                transform: `translateY(${scrollState.thumbOffset}px)`,
              }}
            />
          </div>
        ) : null}
      </div>
    </section>
  );
}

function AppearancePreferencesPanel({
  editorFontSizePx,
  fontSizePx,
  onSelectEditorFontSize,
  onSelectFontSize,
}: {
  editorFontSizePx: number;
  fontSizePx: number;
  onSelectEditorFontSize: (fontSizePx: number) => void;
  onSelectFontSize: (fontSizePx: number) => void;
}) {
  const canDecreaseFontSize = fontSizePx > MIN_FONT_SIZE_PX;
  const canIncreaseFontSize = fontSizePx < MAX_FONT_SIZE_PX;
  const canDecreaseEditorFontSize = editorFontSizePx > MIN_EDITOR_FONT_SIZE_PX;
  const canIncreaseEditorFontSize = editorFontSizePx < MAX_EDITOR_FONT_SIZE_PX;

  return (
    <section className="settings-panel-stack">
      <article className="message-card prompt-settings-card appearance-settings-card">
        <div className="card-label">Appearance</div>
        <h3>Font sizes</h3>
        <div className="prompt-settings-grid appearance-settings-grid">
          <FontSizePreferenceControl
            canDecrease={canDecreaseFontSize}
            canIncrease={canIncreaseFontSize}
            controlsLabel="UI font size controls"
            defaultValue={DEFAULT_FONT_SIZE_PX}
            decreaseId="font-size-decrease"
            label="UI"
            onSelectFontSize={onSelectFontSize}
            resetId="font-size-reset"
            value={fontSizePx}
          />
          <FontSizePreferenceControl
            canDecrease={canDecreaseEditorFontSize}
            canIncrease={canIncreaseEditorFontSize}
            controlsLabel="Editor font size controls"
            defaultValue={DEFAULT_EDITOR_FONT_SIZE_PX}
            decreaseId="editor-font-size-decrease"
            label="Editor"
            onSelectFontSize={onSelectEditorFontSize}
            resetId="editor-font-size-reset"
            value={editorFontSizePx}
          />
          <p className="session-control-hint">
            UI changes apply across the interface. Editor changes affect source and diff editors. Both are saved in this browser.
          </p>
        </div>
      </article>
    </section>
  );
}

function FontSizePreferenceControl({
  canDecrease,
  canIncrease,
  controlsLabel,
  defaultValue,
  decreaseId,
  label,
  onSelectFontSize,
  resetId,
  value,
}: {
  canDecrease: boolean;
  canIncrease: boolean;
  controlsLabel: string;
  defaultValue: number;
  decreaseId: string;
  label: string;
  onSelectFontSize: (fontSizePx: number) => void;
  resetId: string;
  value: number;
}) {
  return (
    <>
      <div className="session-control-group">
        <label className="session-control-label" htmlFor={decreaseId}>
          {label}
        </label>
        <div className="font-size-controls" role="group" aria-label={controlsLabel}>
          <button
            id={decreaseId}
            className="ghost-button font-size-stepper"
            type="button"
            onClick={() => onSelectFontSize(value - 1)}
            disabled={!canDecrease}
          >
            A-
          </button>
          <div className="font-size-readout" aria-live="polite">
            <strong className="font-size-readout-value">{value}px</strong>
            <span className="font-size-readout-copy">
              {value === defaultValue ? "Default" : "Live"}
            </span>
          </div>
          <button
            className="ghost-button font-size-stepper"
            type="button"
            onClick={() => onSelectFontSize(value + 1)}
            disabled={!canIncrease}
          >
            A+
          </button>
        </div>
      </div>
      <div className="session-control-group">
        <label className="session-control-label" htmlFor={resetId}>
          Reset
        </label>
        <button
          id={resetId}
          className="ghost-button font-size-reset"
          type="button"
          onClick={() => onSelectFontSize(defaultValue)}
          disabled={value === defaultValue}
        >
          Use default
        </button>
      </div>
    </>
  );
}

function validateRemoteDrafts(remotes: RemoteConfig[]): string | null {
  let hasLocalRemote = false;
  const seenIds = new Set<string>();

  for (const remote of remotes) {
    const id = remote.id.trim();
    const name = remote.name.trim();
    if (!id) {
      return "Every remote needs an id.";
    }

    const normalizedId = id.toLowerCase();
    if (seenIds.has(normalizedId)) {
      return `Remote ids must be unique. Duplicate id: ${id}.`;
    }
    seenIds.add(normalizedId);

    if (!name) {
      return `Remote ${id} needs a name.`;
    }

    if (isLocalRemoteId(id)) {
      hasLocalRemote = true;
      continue;
    }

    if (!(remote.host?.trim())) {
      return `Remote ${name} needs a host.`;
    }
    if (remote.port !== null && remote.port !== undefined) {
      if (!Number.isInteger(remote.port) || remote.port <= 0 || remote.port > 65535) {
        return `Remote ${name} has an invalid SSH port.`;
      }
    }
  }

  return hasLocalRemote ? null : "The built-in local remote is required.";
}

function createSshRemoteDraft(remotes: RemoteConfig[]): RemoteConfig {
  let suffix = remotes.length;
  let id = `ssh-${suffix}`;
  const existingIds = new Set(remotes.map((remote) => remote.id.trim().toLowerCase()));
  while (existingIds.has(id)) {
    suffix += 1;
    id = `ssh-${suffix}`;
  }

  return {
    id,
    name: `SSH Remote ${suffix}`,
    transport: "ssh",
    enabled: true,
    host: "",
    port: 22,
    user: "",
  };
}

function RemotePreferencesPanel({
  remotes,
  onSaveRemotes,
}: {
  remotes: RemoteConfig[];
  onSaveRemotes: (remotes: RemoteConfig[]) => void;
}) {
  const [draftRemotes, setDraftRemotes] = useState<RemoteConfig[]>(remotes);

  useEffect(() => {
    setDraftRemotes((current) => (areRemoteConfigsEqual(current, remotes) ? current : remotes));
  }, [remotes]);

  const validationError = useMemo(() => validateRemoteDrafts(draftRemotes), [draftRemotes]);
  const hasChanges = useMemo(
    () => JSON.stringify(draftRemotes) !== JSON.stringify(remotes),
    [draftRemotes, remotes],
  );

  function updateRemote(index: number, patch: Partial<RemoteConfig>) {
    setDraftRemotes((current) =>
      current.map((remote, remoteIndex) =>
        remoteIndex === index ? { ...remote, ...patch } : remote,
      ),
    );
  }

  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">Local control plane</p>
          <p className="settings-panel-copy">
            Remote definitions live in the local TermAl server. Projects point at one remote, and SSH-backed execution will route through that mapping.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card remote-settings-card">
        <div className="card-label">Project Routing</div>
        <h3>Remote definitions</h3>
        <div className="remote-settings-list">
          {draftRemotes.map((remote, index) => {
            const isLocal = isLocalRemoteId(remote.id);
            return (
              <article key={`${remote.id}-${index}`} className="remote-settings-row">
                <div className="remote-settings-row-header">
                  <div>
                    <p className="session-control-label">
                      {isLocal ? "Local remote" : remote.name.trim() || `Remote ${index}`}
                    </p>
                    <p className="settings-panel-copy">{remoteConnectionLabel(remote)}</p>
                  </div>
                  <div className="remote-settings-badges">
                    <span className="remote-settings-badge">{remoteBadgeLabel(remote)}</span>
                    <span className="remote-settings-badge">
                      {remote.enabled || isLocal ? "Enabled" : "Disabled"}
                    </span>
                  </div>
                </div>

                <div className="remote-settings-row-fields">
                  <div className="session-control-group">
                    <label className="session-control-label" htmlFor={`remote-id-${index}`}>
                      Remote id
                    </label>
                    <input
                      id={`remote-id-${index}`}
                      className="themed-input"
                      type="text"
                      value={remote.id}
                      onChange={(event) => updateRemote(index, { id: event.target.value })}
                      disabled={isLocal}
                    />
                  </div>
                  <div className="session-control-group">
                    <label className="session-control-label" htmlFor={`remote-name-${index}`}>
                      Name
                    </label>
                    <input
                      id={`remote-name-${index}`}
                      className="themed-input"
                      type="text"
                      value={remote.name}
                      onChange={(event) => updateRemote(index, { name: event.target.value })}
                      disabled={isLocal}
                    />
                  </div>
                  {!isLocal ? (
                    <>
                      <div className="session-control-group">
                        <label className="session-control-label" htmlFor={`remote-host-${index}`}>
                          Host
                        </label>
                        <input
                          id={`remote-host-${index}`}
                          className="themed-input"
                          type="text"
                          value={remote.host ?? ""}
                          onChange={(event) => updateRemote(index, { host: event.target.value })}
                        />
                      </div>
                      <div className="session-control-group">
                        <label className="session-control-label" htmlFor={`remote-user-${index}`}>
                          User
                        </label>
                        <input
                          id={`remote-user-${index}`}
                          className="themed-input"
                          type="text"
                          value={remote.user ?? ""}
                          onChange={(event) => updateRemote(index, { user: event.target.value })}
                        />
                      </div>
                      <div className="session-control-group">
                        <label className="session-control-label" htmlFor={`remote-port-${index}`}>
                          Port
                        </label>
                        <input
                          id={`remote-port-${index}`}
                          className="themed-input"
                          type="number"
                          min={1}
                          max={65535}
                          value={remote.port ?? 22}
                          onChange={(event) => {
                            const value = event.target.value.trim();
                            updateRemote(index, {
                              port: value ? Number.parseInt(value, 10) : null,
                            });
                          }}
                        />
                      </div>
                      <label className="remote-settings-toggle">
                        <input
                          type="checkbox"
                          checked={remote.enabled}
                          onChange={(event) => updateRemote(index, { enabled: event.target.checked })}
                        />
                        <span>Enabled for projects and sessions</span>
                      </label>
                    </>
                  ) : null}
                </div>

                {!isLocal ? (
                  <div className="remote-settings-row-actions">
                    <button
                      className="ghost-button"
                      type="button"
                      onClick={() => {
                        setDraftRemotes((current) => current.filter((_, remoteIndex) => remoteIndex !== index));
                      }}
                    >
                      Remove
                    </button>
                  </div>
                ) : null}
              </article>
            );
          })}
        </div>

        <div className="remote-settings-actions">
          <button
            className="ghost-button"
            type="button"
            onClick={() => {
              setDraftRemotes((current) => [...current, createSshRemoteDraft(current)]);
            }}
          >
            Add SSH remote
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={() => setDraftRemotes(remotes)}
            disabled={!hasChanges}
          >
            Reset
          </button>
          <button
            className="send-button"
            type="button"
            onClick={() => onSaveRemotes(draftRemotes)}
            disabled={!hasChanges || !!validationError}
          >
            Save remotes
          </button>
        </div>

        <p className="session-control-hint">
          {validationError ??
            "Remote ids become stable project routing keys. If you rename one later, the backend will reject the change while projects still reference the old id."}
        </p>
      </article>
    </section>
  );
}
function ClaudeApprovalsPreferencesPanel({
  defaultClaudeApprovalMode,
  defaultClaudeEffort,
  onSelectEffort,
  onSelectMode,
}: {
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  defaultClaudeEffort: ClaudeEffortLevel;
  onSelectEffort: (effort: ClaudeEffortLevel) => void;
  onSelectMode: (mode: ClaudeApprovalMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Claude sessions</p>
          <p className="settings-panel-copy">
            Choose the default Claude mode and effort for sessions created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Claude startup settings</h3>
        <div className="prompt-settings-grid">
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-claude-approval-mode">
              Default Claude mode
            </label>
            <ThemedCombobox
              id="default-claude-approval-mode"
              className="prompt-settings-select"
              value={defaultClaudeApprovalMode}
              options={CLAUDE_APPROVAL_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectMode(nextValue as ClaudeApprovalMode)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-claude-effort">
              Default Claude effort
            </label>
            <ThemedCombobox
              id="default-claude-effort"
              className="prompt-settings-select"
              value={defaultClaudeEffort}
              options={CLAUDE_EFFORT_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectEffort(nextValue as ClaudeEffortLevel)}
            />
          </div>
          <p className="session-control-hint">
            Ask keeps approval cards, Auto-approve continues through tool requests, and Plan keeps
            Claude read-only. The effort setting is used when a new Claude session starts. Existing
            sessions keep their current mode and effort.
          </p>
        </div>
      </article>
    </section>
  );
}

function CodexPromptPreferencesPanel({
  defaultApprovalPolicy,
  defaultReasoningEffort,
  defaultSandboxMode,
  onSelectApprovalPolicy,
  onSelectReasoningEffort,
  onSelectSandboxMode,
}: {
  defaultApprovalPolicy: ApprovalPolicy;
  defaultReasoningEffort: CodexReasoningEffort;
  defaultSandboxMode: SandboxMode;
  onSelectApprovalPolicy: (policy: ApprovalPolicy) => void;
  onSelectReasoningEffort: (effort: CodexReasoningEffort) => void;
  onSelectSandboxMode: (mode: SandboxMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Codex sessions</p>
          <p className="settings-panel-copy">
            Choose the default sandbox, approval policy, and reasoning effort for Codex sessions
            created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Codex prompt settings</h3>
        <div className="prompt-settings-grid">
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-sandbox-mode">
              Default sandbox
            </label>
            <ThemedCombobox
              id="default-codex-sandbox-mode"
              className="prompt-settings-select"
              value={defaultSandboxMode}
              options={SANDBOX_MODE_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectSandboxMode(nextValue as SandboxMode)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-approval-policy">
              Default approval policy
            </label>
            <ThemedCombobox
              id="default-codex-approval-policy"
              className="prompt-settings-select"
              value={defaultApprovalPolicy}
              options={APPROVAL_POLICY_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectApprovalPolicy(nextValue as ApprovalPolicy)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-reasoning-effort">
              Default reasoning effort
            </label>
            <ThemedCombobox
              id="default-codex-reasoning-effort"
              className="prompt-settings-select"
              value={defaultReasoningEffort}
              options={CODEX_REASONING_EFFORT_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectReasoningEffort(nextValue as CodexReasoningEffort)}
            />
          </div>
          <p className="session-control-hint">
            This only affects new Codex sessions you create here. Existing sessions keep their
            current prompt settings.
          </p>
        </div>
      </article>
    </section>
  );
}

export function ThemedCombobox({
  "aria-label": ariaLabel,
  "aria-labelledby": ariaLabelledBy,
  className,
  disabled = false,
  id,
  onChange,
  options,
  value,
}: {
  "aria-label"?: string;
  "aria-labelledby"?: string;
  className?: string;
  disabled?: boolean;
  id: string;
  onChange: (nextValue: string) => void;
  options: readonly ComboboxOption[];
  value: string;
}) {
  const listboxId = useId();
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(() =>
    Math.max(
      options.findIndex((option) => option.value === value),
      0,
    ),
  );
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);

  const selectedIndex = options.findIndex((option) => option.value === value);
  const safeSelectedIndex = selectedIndex >= 0 ? selectedIndex : 0;
  const selectedOption = options[safeSelectedIndex] ?? options[0];

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    setActiveIndex(safeSelectedIndex);
  }, [isOpen, safeSelectedIndex]);

  useLayoutEffect(() => {
    if (!isOpen || !menuStyle) {
      return;
    }

    const listbox = listRef.current;
    if (!listbox) {
      return;
    }

    const activeOption = listbox.querySelector<HTMLElement>(`[data-option-index="${activeIndex}"]`);
    if (!activeOption) {
      return;
    }

    const listRect = listbox.getBoundingClientRect();
    const optionRect = activeOption.getBoundingClientRect();

    if (optionRect.top < listRect.top) {
      listbox.scrollTop += optionRect.top - listRect.top;
    } else if (optionRect.bottom > listRect.bottom) {
      listbox.scrollTop += optionRect.bottom - listRect.bottom;
    }
  }, [activeIndex, isOpen, menuStyle]);

  useLayoutEffect(() => {
    if (!isOpen) {
      return;
    }

    function updateMenuStyle() {
      const trigger = triggerRef.current;
      if (!trigger) {
        return;
      }

      const rect = trigger.getBoundingClientRect();
      const viewportPadding = 12;
      const estimatedHeight = Math.min(Math.max(options.length * 76 + 12, 120), 360);
      const availableBelow = window.innerHeight - rect.bottom - viewportPadding;
      const availableAbove = rect.top - viewportPadding;
      const openUpward =
        availableBelow < Math.min(estimatedHeight, 220) && availableAbove > availableBelow;
      const maxHeight = Math.max(openUpward ? availableAbove : availableBelow, 140);

      setMenuStyle({
        left: rect.left,
        width: rect.width,
        maxHeight,
        top: openUpward ? undefined : rect.bottom + 8,
        bottom: openUpward ? window.innerHeight - rect.top + 8 : undefined,
      });
    }

    updateMenuStyle();
    window.addEventListener("resize", updateMenuStyle);
    window.addEventListener("scroll", updateMenuStyle, true);

    return () => {
      window.removeEventListener("resize", updateMenuStyle);
      window.removeEventListener("scroll", updateMenuStyle, true);
    };
  }, [isOpen, options.length]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }

      if (triggerRef.current?.contains(target) || listRef.current?.contains(target)) {
        return;
      }

      setIsOpen(false);
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsOpen(false);
        triggerRef.current?.focus();
        return;
      }

      if (event.key === "Tab") {
        setIsOpen(false);
        return;
      }

      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActiveIndex((current) => (current + 1) % options.length);
        return;
      }

      if (event.key === "ArrowUp") {
        event.preventDefault();
        setActiveIndex((current) => (current - 1 + options.length) % options.length);
        return;
      }

      if (event.key === "Home") {
        event.preventDefault();
        setActiveIndex(0);
        return;
      }

      if (event.key === "End") {
        event.preventDefault();
        setActiveIndex(options.length - 1);
        return;
      }

      if (event.key === " " || event.key === "Enter") {
        event.preventDefault();
        const nextOption = options[activeIndex];
        if (!nextOption) {
          return;
        }

        onChange(nextOption.value);
        if (event.key === "Enter") {
          setIsOpen(false);
          triggerRef.current?.focus();
        }
      }
    }

    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [activeIndex, isOpen, onChange, options]);

  function handleTriggerKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    if (disabled) {
      return;
    }

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex(safeSelectedIndex);
      setIsOpen(true);
      return;
    }

    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (!isOpen) {
        setActiveIndex(safeSelectedIndex);
        setIsOpen(true);
      }
    }
  }

  return (
    <>
      <button
        ref={triggerRef}
        id={id}
        className={`session-select combo-trigger ${className ?? ""}`.trim()}
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-controls={listboxId}
        aria-expanded={isOpen}
        aria-haspopup="listbox"
        aria-labelledby={ariaLabelledBy}
        aria-activedescendant={isOpen ? `${listboxId}-option-${activeIndex}` : undefined}
        disabled={disabled}
        onClick={() => {
          if (!disabled) {
            setActiveIndex(safeSelectedIndex);
            setIsOpen((current) => !current);
          }
        }}
        onKeyDown={handleTriggerKeyDown}
      >
        <span className="combo-trigger-value">{selectedOption?.label ?? value}</span>
        <span className={`combo-trigger-caret ${isOpen ? "open" : ""}`} aria-hidden="true">
          v
        </span>
      </button>

      {isOpen && menuStyle
        ? createPortal(
            <div
              ref={listRef}
              id={listboxId}
              className="combo-menu"
              role="listbox"
              style={menuStyle}
            >
              {options.map((option, index) => {
                const isSelected = option.value === value;
                const isActive = index === activeIndex;

                return (
                  <button
                    key={option.value}
                    id={`${listboxId}-option-${index}`}
                    className={`combo-option ${isActive ? "active" : ""} ${isSelected ? "selected" : ""}`}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    data-option-index={index}
                    onMouseEnter={() => {
                      setActiveIndex(index);
                    }}
                    onClick={() => {
                      onChange(option.value);
                      setIsOpen(false);
                      triggerRef.current?.focus();
                    }}
                  >
                    <span className="combo-option-copy">
                      <span className="combo-option-label">{option.label}</span>
                      {option.description ? (
                        <span className="combo-option-description">{option.description}</span>
                      ) : null}
                      {option.badges?.length ? (
                        <span className="combo-option-badges">
                          {option.badges.map((badge) => (
                            <span key={badge} className="combo-option-badge">
                              {badge}
                            </span>
                          ))}
                        </span>
                      ) : null}
                    </span>
                    <span
                      className={`combo-option-indicator ${isSelected ? "visible" : ""}`}
                      aria-hidden="true"
                    />
                  </button>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}

function WorkspaceNodeView({
  node,
  codexState,
  projectLookup,
  paneLookup,
  sessionLookup,
  activePaneId,
  isLoading,
  draftsBySessionId,
  draftAttachmentsBySessionId,
  sendingSessionIds,
  stoppingSessionIds,
  killingSessionIds,
  updatingSessionIds,
  refreshingSessionModelOptionIds,
  sessionModelOptionErrors,
  agentCommandsBySessionId,
  refreshingAgentCommandSessionIds,
  agentCommandErrors,
  sessionSettingNotices,
  paneShouldStickToBottomRef,
  paneScrollPositionsRef,
  paneContentSignaturesRef,
  pendingScrollToBottomRequest,
  windowId,
  draggedTab,
  editorAppearance,
  editorFontSizePx,
  onActivatePane,
  onSelectTab,
  onCloseTab,
  onSplitPane,
  onResizeStart,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onPaneViewModeChange,
  onOpenSourceTab,
  onOpenDiffPreviewTab,
  onOpenGitStatusDiffPreviewTab,
  onOpenFilesystemTab,
  onOpenGitStatusTab,
  onOpenInstructionDebuggerTab,
  onPaneSourcePathChange,
  onOpenConversationFromDiff,
  onInsertReviewIntoPrompt,
  onDraftCommit,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onStopSession,
  onKillSession,
  onRenameSessionRequest,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
  onArchiveCodexThread,
  onCompactCodexThread,
  onForkCodexThread,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  renderControlPanel,
  backendConnectionState,
}: {
  node: WorkspaceNode;
  codexState: CodexState;
  projectLookup: Map<string, Project>;
  paneLookup: Map<string, WorkspacePane>;
  sessionLookup: Map<string, Session>;
  activePaneId: string | null;
  isLoading: boolean;
  draftsBySessionId: Record<string, string>;
  draftAttachmentsBySessionId: Record<string, DraftImageAttachment[]>;
  sendingSessionIds: SessionFlagMap;
  stoppingSessionIds: SessionFlagMap;
  killingSessionIds: SessionFlagMap;
  updatingSessionIds: SessionFlagMap;
  refreshingSessionModelOptionIds: SessionFlagMap;
  sessionModelOptionErrors: SessionErrorMap;
  agentCommandsBySessionId: SessionAgentCommandMap;
  refreshingAgentCommandSessionIds: SessionFlagMap;
  agentCommandErrors: SessionErrorMap;
  sessionSettingNotices: SessionNoticeMap;
  paneShouldStickToBottomRef: React.MutableRefObject<Record<string, boolean | undefined>>;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<Record<string, Record<string, string>>>;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  windowId: string;
  draggedTab: WorkspaceTabDrag | null;
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  onActivatePane: (paneId: string) => void;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onResizeStart: (
    splitId: string,
    direction: "row" | "column",
    event: ReactPointerEvent<HTMLDivElement>,
  ) => void;
  onTabDragStart: (drag: WorkspaceTabDrag) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onPaneViewModeChange: (paneId: string, viewMode: SessionPaneViewMode) => void;
  onOpenSourceTab: (
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { line?: number; column?: number; openInNewTab?: boolean },
  ) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusDiffPreviewTab: (
    paneId: string,
    diffPreview: GitDiffResponse,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { openInNewTab?: boolean },
  ) => void;
  onOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenInstructionDebuggerTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onOpenConversationFromDiff: (sessionId: string, preferredPaneId: string | null) => void;
  onInsertReviewIntoPrompt: (
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (sessionId: string, attachments: DraftImageAttachment[]) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onUserInputSubmit: (
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) => void;
  onMcpElicitationSubmit: (
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  onCodexAppRequestSubmit: (
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
  onRenameSessionRequest: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  onScrollToBottomRequestHandled: (token: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onArchiveCodexThread: (sessionId: string) => void;
  onCompactCodexThread: (sessionId: string) => void;
  onForkCodexThread: (sessionId: string, preferredPaneId: string | null) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onRollbackCodexThread: (sessionId: string, numTurns: number) => void;
  onUnarchiveCodexThread: (sessionId: string) => void;
  renderControlPanel: (paneId: string) => JSX.Element;
  backendConnectionState: BackendConnectionState;
}) {
  if (node.type === "pane") {
    const pane = paneLookup.get(node.paneId);
    if (!pane) {
      return null;
    }

    return (
      <SessionPaneView
        pane={pane}
        codexState={codexState}
        projectLookup={projectLookup}
        sessionLookup={sessionLookup}
        isActive={pane.id === activePaneId}
        isLoading={isLoading}
        draft={pane.activeSessionId ? draftsBySessionId[pane.activeSessionId] ?? "" : ""}
        draftAttachments={
          pane.activeSessionId ? draftAttachmentsBySessionId[pane.activeSessionId] ?? [] : []
        }
        isSending={pane.activeSessionId ? Boolean(sendingSessionIds[pane.activeSessionId]) : false}
        isStopping={pane.activeSessionId ? Boolean(stoppingSessionIds[pane.activeSessionId]) : false}
        isKilling={pane.activeSessionId ? Boolean(killingSessionIds[pane.activeSessionId]) : false}
        isUpdating={pane.activeSessionId ? Boolean(updatingSessionIds[pane.activeSessionId]) : false}
        isRefreshingModelOptions={
          pane.activeSessionId ? Boolean(refreshingSessionModelOptionIds[pane.activeSessionId]) : false
        }
        modelOptionsError={
          pane.activeSessionId ? (sessionModelOptionErrors[pane.activeSessionId] ?? null) : null
        }
        agentCommands={
          pane.activeSessionId && Object.prototype.hasOwnProperty.call(agentCommandsBySessionId, pane.activeSessionId)
            ? (agentCommandsBySessionId[pane.activeSessionId] ?? [])
            : []
        }
        hasLoadedAgentCommands={
          pane.activeSessionId
            ? Object.prototype.hasOwnProperty.call(agentCommandsBySessionId, pane.activeSessionId)
            : false
        }
        isRefreshingAgentCommands={
          pane.activeSessionId ? Boolean(refreshingAgentCommandSessionIds[pane.activeSessionId]) : false
        }
        agentCommandsError={
          pane.activeSessionId ? (agentCommandErrors[pane.activeSessionId] ?? null) : null
        }
        sessionSettingNotice={
          pane.activeSessionId ? (sessionSettingNotices[pane.activeSessionId] ?? null) : null
        }
        paneShouldStickToBottomRef={paneShouldStickToBottomRef}
        paneScrollPositionsRef={paneScrollPositionsRef}
        paneContentSignaturesRef={paneContentSignaturesRef}
        pendingScrollToBottomRequest={pendingScrollToBottomRequest}
        windowId={windowId}
        draggedTab={draggedTab}
        editorAppearance={editorAppearance}
        editorFontSizePx={editorFontSizePx}
        onActivatePane={onActivatePane}
        onSelectTab={onSelectTab}
        onCloseTab={onCloseTab}
        onSplitPane={onSplitPane}
        onTabDragStart={onTabDragStart}
        onTabDragEnd={onTabDragEnd}
        onTabDrop={onTabDrop}
        onPaneViewModeChange={onPaneViewModeChange}
        onOpenSourceTab={onOpenSourceTab}
        onOpenDiffPreviewTab={onOpenDiffPreviewTab}
        onOpenGitStatusDiffPreviewTab={onOpenGitStatusDiffPreviewTab}
        onOpenFilesystemTab={onOpenFilesystemTab}
        onOpenGitStatusTab={onOpenGitStatusTab}
        onOpenInstructionDebuggerTab={onOpenInstructionDebuggerTab}
        onPaneSourcePathChange={onPaneSourcePathChange}
        onOpenConversationFromDiff={onOpenConversationFromDiff}
        onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
        onDraftCommit={onDraftCommit}
        onDraftAttachmentsAdd={onDraftAttachmentsAdd}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        onComposerError={onComposerError}
        onSend={onSend}
        onCancelQueuedPrompt={onCancelQueuedPrompt}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={onUserInputSubmit}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
        onCodexAppRequestSubmit={onCodexAppRequestSubmit}
        onStopSession={onStopSession}
        onKillSession={onKillSession}
        onRenameSessionRequest={onRenameSessionRequest}
        onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
        onSessionSettingsChange={onSessionSettingsChange}
        onArchiveCodexThread={onArchiveCodexThread}
        onCompactCodexThread={onCompactCodexThread}
        onForkCodexThread={onForkCodexThread}
        onRefreshSessionModelOptions={onRefreshSessionModelOptions}
        onRefreshAgentCommands={onRefreshAgentCommands}
        onRollbackCodexThread={onRollbackCodexThread}
        onUnarchiveCodexThread={onUnarchiveCodexThread}
        renderControlPanel={renderControlPanel}
        backendConnectionState={backendConnectionState}
      />
    );
  }

  const firstContainsControlPanel = workspaceNodeContainsControlPanel(node.first, paneLookup);
  const secondContainsControlPanel = workspaceNodeContainsControlPanel(node.second, paneLookup);
  const shouldMarkControlPanelBranch =
    node.direction === "row" && firstContainsControlPanel !== secondContainsControlPanel;
  const firstBranchClassName =
    shouldMarkControlPanelBranch && firstContainsControlPanel
      ? "tile-branch control-panel-branch"
      : "tile-branch";
  const secondBranchClassName =
    shouldMarkControlPanelBranch && secondContainsControlPanel
      ? "tile-branch control-panel-branch"
      : "tile-branch";

  return (
    <div className={`tile-split tile-split-${node.direction}`}>
      <div
        className={firstBranchClassName}
        style={{ flexGrow: node.ratio, flexBasis: 0 }}
      >
        <WorkspaceNodeView
          node={node.first}
          codexState={codexState}
          projectLookup={projectLookup}
          paneLookup={paneLookup}
          sessionLookup={sessionLookup}
          activePaneId={activePaneId}
          isLoading={isLoading}
          draftsBySessionId={draftsBySessionId}
          draftAttachmentsBySessionId={draftAttachmentsBySessionId}
          sendingSessionIds={sendingSessionIds}
          stoppingSessionIds={stoppingSessionIds}
          killingSessionIds={killingSessionIds}
          updatingSessionIds={updatingSessionIds}
          refreshingSessionModelOptionIds={refreshingSessionModelOptionIds}
          sessionModelOptionErrors={sessionModelOptionErrors}
          agentCommandsBySessionId={agentCommandsBySessionId}
          refreshingAgentCommandSessionIds={refreshingAgentCommandSessionIds}
          agentCommandErrors={agentCommandErrors}
          sessionSettingNotices={sessionSettingNotices}
          paneShouldStickToBottomRef={paneShouldStickToBottomRef}
          paneScrollPositionsRef={paneScrollPositionsRef}
          paneContentSignaturesRef={paneContentSignaturesRef}
          pendingScrollToBottomRequest={pendingScrollToBottomRequest}
          windowId={windowId}
          draggedTab={draggedTab}
          editorAppearance={editorAppearance}
          editorFontSizePx={editorFontSizePx}
          onActivatePane={onActivatePane}
          onSelectTab={onSelectTab}
          onCloseTab={onCloseTab}
          onSplitPane={onSplitPane}
          onResizeStart={onResizeStart}
          onTabDragStart={onTabDragStart}
          onTabDragEnd={onTabDragEnd}
          onTabDrop={onTabDrop}
          onPaneViewModeChange={onPaneViewModeChange}
          onOpenSourceTab={onOpenSourceTab}
          onOpenDiffPreviewTab={onOpenDiffPreviewTab}
          onOpenGitStatusDiffPreviewTab={onOpenGitStatusDiffPreviewTab}
          onOpenFilesystemTab={onOpenFilesystemTab}
          onOpenGitStatusTab={onOpenGitStatusTab}
          onOpenInstructionDebuggerTab={onOpenInstructionDebuggerTab}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onOpenConversationFromDiff={onOpenConversationFromDiff}
          onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onRenameSessionRequest={onRenameSessionRequest}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
          onArchiveCodexThread={onArchiveCodexThread}
          onCompactCodexThread={onCompactCodexThread}
          onForkCodexThread={onForkCodexThread}
          onRefreshSessionModelOptions={onRefreshSessionModelOptions}
          onRefreshAgentCommands={onRefreshAgentCommands}
          onRollbackCodexThread={onRollbackCodexThread}
          onUnarchiveCodexThread={onUnarchiveCodexThread}
          renderControlPanel={renderControlPanel}
          backendConnectionState={backendConnectionState}
        />
      </div>

      <div
        className={`tile-divider tile-divider-${node.direction}`}
        onPointerDown={(event) => onResizeStart(node.id, node.direction, event)}
      />

      <div
        className={secondBranchClassName}
        style={{ flexGrow: 1 - node.ratio, flexBasis: 0 }}
      >
        <WorkspaceNodeView
          node={node.second}
          codexState={codexState}
          projectLookup={projectLookup}
          paneLookup={paneLookup}
          sessionLookup={sessionLookup}
          activePaneId={activePaneId}
          isLoading={isLoading}
          draftsBySessionId={draftsBySessionId}
          draftAttachmentsBySessionId={draftAttachmentsBySessionId}
          sendingSessionIds={sendingSessionIds}
          stoppingSessionIds={stoppingSessionIds}
          killingSessionIds={killingSessionIds}
          updatingSessionIds={updatingSessionIds}
          refreshingSessionModelOptionIds={refreshingSessionModelOptionIds}
          sessionModelOptionErrors={sessionModelOptionErrors}
          agentCommandsBySessionId={agentCommandsBySessionId}
          refreshingAgentCommandSessionIds={refreshingAgentCommandSessionIds}
          agentCommandErrors={agentCommandErrors}
          sessionSettingNotices={sessionSettingNotices}
          paneShouldStickToBottomRef={paneShouldStickToBottomRef}
          paneScrollPositionsRef={paneScrollPositionsRef}
          paneContentSignaturesRef={paneContentSignaturesRef}
          pendingScrollToBottomRequest={pendingScrollToBottomRequest}
          windowId={windowId}
          draggedTab={draggedTab}
          editorAppearance={editorAppearance}
          editorFontSizePx={editorFontSizePx}
          onActivatePane={onActivatePane}
          onSelectTab={onSelectTab}
          onCloseTab={onCloseTab}
          onSplitPane={onSplitPane}
          onResizeStart={onResizeStart}
          onTabDragStart={onTabDragStart}
          onTabDragEnd={onTabDragEnd}
          onTabDrop={onTabDrop}
          onPaneViewModeChange={onPaneViewModeChange}
          onOpenSourceTab={onOpenSourceTab}
          onOpenDiffPreviewTab={onOpenDiffPreviewTab}
          onOpenGitStatusDiffPreviewTab={onOpenGitStatusDiffPreviewTab}
          onOpenFilesystemTab={onOpenFilesystemTab}
          onOpenGitStatusTab={onOpenGitStatusTab}
          onOpenInstructionDebuggerTab={onOpenInstructionDebuggerTab}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onOpenConversationFromDiff={onOpenConversationFromDiff}
          onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onRenameSessionRequest={onRenameSessionRequest}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
          onArchiveCodexThread={onArchiveCodexThread}
          onCompactCodexThread={onCompactCodexThread}
          onForkCodexThread={onForkCodexThread}
          onRefreshSessionModelOptions={onRefreshSessionModelOptions}
          onRefreshAgentCommands={onRefreshAgentCommands}
          onRollbackCodexThread={onRollbackCodexThread}
          onUnarchiveCodexThread={onUnarchiveCodexThread}
          renderControlPanel={renderControlPanel}
          backendConnectionState={backendConnectionState}
        />
      </div>
    </div>
  );
}

function SessionPaneView({
  pane,
  codexState,
  projectLookup,
  sessionLookup,
  isActive,
  isLoading,
  draft,
  draftAttachments,
  isSending,
  isStopping,
  isKilling,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  sessionSettingNotice,
  paneShouldStickToBottomRef,
  paneScrollPositionsRef,
  paneContentSignaturesRef,
  pendingScrollToBottomRequest,
  windowId,
  draggedTab,
  editorAppearance,
  editorFontSizePx,
  onActivatePane,
  onSelectTab,
  onCloseTab,
  onSplitPane,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onPaneViewModeChange,
  onOpenSourceTab,
  onOpenDiffPreviewTab,
  onOpenGitStatusDiffPreviewTab,
  onOpenFilesystemTab,
  onOpenGitStatusTab,
  onOpenInstructionDebuggerTab,
  onPaneSourcePathChange,
  onOpenConversationFromDiff,
  onInsertReviewIntoPrompt,
  onDraftCommit,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onStopSession,
  onKillSession,
  onRenameSessionRequest,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
  onArchiveCodexThread,
  onCompactCodexThread,
  onForkCodexThread,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  renderControlPanel,
  backendConnectionState,
}: {
  pane: WorkspacePane;
  codexState: CodexState;
  projectLookup: Map<string, Project>;
  sessionLookup: Map<string, Session>;
  isActive: boolean;
  isLoading: boolean;
  draft: string;
  draftAttachments: DraftImageAttachment[];
  isSending: boolean;
  isStopping: boolean;
  isKilling: boolean;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  agentCommands: AgentCommand[];
  hasLoadedAgentCommands: boolean;
  isRefreshingAgentCommands: boolean;
  agentCommandsError: string | null;
  sessionSettingNotice: string | null;
  paneShouldStickToBottomRef: React.MutableRefObject<Record<string, boolean | undefined>>;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<Record<string, Record<string, string>>>;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  windowId: string;
  draggedTab: WorkspaceTabDrag | null;
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  onActivatePane: (paneId: string) => void;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onTabDragStart: (drag: WorkspaceTabDrag) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onPaneViewModeChange: (paneId: string, viewMode: SessionPaneViewMode) => void;
  onOpenSourceTab: (
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { line?: number; column?: number; openInNewTab?: boolean },
  ) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusDiffPreviewTab: (
    paneId: string,
    diffPreview: GitDiffResponse,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { openInNewTab?: boolean },
  ) => void;
  onOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenInstructionDebuggerTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onOpenConversationFromDiff: (sessionId: string, preferredPaneId: string | null) => void;
  onInsertReviewIntoPrompt: (
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (sessionId: string, attachments: DraftImageAttachment[]) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onUserInputSubmit: (
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) => void;
  onMcpElicitationSubmit: (
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  onCodexAppRequestSubmit: (
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
  onRenameSessionRequest: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  onScrollToBottomRequestHandled: (token: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onArchiveCodexThread: (sessionId: string) => void;
  onCompactCodexThread: (sessionId: string) => void;
  onForkCodexThread: (sessionId: string, preferredPaneId: string | null) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onRollbackCodexThread: (sessionId: string, numTurns: number) => void;
  onUnarchiveCodexThread: (sessionId: string) => void;
  renderControlPanel: (paneId: string) => JSX.Element;
  backendConnectionState: BackendConnectionState;
}) {
  const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null;
  const activeControlPanelTab = activeTab?.kind === "controlPanel" ? activeTab : null;
  const activeSourceTab = activeTab?.kind === "source" ? activeTab : null;
  const activeFilesystemTab = activeTab?.kind === "filesystem" ? activeTab : null;
  const activeGitStatusTab = activeTab?.kind === "gitStatus" ? activeTab : null;
  const activeInstructionDebuggerTab =
    activeTab?.kind === "instructionDebugger" ? activeTab : null;
  const activeDiffPreviewTab = activeTab?.kind === "diffPreview" ? activeTab : null;
  const activeSourceOriginSessionId = activeSourceTab?.originSessionId ?? null;
  const activeSourceOriginProjectId = activeSourceTab?.originProjectId ?? null;
  const activeFilesystemOriginSessionId = activeFilesystemTab?.originSessionId ?? null;
  const activeFilesystemOriginProjectId = activeFilesystemTab?.originProjectId ?? null;
  const activeGitStatusOriginSessionId = activeGitStatusTab?.originSessionId ?? null;
  const activeGitStatusOriginProjectId = activeGitStatusTab?.originProjectId ?? null;
  const activeInstructionDebuggerOriginSessionId =
    activeInstructionDebuggerTab?.originSessionId ?? null;
  const activeInstructionDebuggerOriginProjectId =
    activeInstructionDebuggerTab?.originProjectId ?? null;
  const activeInstructionDebuggerSession =
    activeInstructionDebuggerOriginSessionId
      ? (sessionLookup.get(activeInstructionDebuggerOriginSessionId) ?? null)
      : null;
  const activeDiffOriginSessionId = activeDiffPreviewTab?.originSessionId ?? null;
  const activeDiffOriginProjectId = activeDiffPreviewTab?.originProjectId ?? null;
  const activeDiffWorkspaceRoot =
    (activeDiffOriginSessionId ? (sessionLookup.get(activeDiffOriginSessionId)?.workdir ?? null) : null) ??
    (activeDiffOriginProjectId ? (projectLookup.get(activeDiffOriginProjectId)?.rootPath ?? null) : null);
  const isSessionTabActive = activeTab?.kind === "session";
  const sessionTabs = useMemo(
    () =>
      pane.tabs.flatMap((tab) => {
        if (tab.kind !== "session") {
          return [];
        }

        const session = sessionLookup.get(tab.sessionId);
        return session ? [{ tab, session }] : [];
      }),
    [pane.tabs, sessionLookup],
  );
  const activeSession =
    (pane.activeSessionId ? sessionLookup.get(pane.activeSessionId) : null) ??
    sessionTabs[0]?.session ??
    null;
  const sessions = useMemo(() => sessionTabs.map(({ session }) => session), [sessionTabs]);
  const [sourceDraft, setSourceDraft] = useState(pane.sourcePath ?? "");
  const [fileState, setFileState] = useState<SourceFileState>({
    status: "idle",
    path: "",
    content: "",
    error: null,
    language: null,
  });
  const messageStackRef = useRef<HTMLElement | null>(null);
  const [activeDropPlacement, setActiveDropPlacement] = useState<Exclude<TabDropPlacement, "tabs"> | null>(null);
  const [visitedSessionIds, setVisitedSessionIds] = useState<Record<string, true | undefined>>({});
  const [cachedSessionOrder, setCachedSessionOrder] = useState<string[]>([]);
  const [newResponseIndicatorByKey, setNewResponseIndicatorByKey] = useState<
    Record<string, true | undefined>
  >({});
  const [isSessionFindOpen, setIsSessionFindOpen] = useState(false);
  const [sessionFindQuery, setSessionFindQuery] = useState("");
  const [sessionFindActiveIndex, setSessionFindActiveIndex] = useState(0);
  const sessionFindInputRef = useRef<HTMLInputElement>(null);
  const sessionSearchItemRefsRef = useRef<Record<string, HTMLElement | null>>({});
  const paneHasControlPanel = useMemo(
    () => pane.tabs.some((tab) => tab.kind === "controlPanel"),
    [pane.tabs],
  );
  const allowedDropPlacements = useMemo<Exclude<TabDropPlacement, "tabs">[]>(
    () =>
      draggedTab && (draggedTab.tab.kind === "controlPanel" || paneHasControlPanel)
        ? ["left", "right"]
        : ["left", "top", "right", "bottom"],
    [draggedTab, paneHasControlPanel],
  );
  const showDropOverlay = Boolean(draggedTab) && !(
    draggedTab?.sourceWindowId === windowId &&
    draggedTab?.sourcePaneId === pane.id &&
    pane.tabs.length <= 1
  );
  const sourceCandidatePaths = useMemo(
    () => (activeSourceTab && activeSession ? collectCandidateSourcePaths(activeSession) : []),
    [activeSession, activeSourceTab],
  );
  const commandMessages = useMemo(
    () =>
      pane.viewMode === "commands" && activeSession
        ? activeSession.messages.filter((message): message is CommandMessage => message.type === "command")
        : [],
    [activeSession, pane.viewMode],
  );
  const diffMessages = useMemo(
    () =>
      pane.viewMode === "diffs" && activeSession
        ? activeSession.messages.filter((message): message is DiffMessage => message.type === "diff")
        : [],
    [activeSession, pane.viewMode],
  );
  const pendingPrompts = useMemo(() => activeSession?.pendingPrompts ?? [], [activeSession]);
  const mountedSessionsRef = useRef<Session[]>([]);
  const mountedSessions = useMemo(() => {
    if (!activeSession) {
      if (mountedSessionsRef.current.length === 0) {
        return mountedSessionsRef.current;
      }
      mountedSessionsRef.current = [];
      return mountedSessionsRef.current;
    }

    const cachedSessionIds = new Set(cachedSessionOrder);
    cachedSessionIds.add(activeSession.id);
    const next = sessions.filter((session) => cachedSessionIds.has(session.id));
    const prev = mountedSessionsRef.current;
    if (
      next.length === prev.length &&
      next.every((session, index) => session === prev[index])
    ) {
      return prev;
    }
    mountedSessionsRef.current = next;
    return next;
  }, [activeSession, cachedSessionOrder, sessions]);
  const sessionConversationSignature = useMemo(
    () =>
      pane.viewMode === "session" && activeSession
        ? buildSessionConversationSignature(activeSession)
        : "",
    [activeSession, pane.viewMode],
  );
  const isSessionBusy =
    activeSession?.status === "active" || activeSession?.status === "approval";
  const showWaitingIndicator =
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession) &&
    (activeSession?.status === "active" || (!isSessionBusy && isSending));
  const canFindInSession =
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession);
  const hasSessionFindQuery = canFindInSession && sessionFindQuery.trim().length > 0;
  const activeSessionFindSearchIndex = useMemo(
    () =>
      canFindInSession && hasSessionFindQuery && activeSession
        ? buildSessionSearchIndex(activeSession)
        : null,
    [activeSession, canFindInSession, hasSessionFindQuery],
  );
  const sessionSearchMatches = useMemo(
    () =>
      activeSessionFindSearchIndex
        ? buildSessionSearchMatchesFromIndex(activeSessionFindSearchIndex, sessionFindQuery)
        : [],
    [activeSessionFindSearchIndex, sessionFindQuery],
  );
  const sessionSearchMatchedItemKeys = useMemo(
    () => new Set(sessionSearchMatches.map((match) => match.itemKey)),
    [sessionSearchMatches],
  );
  const activeSessionSearchMatch =
    sessionSearchMatches.length > 0
      ? sessionSearchMatches[Math.min(sessionFindActiveIndex, sessionSearchMatches.length - 1)] ?? null
      : null;
  const activeSessionSearchMatchIndex = activeSessionSearchMatch
    ? Math.min(sessionFindActiveIndex, sessionSearchMatches.length - 1)
    : -1;
  const waitingIndicatorPrompt = useMemo(() => {
    if (!showWaitingIndicator || !activeSession || (!isSessionBusy && isSending)) {
      return null;
    }

    return findLastUserPrompt(activeSession);
  }, [activeSession, isSending, isSessionBusy, showWaitingIndicator]);
  const composerInputDisabled = !activeSession || isStopping;
  const composerSendDisabled = !activeSession || isSending || isStopping;
  const scrollStateKey = activeSourceTab
    ? `${pane.id}:source:${activeSourceTab.path ?? "empty"}`
    : activeFilesystemTab
      ? `${pane.id}:filesystem:${activeFilesystemTab.rootPath ?? "empty"}`
      : activeGitStatusTab
        ? `${pane.id}:gitStatus:${activeGitStatusTab.workdir ?? "empty"}`
        : activeInstructionDebuggerTab
          ? `${pane.id}:instructionDebugger:${activeInstructionDebuggerTab.originSessionId ?? activeInstructionDebuggerTab.workdir ?? "empty"}`
        : activeDiffPreviewTab
          ? `${pane.id}:diffPreview:${activeDiffPreviewTab.diffMessageId}`
        : `${pane.id}:${pane.viewMode}:${activeSession?.id ?? "empty"}`;
  const defaultScrollToBottom =
    pane.viewMode === "session" || pane.viewMode === "commands" || pane.viewMode === "diffs";
  const visibleMessages = useMemo(
    () =>
      pane.viewMode === "commands"
        ? commandMessages
        : pane.viewMode === "diffs"
          ? diffMessages
          : [],
    [commandMessages, diffMessages, pane.viewMode],
  );
  const visibleContentSignature = useMemo(
    () =>
      pane.viewMode === "session"
        ? sessionConversationSignature
        : buildMessageListSignature(visibleMessages),
    [pane.viewMode, sessionConversationSignature, visibleMessages],
  );
  const visibleLastMessageAuthor = useMemo(
    () =>
      pane.viewMode === "session"
        ? activeSession?.messages[activeSession.messages.length - 1]?.author
        : visibleMessages[visibleMessages.length - 1]?.author,
    [activeSession, pane.viewMode, visibleMessages],
  );
  const showNewResponseIndicator = Boolean(newResponseIndicatorByKey[scrollStateKey]);
  const paneScrollPositions =
    paneScrollPositionsRef.current[pane.id] ?? (paneScrollPositionsRef.current[pane.id] = {});
  const paneContentSignatures =
    paneContentSignaturesRef.current[pane.id] ?? (paneContentSignaturesRef.current[pane.id] = {});

  function getShouldStickToBottom() {
    return paneShouldStickToBottomRef.current[pane.id] ?? true;
  }

  function setShouldStickToBottom(nextValue: boolean) {
    paneShouldStickToBottomRef.current[pane.id] = nextValue;
  }

  function handleComposerPaste(event: ReactClipboardEvent<HTMLTextAreaElement>) {
    if (!activeSession) {
      return;
    }

    const imageFiles = collectClipboardImageFiles(event.clipboardData);
    if (imageFiles.length === 0) {
      return;
    }

    event.preventDefault();

    void createDraftAttachmentsFromFiles(imageFiles)
      .then(({ attachments, errors }) => {
        if (attachments.length > 0) {
          onDraftAttachmentsAdd(activeSession.id, attachments);
        }

        if (errors.length > 0) {
          onComposerError(errors[0]);
        } else {
          onComposerError(null);
        }
      })
      .catch((error) => {
        onComposerError(getErrorMessage(error));
      });
  }

  async function handleSourceFileSave(
    path: string,
    content: string,
    sessionId: string | null,
    projectId: string | null,
  ) {
    if (!sessionId && !projectId) {
      throw new Error("This file view is no longer associated with a live session or project.");
    }

    const response = await saveFile(path, content, {
      sessionId,
      projectId,
    });
    setFileState({
      status: "ready",
      path: response.path,
      content: response.content,
      error: null,
      language: response.language ?? null,
    });
  }

  function setNewResponseIndicator(key: string, visible: boolean) {
    setNewResponseIndicatorByKey((current) => {
      const isVisible = Boolean(current[key]);
      if (isVisible === visible) {
        return current;
      }

      const nextState = { ...current };
      if (visible) {
        nextState[key] = true;
      } else {
        delete nextState[key];
      }
      return nextState;
    });
  }

  function handleConversationSearchItemMount(itemKey: string, node: HTMLElement | null) {
    if (node) {
      sessionSearchItemRefsRef.current[itemKey] = node;
      return;
    }

    delete sessionSearchItemRefsRef.current[itemKey];
  }

  function focusSessionFindInput(selectAll = false) {
    window.requestAnimationFrame(() => {
      const input = sessionFindInputRef.current;
      if (!input) {
        return;
      }

      input.focus();
      if (selectAll) {
        input.select();
      }
    });
  }

  function openSessionFind(selectAll = true) {
    if (!canFindInSession) {
      return;
    }

    setIsSessionFindOpen(true);
    focusSessionFindInput(selectAll);
  }

  function closeSessionFind() {
    setIsSessionFindOpen(false);
    setSessionFindQuery("");
    setSessionFindActiveIndex(0);
    sessionSearchItemRefsRef.current = {};
  }

  function stepSessionFind(direction: -1 | 1) {
    if (sessionSearchMatches.length === 0) {
      return;
    }

    setSessionFindActiveIndex((current) => {
      const safeCurrent =
        current >= 0 && current < sessionSearchMatches.length ? current : 0;
      return (safeCurrent + direction + sessionSearchMatches.length) % sessionSearchMatches.length;
    });
  }

  function scrollToLatestMessage(behavior: ScrollBehavior) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    node.scrollTo({
      top: node.scrollHeight,
      behavior,
    });
    setShouldStickToBottom(true);
    paneScrollPositions[scrollStateKey] = {
      top: node.scrollHeight,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }

  function scrollMessageStackByPage(direction: -1 | 1) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const distance = Math.max(Math.round(node.clientHeight * 0.85), 160);
    node.scrollBy({
      top: distance * direction,
      behavior: "smooth",
    });
  }

  function scrollMessageStackToBoundary(boundary: "top" | "bottom") {
    if (boundary === "bottom") {
      scrollToLatestMessage("smooth");
      return;
    }

    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    node.scrollTo({
      top: 0,
      behavior: "auto",
    });
    setShouldStickToBottom(false);
    paneScrollPositions[scrollStateKey] = {
      top: 0,
      shouldStick: false,
    };
  }

  function handlePaneKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    if (event.defaultPrevented) {
      return;
    }

    const command = resolvePaneScrollCommand(
      {
        altKey: event.altKey,
        ctrlKey: event.ctrlKey,
        key: event.key,
        metaKey: event.metaKey,
        shiftKey: event.shiftKey,
      },
      event.target,
    );
    if (!command) {
      return;
    }

    event.preventDefault();
    if (command.kind === "boundary") {
      scrollMessageStackToBoundary(command.direction === "up" ? "top" : "bottom");
    } else {
      scrollMessageStackByPage(command.direction === "up" ? -1 : 1);
    }
  }

  function handleMessageStackWheel(event: ReactWheelEvent<HTMLElement>) {
    if (event.defaultPrevented || event.ctrlKey) {
      return;
    }

    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const deltaY = normalizeWheelDelta(event, node);
    if (Math.abs(deltaY) < 0.5) {
      return;
    }

    if (canNestedScrollableConsumeWheel(event.target, node, deltaY)) {
      return;
    }

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (maxScrollTop <= 0) {
      return;
    }

    const nextScrollTop = clamp(node.scrollTop + deltaY, 0, maxScrollTop);
    if (Math.abs(nextScrollTop - node.scrollTop) < 0.5) {
      return;
    }

    event.preventDefault();
    node.scrollTop = nextScrollTop;
  }

  useEffect(() => {
    if (canFindInSession) {
      return;
    }

    closeSessionFind();
  }, [canFindInSession]);

  useEffect(() => {
    closeSessionFind();
  }, [activeSession?.id]);

  useEffect(() => {
    setSessionFindActiveIndex(0);
  }, [sessionFindQuery]);

  useEffect(() => {
    if (!isActive || !canFindInSession) {
      return;
    }

    function handleWindowKeyDown(event: KeyboardEvent) {
      const key = event.key.toLowerCase();
      const hasPrimaryModifier = event.metaKey || event.ctrlKey;
      if (
        event.defaultPrevented ||
        key !== "f" ||
        !hasPrimaryModifier ||
        event.altKey ||
        event.shiftKey
      ) {
        return;
      }

      event.preventDefault();
      openSessionFind();
    }

    window.addEventListener("keydown", handleWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", handleWindowKeyDown);
    };
  }, [canFindInSession, isActive]);

  function scheduleSettledScrollToBottom(
    behavior: ScrollBehavior,
    options: {
      maxAttempts?: number;
      onComplete?: () => void;
    } = {},
  ) {
    let frameId = 0;
    let cancelled = false;
    let completed = false;
    let remainingAttempts = options.maxAttempts ?? 12;
    let previousScrollHeight = -1;
    let stableFrameCount = 0;

    function complete() {
      if (cancelled || completed) {
        return;
      }

      completed = true;
      options.onComplete?.();
    }

    const tick = () => {
      const node = messageStackRef.current;
      if (!node) {
        remainingAttempts -= 1;
        if (remainingAttempts > 0) {
          frameId = window.requestAnimationFrame(tick);
        } else {
          complete();
        }
        return;
      }

      scrollToLatestMessage(behavior);

      const bottomGap = Math.max(node.scrollHeight - node.clientHeight - node.scrollTop, 0);
      const heightStable = node.scrollHeight === previousScrollHeight;
      if (bottomGap <= 4 && heightStable) {
        stableFrameCount += 1;
      } else {
        stableFrameCount = 0;
      }

      previousScrollHeight = node.scrollHeight;
      remainingAttempts -= 1;
      if (remainingAttempts > 0 && stableFrameCount < 2) {
        frameId = window.requestAnimationFrame(tick);
      } else {
        complete();
      }
    };

    frameId = window.requestAnimationFrame(tick);
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(frameId);
    };
  }

  useLayoutEffect(() => {
    const frameId = window.requestAnimationFrame(() => {
      const node = messageStackRef.current;
      if (!node) {
        return;
      }

      const saved = paneScrollPositions[scrollStateKey];
      if (saved) {
        const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
        node.scrollTop = Math.min(saved.top, maxScrollTop);
        setShouldStickToBottom(saved.shouldStick);
        return;
      }

      if (defaultScrollToBottom) {
        node.scrollTop = node.scrollHeight;
        setShouldStickToBottom(true);
        paneScrollPositions[scrollStateKey] = {
          top: node.scrollTop,
          shouldStick: true,
        };
        return;
      }

      node.scrollTop = 0;
      setShouldStickToBottom(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [defaultScrollToBottom, scrollStateKey]);

  useLayoutEffect(() => {
    if (!hasSessionFindQuery || !activeSessionSearchMatch) {
      return;
    }

    const node = sessionSearchItemRefsRef.current[activeSessionSearchMatch.itemKey];
    if (!node) {
      return;
    }

    setShouldStickToBottom(false);
    node.scrollIntoView({
      block: "center",
      behavior: "auto",
    });

    const container = messageStackRef.current;
    if (!container) {
      return;
    }

    paneScrollPositions[scrollStateKey] = {
      top: container.scrollTop,
      shouldStick: false,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }, [activeSessionSearchMatch, hasSessionFindQuery, paneScrollPositions, scrollStateKey]);

  useLayoutEffect(() => {
    if (
      !activeSession ||
      !isSessionTabActive ||
      pane.viewMode !== "session" ||
      visitedSessionIds[activeSession.id]
    ) {
      return;
    }

    return scheduleSettledScrollToBottom("auto");
  }, [activeSession, isSessionTabActive, pane.viewMode, scrollStateKey, visitedSessionIds]);

  useEffect(() => {
    if (!activeSession?.id) {
      return;
    }

    setVisitedSessionIds((current) =>
      current[activeSession.id]
        ? current
        : {
            ...current,
            [activeSession.id]: true,
          },
    );
    setCachedSessionOrder((current) => {
      const nextOrder = [activeSession.id, ...current.filter((sessionId) => sessionId !== activeSession.id)].slice(
        0,
        MAX_CACHED_SESSION_PAGES_PER_PANE,
      );

      if (
        nextOrder.length === current.length &&
        nextOrder.every((sessionId, index) => sessionId === current[index])
      ) {
        return current;
      }

      return nextOrder;
    });
  }, [activeSession?.id]);

  useEffect(() => {
    const availableSessionIds = new Set(sessions.map((session) => session.id));
    setVisitedSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setCachedSessionOrder((current) => {
      const nextOrder = current.filter((sessionId) => availableSessionIds.has(sessionId));
      if (
        nextOrder.length === current.length &&
        nextOrder.every((sessionId, index) => sessionId === current[index])
      ) {
        return current;
      }

      return nextOrder;
    });
  }, [sessions]);

  useEffect(() => {
    if (!activeSession || !isSessionTabActive) {
      return;
    }

    const previousSignature = paneContentSignatures[scrollStateKey];
    paneContentSignatures[scrollStateKey] = visibleContentSignature;
    if (previousSignature === undefined || previousSignature === visibleContentSignature) {
      return;
    }

    if (hasSessionFindQuery) {
      setShouldStickToBottom(false);
      if (pane.viewMode === "session" && visibleLastMessageAuthor === "assistant") {
        setNewResponseIndicator(scrollStateKey, true);
      }
      return;
    }

    const shouldScroll =
      getShouldStickToBottom() ||
      paneScrollPositions[scrollStateKey]?.shouldStick === true ||
      visibleLastMessageAuthor === "you";
    if (!shouldScroll) {
      if (pane.viewMode === "session" && visibleLastMessageAuthor === "assistant") {
        setNewResponseIndicator(scrollStateKey, true);
      }
      return;
    }

    const behavior = visibleLastMessageAuthor === "you" ? "smooth" : "auto";
    setNewResponseIndicator(scrollStateKey, false);
    const frameId = window.requestAnimationFrame(() => {
      scrollToLatestMessage(behavior);
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [
    activeSession,
    hasSessionFindQuery,
    isSessionTabActive,
    pane.viewMode,
    scrollStateKey,
    visibleContentSignature,
    visibleLastMessageAuthor,
  ]);

  useEffect(() => {
    if (
      !pendingScrollToBottomRequest ||
      !isActive ||
      pane.viewMode !== "session" ||
      activeSession?.id !== pendingScrollToBottomRequest.sessionId
    ) {
      return;
    }

    const requestToken = pendingScrollToBottomRequest.token;
    return scheduleSettledScrollToBottom("auto", {
      onComplete: () => {
        onScrollToBottomRequestHandled(requestToken);
      },
    });
  }, [
    activeSession?.id,
    isActive,
    onScrollToBottomRequestHandled,
    pane.viewMode,
    pendingScrollToBottomRequest,
    scrollStateKey,
  ]);

  useEffect(() => {
    setSourceDraft(pane.sourcePath ?? "");
  }, [pane.id, pane.sourcePath]);

  useEffect(() => {
    if (!isSending || pane.viewMode !== "session") {
      return;
    }

    const frameId = window.requestAnimationFrame(() => {
      scrollToLatestMessage("smooth");
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [isSending, pane.viewMode, scrollStateKey]);

  useEffect(() => {
    if (pane.viewMode !== "source") {
      return;
    }

    if (!pane.sourcePath && sourceCandidatePaths[0]) {
      onPaneSourcePathChange(pane.id, sourceCandidatePaths[0]);
    }
  }, [onPaneSourcePathChange, pane.id, pane.sourcePath, pane.viewMode, sourceCandidatePaths]);

  useEffect(() => {
    let cancelled = false;

    async function loadFile(path: string) {
      if (!activeSourceOriginSessionId && !activeSourceOriginProjectId) {
        setFileState({
          status: "error",
          path,
          content: "",
          error: "This file view is no longer associated with a live session or project.",
          language: null,
        });
        return;
      }

      setFileState({
        status: "loading",
        path,
        content: "",
        error: null,
        language: null,
      });

      try {
        const response = await fetchFile(path, {
          sessionId: activeSourceOriginSessionId,
          projectId: activeSourceOriginProjectId,
        });
        if (cancelled) {
          return;
        }

        setFileState({
          status: "ready",
          path: response.path,
          content: response.content,
          error: null,
          language: response.language ?? null,
        });
      } catch (error) {
        if (cancelled) {
          return;
        }

        setFileState({
          status: "error",
          path,
          content: "",
          error: getErrorMessage(error),
          language: null,
        });
      }
    }

    if (pane.viewMode === "source" && pane.sourcePath) {
      void loadFile(pane.sourcePath);
    } else if (pane.viewMode === "source") {
      setFileState({
        status: "idle",
        path: "",
        content: "",
        error: null,
        language: null,
      });
    }

    return () => {
      cancelled = true;
    };
  }, [activeSourceOriginProjectId, activeSourceOriginSessionId, pane.sourcePath, pane.viewMode]);

  useEffect(() => {
    if (!showDropOverlay) {
      setActiveDropPlacement(null);
    }
  }, [showDropOverlay]);

  return (
    <section
      className={`workspace-pane thread panel ${isActive ? "active" : ""}`}
      onMouseDown={() => {
        if (!isActive) {
          onActivatePane(pane.id);
        }
      }}
      onKeyDown={handlePaneKeyDown}
    >
      {showDropOverlay ? (
        <div className="pane-drop-overlay">
          {allowedDropPlacements.map((placement) => (
            <div
              key={placement}
              className={`pane-drop-zone pane-drop-zone-${placement} ${activeDropPlacement === placement ? "active" : ""}`}
              onDragEnter={() => {
                setActiveDropPlacement(placement);
              }}
              onDragOver={(event) => {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
                if (activeDropPlacement !== placement) {
                  setActiveDropPlacement(placement);
                }
              }}
              onDragLeave={() => {
                setActiveDropPlacement((current) => (current === placement ? null : current));
              }}
              onDrop={(event) => {
                event.preventDefault();
                setActiveDropPlacement(null);
                onTabDrop(pane.id, placement);
              }}
            >
              <span>{dropLabelForPlacement(placement)}</span>
            </div>
          ))}
        </div>
      ) : null}

      <div className="pane-top">
        <div className="pane-bar">
          <div className="pane-bar-left">
            <PaneTabs
              paneId={pane.id}
              windowId={windowId}
              tabs={pane.tabs}
              activeTabId={activeTab?.id ?? null}
              codexState={codexState}
              projectLookup={projectLookup}
              sessionLookup={sessionLookup}
              draggedTab={draggedTab}
              onSelectTab={onSelectTab}
              onCloseTab={onCloseTab}
              onTabDragStart={onTabDragStart}
              onTabDragEnd={onTabDragEnd}
              onTabDrop={onTabDrop}
              onRenameSessionRequest={onRenameSessionRequest}
            />
          </div>
          {activeControlPanelTab ? (
            <div className="pane-bar-right">
              <BackendConnectionStatus state={backendConnectionState} />
            </div>
          ) : null}
        </div>

        <div className="pane-view-strip">
          {activeTab?.kind === "session" ? (
            <div className="pane-view-strip-left">
              {(["session", "prompt", "commands", "diffs"] as SessionPaneViewMode[]).map((viewMode) => (
                <button
                  key={viewMode}
                  className={`pane-view-button ${pane.viewMode === viewMode ? "selected" : ""}`}
                  type="button"
                  onClick={() => onPaneViewModeChange(pane.id, viewMode)}
                >
                  {labelForPaneViewMode(viewMode)}
                </button>
              ))}
              <button
                className="pane-view-button"
                type="button"
                onClick={() => {
                  const candidatePath = activeSession
                    ? collectCandidateSourcePaths(activeSession)[0] ?? null
                    : null;
                  onOpenSourceTab(pane.id, candidatePath, activeSession?.id ?? null, activeSession?.projectId ?? null);
                }}
              >
                File
              </button>
              <button
                className="pane-view-button"
                type="button"
                onClick={() =>
                  onOpenFilesystemTab(
                    pane.id,
                    activeSession?.workdir ?? null,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
              >
                Files
              </button>
              <button
                className="pane-view-button"
                type="button"
                onClick={() =>
                  onOpenGitStatusTab(
                    pane.id,
                    activeSession?.workdir ?? null,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
                >
                  Git
                </button>
                <button
                  className="pane-view-button"
                  type="button"
                  onClick={() =>
                    onOpenInstructionDebuggerTab(
                      pane.id,
                      activeSession?.workdir ?? null,
                      activeSession?.id ?? null,
                      activeSession?.projectId ?? null,
                    )
                  }
                >
                  Instructions
                </button>
                {canFindInSession ? (
                  <button
                    className={`pane-view-button${isSessionFindOpen ? " selected" : ""}`}
                    type="button"
                    onClick={() => openSessionFind(!isSessionFindOpen)}
                    title={`Find in session (${primaryModifierLabel()}+F)`}
                  >
                    Find
                  </button>
                ) : null}
            </div>
          ) : null}
          <div className="pane-view-strip-right">
            {canFindInSession && isSessionFindOpen ? (
              <SessionFindBar
                inputRef={sessionFindInputRef}
                query={sessionFindQuery}
                activeIndex={activeSessionSearchMatchIndex}
                matches={sessionSearchMatches}
                onChange={(nextValue) => setSessionFindQuery(nextValue)}
                onNext={() => stepSessionFind(1)}
                onPrevious={() => stepSessionFind(-1)}
                onClose={closeSessionFind}
              />
            ) : null}
          </div>
        </div>
      </div>

      <section
        ref={messageStackRef}
        className={`message-stack${activeControlPanelTab ? " control-panel-stack" : ""}${activeSourceTab || activeDiffPreviewTab ? " editor-panel-stack" : ""}`}
        onWheel={handleMessageStackWheel}
        onScroll={(event) => {
          const node = event.currentTarget;
          const shouldStick = node.scrollHeight - node.scrollTop - node.clientHeight < 72;
          setShouldStickToBottom(shouldStick);
          paneScrollPositions[scrollStateKey] = {
            top: node.scrollTop,
            shouldStick,
          };
          if (shouldStick) {
            setNewResponseIndicator(scrollStateKey, false);
          }
        }}
      >
        {activeControlPanelTab ? (
          renderControlPanel(pane.id)
        ) : activeSourceTab ? (
          <SourcePanel
            candidatePaths={sourceCandidatePaths}
            editorAppearance={editorAppearance}
            editorFontSizePx={editorFontSizePx}
            fileState={fileState}
            sourceDraft={sourceDraft}
            sourceFocus={
              activeSourceTab?.focusLineNumber
                ? {
                    line: activeSourceTab.focusLineNumber,
                    column: activeSourceTab.focusColumnNumber ?? null,
                    token: activeSourceTab.focusToken ?? null,
                  }
                : null
            }
            sourcePath={activeSourceTab.path}
            onDraftChange={setSourceDraft}
            onOpenInstructionDebugger={
              activeSourceOriginSessionId
                ? () =>
                    onOpenInstructionDebuggerTab(
                      pane.id,
                      sessionLookup.get(activeSourceOriginSessionId)?.workdir ?? null,
                      activeSourceOriginSessionId,
                      activeSourceOriginProjectId,
                    )
                : null
            }
            onOpenPath={(path) => onPaneSourcePathChange(pane.id, path)}
            onSaveFile={(path, content) =>
              handleSourceFileSave(path, content, activeSourceOriginSessionId, activeSourceOriginProjectId)
            }
          />
        ) : activeFilesystemTab ? (
          <FileSystemPanel
            rootPath={activeFilesystemTab.rootPath}
            sessionId={activeFilesystemOriginSessionId}
            projectId={activeFilesystemOriginProjectId}
            onOpenPath={(path, options) =>
              onOpenSourceTab(
                pane.id,
                path,
                activeFilesystemOriginSessionId,
                activeFilesystemOriginProjectId,
                options,
              )
            }
            onOpenRootPath={(path) =>
              onOpenFilesystemTab(
                pane.id,
                path,
                activeFilesystemOriginSessionId,
                activeFilesystemOriginProjectId,
              )
            }
          />
        ) : activeGitStatusTab ? (
          <GitStatusPanel
            sessionId={activeGitStatusOriginSessionId}
            workdir={activeGitStatusTab.workdir}
            onOpenDiff={(diff, options) =>
              onOpenGitStatusDiffPreviewTab(
                pane.id,
                diff,
                activeGitStatusOriginSessionId,
                activeGitStatusOriginProjectId,
                options,
              )
            }
            onOpenWorkdir={(path) =>
              onOpenGitStatusTab(pane.id, path, activeGitStatusOriginSessionId, activeGitStatusOriginProjectId)
            }
          />
        ) : activeInstructionDebuggerTab ? (
          <InstructionDebuggerPanel
            session={activeInstructionDebuggerSession}
            workdir={activeInstructionDebuggerTab.workdir}
            onOpenPath={(path, options) =>
              onOpenSourceTab(
                pane.id,
                path,
                activeInstructionDebuggerOriginSessionId,
                activeInstructionDebuggerOriginProjectId,
                options,
              )
            }
          />
        ) : activeDiffPreviewTab ? (
          <DiffPanel
            appearance={editorAppearance}
            changeType={activeDiffPreviewTab.changeType}
            changeSetId={activeDiffPreviewTab.changeSetId ?? null}
            fontSizePx={editorFontSizePx}
            diff={activeDiffPreviewTab.diff}
            diffMessageId={activeDiffPreviewTab.diffMessageId}
            filePath={activeDiffPreviewTab.filePath}
            gitSectionId={activeDiffPreviewTab.gitSectionId ?? null}
            language={activeDiffPreviewTab.language ?? null}
            sessionId={activeDiffOriginSessionId}
            projectId={activeDiffOriginProjectId}
            originAgentName={
              activeDiffOriginSessionId
                ? (sessionLookup.get(activeDiffOriginSessionId)?.agent ?? null)
                : null
            }
            workspaceRoot={activeDiffWorkspaceRoot}
            onOpenPath={(path) =>
              onOpenSourceTab(pane.id, path, activeDiffOriginSessionId, activeDiffOriginProjectId)
            }
            onInsertReviewIntoPrompt={
              activeDiffOriginSessionId
                ? (_reviewFilePath, prompt) =>
                    onInsertReviewIntoPrompt(activeDiffOriginSessionId, pane.id, prompt)
                : undefined
            }
            onOpenConversation={
              activeDiffOriginSessionId
                ? () => onOpenConversationFromDiff(activeDiffOriginSessionId, pane.id)
                : undefined
            }
            onSaveFile={(path, content) =>
              handleSourceFileSave(path, content, activeDiffOriginSessionId, activeDiffOriginProjectId)
            }
            summary={activeDiffPreviewTab.summary}
          />
        ) : (
          <AgentSessionPanel
            paneId={pane.id}
            viewMode={pane.viewMode}
            scrollContainerRef={messageStackRef}
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
            conversationSearchQuery={hasSessionFindQuery ? sessionFindQuery : ""}
            conversationSearchMatchedItemKeys={sessionSearchMatchedItemKeys}
            conversationSearchActiveItemKey={activeSessionSearchMatch?.itemKey ?? null}
            onConversationSearchItemMount={handleConversationSearchItemMount}
            renderCommandCard={(message) => <CommandCard message={message} />}
            renderDiffCard={(message) => (
              <DiffCard
                message={message}
                onOpenPreview={() =>
                  onOpenDiffPreviewTab(pane.id, message, activeSession?.id ?? null, activeSession?.projectId ?? null)
                }
                workspaceRoot={activeSession?.workdir ?? null}
              />
            )}
            renderMessageCard={(
              message,
              preferImmediateHeavyRender,
              handleDecision,
              handleUserInput,
              handleMcpElicitation,
              handleCodexAppRequest,
            ) => (
              <MessageCard
                message={message}
                onOpenDiffPreview={(diffMessage) =>
                  onOpenDiffPreviewTab(pane.id, diffMessage, activeSession?.id ?? null, activeSession?.projectId ?? null)
                }
                onOpenSourceLink={(target) =>
                  onOpenSourceTab(
                    pane.id,
                    target.path,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                    {
                      line: target.line,
                      column: target.column,
                      openInNewTab: target.openInNewTab,
                    },
                  )
                }
                preferImmediateHeavyRender={preferImmediateHeavyRender}
                onApprovalDecision={handleDecision}
                onUserInputSubmit={handleUserInput}
                onMcpElicitationSubmit={handleMcpElicitation}
                onCodexAppRequestSubmit={handleCodexAppRequest}
                searchQuery={
                  activeSessionSearchMatch?.itemKey === `message:${message.id}` ? sessionFindQuery : ""
                }
                searchHighlightTone={
                  activeSessionSearchMatch?.itemKey === `message:${message.id}` ? "active" : "match"
                }
                workspaceRoot={activeSession?.workdir ?? null}
              />
            )}
            renderPromptSettings={(panelPaneId, session, panelIsUpdating, handleSettingsChange) => {
              if (session.agent === "Codex") {
                return (
                  <CodexPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    sessionNotice={session.id === activeSession?.id ? sessionSettingNotice : null}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onArchiveThread={onArchiveCodexThread}
                    onCompactThread={onCompactCodexThread}
                    onForkThread={onForkCodexThread}
                    onRollbackThread={onRollbackCodexThread}
                    onSessionSettingsChange={handleSettingsChange}
                    onUnarchiveThread={onUnarchiveCodexThread}
                  />
                );
              }

              if (session.agent === "Claude") {
                return (
                  <ClaudePromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              if (session.agent === "Cursor") {
                return (
                  <CursorPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              if (session.agent === "Gemini") {
                return (
                  <GeminiPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              return null;
            }}
          />
        )}
      </section>
      {activeControlPanelTab || activeSourceTab || activeFilesystemTab || activeGitStatusTab || activeInstructionDebuggerTab || activeDiffPreviewTab ? null : (
        <AgentSessionPanelFooter
          paneId={pane.id}
          viewMode={pane.viewMode}
          isPaneActive={isActive}
          activeSession={activeSession}
          committedDraft={draft}
          draftAttachments={draftAttachments}
          formatByteSize={formatByteSize}
          isSending={isSending}
          isStopping={isStopping}
          isSessionBusy={isSessionBusy}
          isUpdating={isUpdating}
          showNewResponseIndicator={showNewResponseIndicator}
          footerModeLabel={labelForPaneViewMode(pane.viewMode)}
          onScrollToLatest={() => scrollToLatestMessage("smooth")}
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
          onPaste={handleComposerPaste}
        />
      )}
    </section>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function workspaceNodeContainsControlPanel(
  node: WorkspaceNode,
  paneLookup: Map<string, WorkspacePane>,
): boolean {
  if (node.type === "pane") {
    return paneLookup.get(node.paneId)?.tabs.some((tab) => tab.kind === "controlPanel") ?? false;
  }

  return (
    workspaceNodeContainsControlPanel(node.first, paneLookup) ||
    workspaceNodeContainsControlPanel(node.second, paneLookup)
  );
}

function findWorkspaceSplitNode(
  node: WorkspaceNode | null,
  splitId: string,
): Extract<WorkspaceNode, { type: "split" }> | null {
  if (!node || node.type === "pane") {
    return null;
  }

  if (node.id === splitId) {
    return node;
  }

  return (
    findWorkspaceSplitNode(node.first, splitId) ?? findWorkspaceSplitNode(node.second, splitId)
  );
}

function resolveRootCssLengthPx(cssVariableName: string, fallbackPx: number): number {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return fallbackPx;
  }

  const rootStyle = window.getComputedStyle(document.documentElement);
  const rawValue = rootStyle.getPropertyValue(cssVariableName).trim();
  if (!rawValue) {
    return fallbackPx;
  }

  const numericValue = Number.parseFloat(rawValue);
  if (!Number.isFinite(numericValue)) {
    return fallbackPx;
  }

  if (rawValue.endsWith("rem")) {
    const rootFontSizePx = Number.parseFloat(rootStyle.fontSize);
    return numericValue * (Number.isFinite(rootFontSizePx) ? rootFontSizePx : 16);
  }

  return numericValue;
}

export function getWorkspaceSplitResizeBounds(
  root: WorkspaceNode | null,
  splitId: string,
  direction: "row" | "column",
  size: number,
  paneLookup: Map<string, WorkspacePane>,
): { minRatio: number; maxRatio: number } {
  if (direction !== "row" || size <= 0) {
    return {
      minRatio: DEFAULT_SPLIT_MIN_RATIO,
      maxRatio: DEFAULT_SPLIT_MAX_RATIO,
    };
  }

  const splitNode = findWorkspaceSplitNode(root, splitId);
  if (!splitNode) {
    return {
      minRatio: DEFAULT_SPLIT_MIN_RATIO,
      maxRatio: DEFAULT_SPLIT_MAX_RATIO,
    };
  }

  const controlPanelMinRatio = clamp(
    resolveRootCssLengthPx(
      "--control-panel-pane-min-width",
      CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX,
    ) / size,
    0,
    1,
  );
  const firstMinRatio = workspaceNodeContainsControlPanel(splitNode.first, paneLookup)
    ? controlPanelMinRatio
    : DEFAULT_SPLIT_MIN_RATIO;
  const secondMinRatio = workspaceNodeContainsControlPanel(splitNode.second, paneLookup)
    ? controlPanelMinRatio
    : DEFAULT_SPLIT_MIN_RATIO;
  const minRatio = firstMinRatio;
  const maxRatio = 1 - secondMinRatio;

  if (minRatio <= maxRatio) {
    return {
      minRatio,
      maxRatio,
    };
  }

  const constrainedRatio = clamp(
    firstMinRatio / Math.max(firstMinRatio + secondMinRatio, Number.EPSILON),
    0,
    1,
  );

  return {
    minRatio: constrainedRatio,
    maxRatio: constrainedRatio,
  };
}

function SessionFindBar({
  inputRef,
  query,
  activeIndex,
  matches,
  onChange,
  onNext,
  onPrevious,
  onClose,
}: {
  inputRef: RefObject<HTMLInputElement>;
  query: string;
  activeIndex: number;
  matches: SessionSearchMatch[];
  onChange: (nextValue: string) => void;
  onNext: () => void;
  onPrevious: () => void;
  onClose: () => void;
}) {
  const hasQuery = query.trim().length > 0;
  const hasMatches = matches.length > 0;
  const currentMatch = hasMatches && activeIndex >= 0 ? matches[activeIndex] ?? null : null;
  const countLabel = !hasQuery
    ? "Type to search"
    : hasMatches
      ? `${activeIndex + 1} of ${matches.length}`
      : "No matches";

  return (
    <div className="session-find-bar" role="search" aria-label="Find in session">
      <input
        ref={inputRef}
        className="session-find-input"
        type="search"
        value={query}
        placeholder="Find in session"
        spellCheck={false}
        onChange={(event) => onChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            if (event.shiftKey) {
              onPrevious();
            } else {
              onNext();
            }
            return;
          }

          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
      />
      <span
        className="session-find-count"
        aria-live="polite"
        title={currentMatch?.snippet ?? undefined}
      >
        {countLabel}
      </span>
      <button
        className="session-find-button"
        type="button"
        onClick={onPrevious}
        disabled={!hasMatches}
      >
        Prev
      </button>
      <button
        className="session-find-button"
        type="button"
        onClick={onNext}
        disabled={!hasMatches}
      >
        Next
      </button>
      <button className="session-find-button session-find-close" type="button" onClick={onClose}>
        Close
      </button>
    </div>
  );
}

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
              : "Start the Claude session once to load its live model list. New Claude sessions begin on Sonnet, and you can still paste a full Claude model id manually."}
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

type MarkdownFileLinkTarget = {
  path: string;
  line?: number;
  column?: number;
  openInNewTab?: boolean;
};

export const MessageCard = memo(function MessageCard({
  message,
  onOpenDiffPreview,
  onOpenSourceLink,
  preferImmediateHeavyRender = false,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit = () => {},
  onCodexAppRequestSubmit = () => {},
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: Message;
  onOpenDiffPreview?: (message: DiffMessage) => void;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateHeavyRender?: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: (messageId: string, answers: Record<string, string[]>) => void;
  onMcpElicitationSubmit?: (
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  onCodexAppRequestSubmit?: (messageId: string, result: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  switch (message.type) {
    case "text": {
      const connectionRetryNotice =
        message.author === "assistant" ? parseConnectionRetryNotice(message.text) : null;
      const commandLabel =
        message.author === "you"
          ? promptCommandMetaLabel(message.text, message.expandedText)
          : null;

      if (connectionRetryNotice) {
        return (
          <ConnectionRetryCard
            message={message}
            notice={connectionRetryNotice}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
          />
        );
      }

      return (
        <article className={`message-card bubble bubble-${message.author}`}>
          <MessageMeta
            author={message.author}
            timestamp={message.timestamp}
            trailing={
              commandLabel ? <span className="message-meta-tag">{commandLabel}</span> : undefined
            }
          />
          {message.attachments && message.attachments.length > 0 ? (
            <MessageAttachmentList
              attachments={message.attachments}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : null}
          {message.author === "assistant" ? (
            preferImmediateHeavyRender ? (
              <MarkdownContent
                markdown={message.text}
                onOpenSourceLink={onOpenSourceLink}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
                workspaceRoot={workspaceRoot}
              />
            ) : (
              <DeferredMarkdownContent
                markdown={message.text}
                onOpenSourceLink={onOpenSourceLink}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
                workspaceRoot={workspaceRoot}
              />
            )
          ) : message.text ? (
            <>
              <p className="plain-text-copy">
                {renderHighlightedText(message.text, searchQuery, searchHighlightTone)}
              </p>
              {message.expandedText ? (
                <ExpandedPromptPanel
                  expandedText={message.expandedText}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : null}
            </>
          ) : (
            <p className="support-copy">{imageAttachmentSummaryLabel(message.attachments?.length ?? 0)}</p>
          )}
        </article>
      );
    }
    case "thinking":
      return (
        <ThinkingCard
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "command":
      return (
        <CommandCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "diff":
      return (
        <DiffCard
          message={message}
          onOpenPreview={() => onOpenDiffPreview?.(message)}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "markdown":
      return (
        <MarkdownCard
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "parallelAgents":
      return (
        <ParallelAgentsCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "subagentResult":
      return (
        <SubagentResultCard
          message={message}
          onOpenSourceLink={onOpenSourceLink}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
          workspaceRoot={workspaceRoot}
        />
      );
    case "approval":
      return (
        <ApprovalCard
          message={message}
          onApprovalDecision={onApprovalDecision}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "userInputRequest":
      return (
        <UserInputRequestCard
          message={message}
          onSubmit={onUserInputSubmit}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "mcpElicitationRequest":
      return (
        <McpElicitationRequestCard
          message={message}
          onSubmit={onMcpElicitationSubmit}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "codexAppRequest":
      return (
        <CodexAppRequestCard
          message={message}
          onSubmit={onCodexAppRequestSubmit}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    default:
      return null;
  }
}, (previous, next) =>
  previous.message === next.message &&
  previous.onOpenDiffPreview === next.onOpenDiffPreview &&
  previous.onOpenSourceLink === next.onOpenSourceLink &&
  previous.preferImmediateHeavyRender === next.preferImmediateHeavyRender &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone &&
  previous.workspaceRoot === next.workspaceRoot
);

function promptCommandMetaLabel(text: string, expandedText?: string | null) {
  return expandedText && text.trim().startsWith("/") ? "Command" : null;
}

function ConnectionRetryCard({
  message,
  notice,
  searchQuery,
  searchHighlightTone,
}: {
  message: TextMessage;
  notice: ConnectionRetryNotice;
  searchQuery: string;
  searchHighlightTone: SearchHighlightTone;
}) {
  return (
    <article className="message-card connection-notice-card" role="status" aria-live="polite">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="connection-notice-body">
        <div className="activity-spinner connection-notice-spinner" aria-hidden="true" />
        <div className="connection-notice-copy">
          <div className="card-label">Connection</div>
          <div className="connection-notice-heading">
            <h3>Reconnecting to continue this turn</h3>
            {notice.attemptLabel ? (
              <span className="chip chip-status chip-status-active">{notice.attemptLabel}</span>
            ) : null}
          </div>
          <p className="connection-notice-detail">
            {renderHighlightedText(notice.detail, searchQuery, searchHighlightTone)}
          </p>
        </div>
      </div>
    </article>
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
        <div key={`${attachment.fileName}-${attachment.byteSize}-${index}`} className="message-attachment-chip">
          <strong className="message-attachment-name">
            {renderHighlightedText(attachment.fileName, searchQuery, searchHighlightTone)}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} {"\u00b7"}{" "}
            {renderHighlightedText(attachment.mediaType, searchQuery, searchHighlightTone)}
          </span>
        </div>
      ))}
    </div>
  );
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
  return (
    <div className="message-meta">
      <span>{author === "you" ? "You" : "Agent"}</span>
      <span className="message-meta-end">
        {trailing}
        <span>{timestamp}</span>
      </span>
    </div>
  );
}

function DeferredHeavyContent({
  children,
  estimatedHeight,
  placeholder,
}: {
  children: ReactNode;
  estimatedHeight: number;
  placeholder: ReactNode;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [isActivated, setIsActivated] = useState(false);

  useLayoutEffect(() => {
    if (isActivated) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    if (isElementNearRenderViewport(node, root, DEFERRED_RENDER_ROOT_MARGIN_PX)) {
      setIsActivated(true);
    }
  }, [isActivated]);

  useEffect(() => {
    if (isActivated) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    if (typeof window === "undefined" || typeof window.IntersectionObserver === "undefined") {
      setIsActivated(true);
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    const observer = new window.IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting || entry.intersectionRatio > 0)) {
          setIsActivated(true);
        }
      },
      {
        root,
        rootMargin: `${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px ${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px`,
        threshold: 0.01,
      },
    );

    observer.observe(node);
    return () => {
      observer.disconnect();
    };
  }, [isActivated]);

  return (
    <div
      ref={containerRef}
      className="deferred-heavy-content"
      style={
        isActivated
          ? undefined
          : ({ "--deferred-min-height": `${estimatedHeight}px` } as CSSProperties)
      }
    >
      {isActivated ? children : placeholder}
    </div>
  );
}

function DeferredHighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
  searchQuery,
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const metrics = useMemo(() => measureTextBlock(code), [code]);
  const showSearchHighlight = containsSearchMatch(code, searchQuery ?? "");
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_CODE_LINE_THRESHOLD || code.length >= HEAVY_CODE_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    );
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateCodeBlockHeight(metrics.lineCount)}
      placeholder={
        <pre className={`${className} syntax-block deferred-code-placeholder`}>
          <code>{buildDeferredPreviewText(code)}</code>
        </pre>
      }
    >
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    </DeferredHeavyContent>
  );
}

function DeferredMarkdownContent({
  markdown,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const metrics = useMemo(() => measureTextBlock(markdown), [markdown]);
  const showSearchHighlight = containsSearchMatch(markdown, searchQuery);
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_MARKDOWN_LINE_THRESHOLD ||
      markdown.length >= HEAVY_MARKDOWN_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    );
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateMarkdownBlockHeight(metrics.lineCount)}
      placeholder={
        <div className="markdown-copy deferred-markdown-placeholder">
          <p className="plain-text-copy">{buildMarkdownPreviewText(markdown)}</p>
        </div>
      }
    >
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </DeferredHeavyContent>
  );
}

function HighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
  showCopyButton = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
  showCopyButton?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [copied, setCopied] = useState(false);
  const showSearchHighlight = containsSearchMatch(code, searchQuery);
  const highlighted = useMemo(
    () =>
      highlightCode(code, {
        commandHint,
        language,
        pathHint,
      }),
    [code, commandHint, language, pathHint],
  );

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(code);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <pre className={`${className} syntax-block${showCopyButton ? " copyable" : ""}`}>
      {showCopyButton ? (
        <button
          className={`command-icon-button syntax-copy-button${copied ? " copied" : ""}`}
          type="button"
          onMouseDown={(event) => {
            event.preventDefault();
            event.stopPropagation();
          }}
          onClick={() => void handleCopy()}
          aria-label={copied ? "Code copied" : "Copy code"}
          title={copied ? "Copied" : "Copy code"}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </button>
      ) : null}
      <code className={`hljs${highlighted.language ? ` language-${highlighted.language}` : ""}`}>
        {showSearchHighlight
          ? renderHighlightedText(code, searchQuery, searchHighlightTone)
          : (
            <span dangerouslySetInnerHTML={{ __html: highlighted.html }} />
          )}
      </code>
    </pre>
  );
}

function ThinkingCard({
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: ThinkingMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const markdown = message.lines.join("\n");

  return (
    <article className="message-card reasoning-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Thinking</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredMarkdownContent
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </article>
  );
}

function CommandCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CommandMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [inputExpanded, setInputExpanded] = useState(false);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const [copiedSection, setCopiedSection] = useState<"command" | "output" | null>(null);
  const hasOutput = message.output.trim().length > 0;
  const displayOutput = hasOutput
    ? message.output
    : message.status === "running"
      ? "Awaiting output\u2026"
      : "No output";
  const canExpandCommand =
    message.command.split("\n").length > 10 || message.command.length > 480;
  const canExpandOutput =
    hasOutput && (message.output.split("\n").length > 10 || message.output.length > 480);
  const statusTone = mapCommandStatus(message.status);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isInputExpanded = inputExpanded || isSearchExpanded;
  const isOutputExpanded = outputExpanded || isSearchExpanded;

  useEffect(() => {
    if (!copiedSection) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedSection(null);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedSection]);

  async function handleCopy(section: "command" | "output", text: string) {
    try {
      await copyTextToClipboard(text);
      setCopiedSection(section);
    } catch {
      setCopiedSection(null);
    }
  }

  return (
    <article className="message-card utility-card command-card">
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          <span className={`chip chip-status chip-status-${statusTone} command-status-chip`}>
            {message.status}
          </span>
        }
      />
      <div className="card-label command-card-label">Command</div>

      <div className="command-panel">
        <div className="command-row">
          <div className="command-row-label">IN</div>
          <div className="command-row-body">
            <div className={`command-input-shell ${isInputExpanded ? "expanded" : "collapsed"}`}>
              <DeferredHighlightedCodeBlock
                className="command-text command-text-input"
                code={message.command}
                language={message.commandLanguage ?? "bash"}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copiedSection === "command" ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy("command", message.command)}
              aria-label={copiedSection === "command" ? "Command copied" : "Copy command"}
              title={copiedSection === "command" ? "Copied" : "Copy command"}
            >
              {copiedSection === "command" ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandCommand ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setInputExpanded((open) => !open)}
                aria-label={isInputExpanded ? "Collapse command" : "Expand command"}
                aria-pressed={isInputExpanded}
                title={isInputExpanded ? "Collapse command" : "Expand command"}
              >
                {isInputExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>

        <div className="command-row command-row-output">
          <div className="command-row-label">OUT</div>
          <div className="command-row-body">
            <div
              className={`command-output-shell ${isOutputExpanded ? "expanded" : "collapsed"} ${hasOutput ? "has-output" : "empty"}`}
            >
              {hasOutput ? (
                <DeferredHighlightedCodeBlock
                  className="command-text command-text-output"
                  code={displayOutput}
                  language={message.outputLanguage ?? null}
                  commandHint={message.output ? message.command : null}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : (
                <pre className="command-text command-text-output command-text-placeholder">
                  {displayOutput}
                </pre>
              )}
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copiedSection === "output" ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy("output", message.output)}
              aria-label={copiedSection === "output" ? "Output copied" : "Copy output"}
              title={copiedSection === "output" ? "Copied" : "Copy output"}
              disabled={!message.output}
            >
              {copiedSection === "output" ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandOutput ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setOutputExpanded((open) => !open)}
                aria-label={isOutputExpanded ? "Collapse output" : "Expand output"}
                aria-pressed={isOutputExpanded}
                title={isOutputExpanded ? "Collapse output" : "Expand output"}
              >
                {isOutputExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}

function CopyIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M5 2.5h6.5A1.5 1.5 0 0 1 13 4v7.5A1.5 1.5 0 0 1 11.5 13H5A1.5 1.5 0 0 1 3.5 11.5V4A1.5 1.5 0 0 1 5 2.5Zm0 1a.5.5 0 0 0-.5.5v7.5a.5.5 0 0 0 .5.5h6.5a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H5Z"
        fill="currentColor"
      />
      <path
        d="M2.5 5.5a.5.5 0 0 1 .5.5v6A1.5 1.5 0 0 0 4.5 13.5h5a.5.5 0 0 1 0 1h-5A2.5 2.5 0 0 1 2 12V6a.5.5 0 0 1 .5-.5Z"
        fill="currentColor"
      />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M13.35 4.65a.5.5 0 0 1 0 .7l-6 6a.5.5 0 0 1-.7 0l-3-3a.5.5 0 1 1 .7-.7L7 10.29l5.65-5.64a.5.5 0 0 1 .7 0Z"
        fill="currentColor"
      />
    </svg>
  );
}

function ExpandIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M2.5 6V2.5H6v1H4.2l2.15 2.15-.7.7L3.5 4.2V6h-1Zm11 0V4.2l-2.15 2.15-.7-.7L12.8 3.5H11v-1h3.5V6h-1ZM6.35 10.35l.7.7L4.2 13.9H6v1H2.5v-3.5h1V12.8l2.85-2.45Zm4.6.7.7-.7 2.85 2.45v-1.4h1v3.5H11v-1h1.8l-2.85-2.85Z"
        fill="currentColor"
      />
    </svg>
  );
}

function CollapseIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M6.35 6.35 5.65 7.05 3.5 4.9V6h-1V2.5H6v1H4.9l1.45 1.45Zm3.3 0L11.1 4.9H10v-1h3.5V6h-1V4.9l-2.15 2.15-.7-.7Zm-3.3 3.3.7.7L4.9 12.5H6v1H2.5V10h1v1.1l2.15-2.15Zm3.3.7.7-.7 2.15 2.15V10h1v3.5H10v-1h1.1l-1.45-1.45Z"
        fill="currentColor"
      />
    </svg>
  );
}

function DiffCard({
  message,
  onOpenPreview,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: DiffMessage;
  onOpenPreview: () => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const diffStats = useMemo(() => {
    const changeSummary = buildDiffPreviewModel(message.diff, message.changeType).changeSummary;
    return {
      addedLineCount: changeSummary.changedLineCount + changeSummary.addedLineCount,
      removedLineCount: changeSummary.changedLineCount + changeSummary.removedLineCount,
    };
  }, [message.changeType, message.diff]);
  const displayPath = useMemo(
    () => relativizePathToWorkspace(message.filePath, workspaceRoot),
    [message.filePath, workspaceRoot],
  );
  const canExpandDiff = message.diff.split("\n").length > 14 || message.diff.length > 900;
  const isExpanded = !canExpandDiff || expanded || searchQuery.trim().length > 0;

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(message.diff);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <article className="message-card utility-card diff-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">{message.changeType === "create" ? "New file" : "File edit"}</div>
      <div className="command-panel diff-panel">
        <div className="command-row diff-file-row">
          <div className="command-row-label diff-file-label">
            <span>FILE</span>
            {diffStats.addedLineCount > 0 || diffStats.removedLineCount > 0 ? (
              <div className="diff-file-stats">
                {diffStats.addedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-added">+{diffStats.addedLineCount}</span>
                ) : null}
                {diffStats.removedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-removed">-{diffStats.removedLineCount}</span>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="command-row-body">
            <div
              className="diff-file-path"
              title={displayPath !== message.filePath ? message.filePath : undefined}
            >
              {renderHighlightedText(displayPath, searchQuery, searchHighlightTone)}
            </div>
            <p className="diff-file-summary">
              {renderHighlightedText(message.summary, searchQuery, searchHighlightTone)}
            </p>
          </div>
        </div>
        <div className="command-row diff-row">
          <div className="command-row-label">DIFF</div>
          <div className="command-row-body">
            <div className={`diff-preview-shell ${isExpanded ? "expanded" : "collapsed"}`}>
              <DeferredHighlightedCodeBlock
                className="diff-block diff-preview-text"
                code={message.diff}
                language={message.language ?? "diff"}
                pathHint={message.filePath}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className="command-icon-button"
              type="button"
              onClick={onOpenPreview}
              aria-label="Open diff preview"
              title="Open diff preview"
            >
              <PreviewIcon />
            </button>
            <button
              className={`command-icon-button${copied ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy()}
              aria-label={copied ? "Diff copied" : "Copy diff"}
              title={copied ? "Copied" : "Copy diff"}
            >
              {copied ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandDiff ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setExpanded((open) => !open)}
                aria-label={isExpanded ? "Collapse diff" : "Expand diff"}
                aria-pressed={isExpanded}
                title={isExpanded ? "Collapse diff" : "Expand diff"}
              >
                {isExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}

function DialogCloseIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M4.25 4.25 11.75 11.75M11.75 4.25 4.25 11.75"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.6"
      />
    </svg>
  );
}

function PreviewIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M8 3c3.38 0 6.18 2.35 7 5-.82 2.65-3.62 5-7 5S1.82 10.65 1 8c.82-2.65 3.62-5 7-5Zm0 1C5.2 4 2.82 5.82 2.05 8 2.82 10.18 5.2 12 8 12s5.18-1.82 5.95-4C13.18 5.82 10.8 4 8 4Zm0 1.5A2.5 2.5 0 1 1 5.5 8 2.5 2.5 0 0 1 8 5.5Zm0 1A1.5 1.5 0 1 0 9.5 8 1.5 1.5 0 0 0 8 6.5Z"
        fill="currentColor"
      />
    </svg>
  );
}

function MarkdownCard({
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: MarkdownMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  return (
    <article className="message-card markdown-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Markdown</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredMarkdownContent
        markdown={message.markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </article>
  );
}

function parallelAgentsHeading(message: ParallelAgentsMessage) {
  const count = message.agents.length;
  const label = count === 1 ? "agent" : "agents";
  const activeCount = message.agents.filter(
    (agent) => agent.status === "initializing" || agent.status === "running",
  ).length;
  if (activeCount > 0) {
    return `Running ${count} ${label}`;
  }

  const errorCount = message.agents.filter((agent) => agent.status === "error").length;
  if (errorCount > 0) {
    return `${count} ${label} finished with ${errorCount} error${errorCount === 1 ? "" : "s"}`;
  }

  return `${count} ${label} completed`;
}

function parallelAgentsSummary(message: ParallelAgentsMessage) {
  const activeCount = message.agents.filter(
    (agent) => agent.status === "initializing" || agent.status === "running",
  ).length;
  const completedCount = message.agents.filter((agent) => agent.status === "completed").length;
  const errorCount = message.agents.filter((agent) => agent.status === "error").length;

  if (activeCount > 0) {
    const parts = [];
    if (completedCount > 0) {
      parts.push(`${completedCount} done`);
    }
    if (errorCount > 0) {
      parts.push(`${errorCount} failed`);
    }
    parts.push(`${activeCount} active`);
    return parts.join(" · ");
  }

  if (errorCount > 0 && completedCount > 0) {
    return `${completedCount} completed · ${errorCount} failed`;
  }
  if (errorCount > 0) {
    return `${errorCount} failed`;
  }

  return "All task agents completed.";
}

function parallelAgentStatusLabel(status: ParallelAgentsMessage["agents"][number]["status"]) {
  switch (status) {
    case "initializing":
      return "initializing";
    case "running":
      return "running";
    case "completed":
      return "completed";
    case "error":
      return "failed";
  }
}

function parallelAgentStatusTone(status: ParallelAgentsMessage["agents"][number]["status"]) {
  switch (status) {
    case "initializing":
    case "running":
      return "active";
    case "completed":
      return "idle";
    case "error":
      return "error";
  }
}

function parallelAgentDetail(agent: ParallelAgentsMessage["agents"][number]) {
  if (agent.detail?.trim()) {
    return agent.detail;
  }

  return agent.status === "error" ? "Task failed." : "Initializing...";
}

function ParallelAgentsCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ParallelAgentsMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(false);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const hasActiveAgents = message.agents.some(
    (agent) => agent.status === "initializing" || agent.status === "running",
  );
  useEffect(() => {
    if (hasActiveAgents) {
      setExpanded(true);
    }
  }, [hasActiveAgents]);
  const canCollapse = !hasActiveAgents && !isSearchExpanded;
  const isExpanded = hasActiveAgents || isSearchExpanded || expanded;
  const heading = parallelAgentsHeading(message);
  const summary = parallelAgentsSummary(message);

  return (
    <article className={`message-card reasoning-card parallel-agents-card${isExpanded ? " is-expanded" : ""}`}>
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          canCollapse ? (
            <button
              className="ghost-button parallel-agents-toggle"
              type="button"
              onClick={() => setExpanded((open) => !open)}
              aria-expanded={isExpanded}
            >
              {isExpanded ? "Hide tasks" : "Show tasks"}
            </button>
          ) : undefined
        }
      />
      <div className="card-label parallel-agents-card-label">Parallel agents</div>
      <div className="parallel-agents-header">
        <h3>{renderHighlightedText(heading, searchQuery, searchHighlightTone)}</h3>
        <span className="parallel-agents-summary">{summary}</span>
      </div>
      {isExpanded ? (
        <ol className="parallel-agents-tree">
          {message.agents.map((agent, index) => {
            const isLast = index === message.agents.length - 1;
            return (
              <li
                key={agent.id}
                className={`parallel-agent-row parallel-agent-row-${parallelAgentStatusTone(agent.status)}`}
              >
                <div className="parallel-agent-line">
                  <span className="parallel-agent-branch" aria-hidden="true">
                    {isLast ? "└" : "├"}
                  </span>
                  <div className="parallel-agent-copy">
                    <div className="parallel-agent-title-row">
                      <span className="parallel-agent-title">
                        {renderHighlightedText(agent.title, searchQuery, searchHighlightTone)}
                      </span>
                      <span
                        className={`parallel-agent-status parallel-agent-status-${parallelAgentStatusTone(agent.status)}`}
                      >
                        {parallelAgentStatusLabel(agent.status)}
                      </span>
                    </div>
                    <div className="parallel-agent-detail-row">
                      <span className="parallel-agent-branch-child" aria-hidden="true">
                        {isLast ? " " : "│"}
                      </span>
                      <span className="parallel-agent-detail">
                        {renderHighlightedText(
                          parallelAgentDetail(agent),
                          searchQuery,
                          searchHighlightTone,
                        )}
                      </span>
                    </div>
                  </div>
                </div>
              </li>
            );
          })}
        </ol>
      ) : null}
    </article>
  );
}
function SubagentResultCard({
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: SubagentResultMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isExpanded = expanded || isSearchExpanded;

  return (
    <article className={`message-card reasoning-card subagent-result-card${isExpanded ? " is-expanded" : ""}`}>
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          <button
            className="ghost-button subagent-result-toggle"
            type="button"
            onClick={() => setExpanded((open) => !open)}
            aria-expanded={isExpanded}
          >
            {isExpanded ? "Hide details" : "Show details"}
          </button>
        }
      />
      <div className="card-label subagent-result-card-label">Thinking</div>
      {isExpanded ? (
        <>
          <div className="subagent-result-header">
            <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
          </div>
          <DeferredMarkdownContent
            markdown={message.summary}
            onOpenSourceLink={onOpenSourceLink}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
            workspaceRoot={workspaceRoot}
          />
        </>
      ) : null}
    </article>
  );
}

function ApprovalCard({
  message,
  onApprovalDecision,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ApprovalMessage;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const decided = message.decision !== "pending";
  const chosen = (d: ApprovalDecision) => (message.decision === d ? " chosen" : "");
  const resolvedDecision = message.decision === "pending" ? null : message.decision;

  return (
    <article className={`message-card approval-card${decided ? " decided" : ""}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Approval</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredHighlightedCodeBlock
        className="approval-command"
        code={message.command}
        language={message.commandLanguage ?? "bash"}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>
      <div className="approval-actions">
        <button
          className={`approval-button${chosen("accepted")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "accepted")}
          disabled={decided}
        >
          Approve
        </button>
        <button
          className={`approval-button${chosen("acceptedForSession")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "acceptedForSession")}
          disabled={decided}
        >
          Approve for session
        </button>
        <button
          className={`approval-button approval-button-reject${chosen("rejected")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "rejected")}
          disabled={decided}
        >
          Reject
        </button>
      </div>
      {resolvedDecision ? (
        <p className="approval-result">Decision: {renderDecision(resolvedDecision)}</p>
      ) : null}
    </article>
  );
}

type UserInputDraftField = {
  customAnswer: string;
  selectedOption: string;
};

type McpElicitationDraftField = {
  selectedOption: string;
  selections: string[];
  text: string;
};

function buildUserInputDraft(
  questions: UserInputQuestion[],
  submittedAnswers?: Record<string, string[]> | null,
): Record<string, UserInputDraftField> {
  const next: Record<string, UserInputDraftField> = {};
  for (const question of questions) {
    const answer = submittedAnswers?.[question.id]?.[0] ?? "";
    const optionLabels = new Set((question.options ?? []).map((option) => option.label));
    if (optionLabels.has(answer)) {
      next[question.id] = {
        customAnswer: "",
        selectedOption: answer,
      };
      continue;
    }

    next[question.id] = {
      customAnswer: answer === "[secret provided]" ? "" : answer,
      selectedOption: question.isOther && answer ? "__other__" : "",
    };
  }
  return next;
}

function buildUserInputSummary(
  message: UserInputRequestMessage,
  searchQuery: string,
  searchHighlightTone: SearchHighlightTone,
) {
  const submittedAnswers = message.submittedAnswers ?? {};
  return message.questions
    .filter((question) => submittedAnswers[question.id]?.length)
    .map((question) => (
      <div key={question.id} className="user-input-summary-row">
        <div className="user-input-summary-header">
          {renderHighlightedText(question.header, searchQuery, searchHighlightTone)}
        </div>
        <div className="user-input-summary-value">
          {renderHighlightedText(
            submittedAnswers[question.id]!.join(", "),
            searchQuery,
            searchHighlightTone,
          )}
        </div>
      </div>
    ));
}

function UserInputRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: UserInputRequestMessage;
  onSubmit: (messageId: string, answers: Record<string, string[]>) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [draft, setDraft] = useState<Record<string, UserInputDraftField>>(() =>
    buildUserInputDraft(message.questions, message.submittedAnswers),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const pending = message.state === "pending";

  useEffect(() => {
    setDraft(buildUserInputDraft(message.questions, message.submittedAnswers));
    setValidationError(null);
  }, [message.id, message.questions, message.state, message.submittedAnswers]);

  function updateField(questionId: string, nextField: Partial<UserInputDraftField>) {
    setDraft((current) => ({
      ...current,
      [questionId]: {
        customAnswer: current[questionId]?.customAnswer ?? "",
        selectedOption: current[questionId]?.selectedOption ?? "",
        ...nextField,
      },
    }));
  }

  function handleSubmit() {
    const answers: Record<string, string[]> = {};
    for (const question of message.questions) {
      const field = draft[question.id] ?? { customAnswer: "", selectedOption: "" };
      const optionLabels = new Set((question.options ?? []).map((option) => option.label));
      let answer = "";
      if (field.selectedOption && field.selectedOption !== "__other__") {
        answer = field.selectedOption;
      } else {
        answer = field.customAnswer.trim();
      }

      if (!answer) {
        setValidationError(`Answer "${question.header}" before submitting.`);
        return;
      }
      if (optionLabels.size > 0 && !optionLabels.has(answer) && !question.isOther) {
        setValidationError(`"${question.header}" must use one of the provided options.`);
        return;
      }

      answers[question.id] = [answer];
    }

    setValidationError(null);
    onSubmit(message.id, answers);
  }

  return (
    <article className={`message-card user-input-card${pending ? "" : " decided"}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Input request</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>

      <div className="user-input-questions">
        {message.questions.map((question) => {
          const field = draft[question.id] ?? { customAnswer: "", selectedOption: "" };
          const options = question.options ?? [];
          const inputType = question.isSecret ? "password" : "text";
          const usesOther = !!question.isOther;
          const showFreeform = options.length === 0 || field.selectedOption === "__other__";

          return (
            <section key={question.id} className="user-input-question">
              <div className="user-input-question-header">
                {renderHighlightedText(question.header, searchQuery, searchHighlightTone)}
              </div>
              <p className="support-copy">
                {renderHighlightedText(question.question, searchQuery, searchHighlightTone)}
              </p>

              {options.length > 0 ? (
                <div className="user-input-options">
                  {options.map((option) => (
                    <label key={option.label} className="user-input-option">
                      <input
                        type="radio"
                        name={`user-input-${message.id}-${question.id}`}
                        checked={field.selectedOption === option.label}
                        disabled={!pending}
                        onChange={() =>
                          updateField(question.id, {
                            customAnswer: "",
                            selectedOption: option.label,
                          })
                        }
                      />
                      <span>
                        <strong>
                          {renderHighlightedText(option.label, searchQuery, searchHighlightTone)}
                        </strong>
                        <span className="user-input-option-description">
                          {renderHighlightedText(
                            option.description,
                            searchQuery,
                            searchHighlightTone,
                          )}
                        </span>
                      </span>
                    </label>
                  ))}
                  {usesOther ? (
                    <label className="user-input-option">
                      <input
                        type="radio"
                        name={`user-input-${message.id}-${question.id}`}
                        checked={field.selectedOption === "__other__"}
                        disabled={!pending}
                        onChange={() => updateField(question.id, { selectedOption: "__other__" })}
                      />
                      <span>Other</span>
                    </label>
                  ) : null}
                </div>
              ) : null}

              {showFreeform ? (
                <input
                  className="user-input-text"
                  type={inputType}
                  value={field.customAnswer}
                  disabled={!pending}
                  onChange={(event) =>
                    updateField(question.id, { customAnswer: event.target.value })
                  }
                />
              ) : null}
            </section>
          );
        })}
      </div>

      {!pending ? (
        <div className="user-input-summary">
          {buildUserInputSummary(message, searchQuery, searchHighlightTone)}
        </div>
      ) : null}

      {validationError ? <p className="approval-result">{validationError}</p> : null}

      {pending ? (
        <div className="approval-actions">
          <button className="approval-button" type="button" onClick={handleSubmit}>
            Submit answers
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

function isJsonObject(
  value: JsonValue | null | undefined,
): value is { [key: string]: JsonValue | undefined } {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function mcpSingleSelectOptions(schema: McpElicitationPrimitiveSchema) {
  if (schema.type !== "string") {
    return [];
  }
  if (schema.oneOf?.length) {
    return schema.oneOf.map((option) => ({ label: option.title, value: option.const }));
  }
  return (schema.enum ?? []).map((value, index) => ({
    label: schema.enumNames?.[index] ?? value,
    value,
  }));
}

function mcpMultiSelectOptions(schema: McpElicitationPrimitiveSchema) {
  if (schema.type !== "array") {
    return [];
  }
  if (schema.items.anyOf?.length) {
    return schema.items.anyOf.map((option) => ({ label: option.title, value: option.const }));
  }
  return (schema.items.enum ?? []).map((value) => ({ label: value, value }));
}

function buildMcpElicitationDraft(
  message: McpElicitationRequestMessage,
): Record<string, McpElicitationDraftField> {
  if (message.request.mode !== "form") {
    return {};
  }

  const submitted = isJsonObject(message.submittedContent) ? message.submittedContent : {};
  const next: Record<string, McpElicitationDraftField> = {};
  for (const [fieldName, schema] of Object.entries(message.request.requestedSchema.properties)) {
    if (!schema) {
      continue;
    }
    const submittedValue = submitted[fieldName];
    switch (schema.type) {
      case "boolean":
        next[fieldName] = {
          selectedOption:
            typeof submittedValue === "boolean"
              ? submittedValue
                ? "true"
                : "false"
              : "",
          selections: [],
          text: "",
        };
        break;
      case "array":
        next[fieldName] = {
          selectedOption: "",
          selections: Array.isArray(submittedValue)
            ? submittedValue.filter((value): value is string => typeof value === "string")
            : (schema.default ?? []),
          text: "",
        };
        break;
      case "number":
      case "integer":
        next[fieldName] = {
          selectedOption: "",
          selections: [],
          text:
            typeof submittedValue === "number"
              ? String(submittedValue)
              : schema.default !== undefined && schema.default !== null
                ? String(schema.default)
                : "",
        };
        break;
      case "string": {
        const options = mcpSingleSelectOptions(schema);
        const submittedText = typeof submittedValue === "string" ? submittedValue : "";
        next[fieldName] = {
          selectedOption: options.some((option) => option.value === submittedText) ? submittedText : "",
          selections: [],
          text:
            submittedText ||
            (typeof schema.default === "string" ? schema.default : ""),
        };
        break;
      }
    }
  }
  return next;
}

function formatMcpElicitationSummaryValue(value: JsonValue) {
  if (Array.isArray(value)) {
    return value.join(", ");
  }
  if (typeof value === "boolean") {
    return value ? "Yes" : "No";
  }
  if (value === null) {
    return "null";
  }
  if (typeof value === "object") {
    return JSON.stringify(value);
  }
  return String(value);
}

function buildMcpElicitationSummary(
  message: McpElicitationRequestMessage,
  searchQuery: string,
  searchHighlightTone: SearchHighlightTone,
) {
  if (!isJsonObject(message.submittedContent) || message.request.mode !== "form") {
    return null;
  }
  const submittedContent = message.submittedContent;

  return Object.entries(message.request.requestedSchema.properties)
    .filter(([fieldName]) => submittedContent[fieldName] !== undefined)
    .map(([fieldName, schema]) => {
      const value = submittedContent[fieldName];
      if (!schema || value === undefined) {
        return null;
      }
      return (
        <div key={fieldName} className="user-input-summary-row">
          <div className="user-input-summary-header">
            {renderHighlightedText(schema.title ?? fieldName, searchQuery, searchHighlightTone)}
          </div>
          <div className="user-input-summary-value">
            {renderHighlightedText(
              formatMcpElicitationSummaryValue(value),
              searchQuery,
              searchHighlightTone,
            )}
          </div>
        </div>
      );
    });
}

function McpElicitationRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: McpElicitationRequestMessage;
  onSubmit: (messageId: string, action: McpElicitationAction, content?: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [draft, setDraft] = useState<Record<string, McpElicitationDraftField>>(() =>
    buildMcpElicitationDraft(message),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const pending = message.state === "pending";

  useEffect(() => {
    setDraft(buildMcpElicitationDraft(message));
    setValidationError(null);
  }, [message]);

  function updateField(fieldName: string, nextField: Partial<McpElicitationDraftField>) {
    setDraft((current) => ({
      ...current,
      [fieldName]: {
        selectedOption: current[fieldName]?.selectedOption ?? "",
        selections: current[fieldName]?.selections ?? [],
        text: current[fieldName]?.text ?? "",
        ...nextField,
      },
    }));
  }

  function handleSubmit(action: McpElicitationAction) {
    if (message.request.mode !== "form" || action !== "accept") {
      setValidationError(null);
      onSubmit(message.id, action);
      return;
    }

    const required = new Set(message.request.requestedSchema.required ?? []);
    const content: Record<string, JsonValue> = {};

    for (const [fieldName, schema] of Object.entries(message.request.requestedSchema.properties)) {
      if (!schema) {
        continue;
      }
      const field = draft[fieldName] ?? { selectedOption: "", selections: [], text: "" };
      switch (schema.type) {
        case "string": {
          const options = mcpSingleSelectOptions(schema);
          const hasOptions = options.length > 0;
          const rawValue = hasOptions ? field.selectedOption : field.text.trim();
          if (!rawValue) {
            if (required.has(fieldName)) {
              setValidationError(`Answer "${schema.title ?? fieldName}" before accepting.`);
              return;
            }
            break;
          }
          if (hasOptions && !options.some((option) => option.value === rawValue)) {
            setValidationError(`"${schema.title ?? fieldName}" must use one of the provided options.`);
            return;
          }
          const valueLength = Array.from(rawValue).length;
          if (schema.minLength != null && valueLength < schema.minLength) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at least ${schema.minLength} characters.`,
            );
            return;
          }
          if (schema.maxLength != null && valueLength > schema.maxLength) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at most ${schema.maxLength} characters.`,
            );
            return;
          }
          content[fieldName] = rawValue;
          break;
        }
        case "number":
        case "integer": {
          const rawValue = field.text.trim();
          if (!rawValue) {
            if (required.has(fieldName)) {
              setValidationError(`Answer "${schema.title ?? fieldName}" before accepting.`);
              return;
            }
            break;
          }
          const numericValue = Number(rawValue);
          if (!Number.isFinite(numericValue)) {
            setValidationError(`"${schema.title ?? fieldName}" must be a valid number.`);
            return;
          }
          if (schema.type === "integer" && !Number.isInteger(numericValue)) {
            setValidationError(`"${schema.title ?? fieldName}" must be a whole number.`);
            return;
          }
          if (schema.minimum != null && numericValue < schema.minimum) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at least ${schema.minimum}.`,
            );
            return;
          }
          if (schema.maximum != null && numericValue > schema.maximum) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at most ${schema.maximum}.`,
            );
            return;
          }
          content[fieldName] = numericValue;
          break;
        }
        case "boolean": {
          if (!field.selectedOption) {
            if (required.has(fieldName)) {
              setValidationError(`Answer "${schema.title ?? fieldName}" before accepting.`);
              return;
            }
            break;
          }
          content[fieldName] = field.selectedOption === "true";
          break;
        }
        case "array": {
          const options = mcpMultiSelectOptions(schema);
          if (field.selections.length === 0) {
            if (required.has(fieldName) || (schema.minItems ?? 0) > 0) {
              setValidationError(`Choose at least one option for "${schema.title ?? fieldName}".`);
              return;
            }
            break;
          }
          if (!field.selections.every((selection) => options.some((option) => option.value === selection))) {
            setValidationError(`"${schema.title ?? fieldName}" must use one of the provided options.`);
            return;
          }
          if (schema.minItems != null && field.selections.length < schema.minItems) {
            setValidationError(
              `"${schema.title ?? fieldName}" must include at least ${schema.minItems} selections.`,
            );
            return;
          }
          if (schema.maxItems != null && field.selections.length > schema.maxItems) {
            setValidationError(
              `"${schema.title ?? fieldName}" must include at most ${schema.maxItems} selections.`,
            );
            return;
          }
          content[fieldName] = field.selections;
          break;
        }
      }
    }

    setValidationError(null);
    onSubmit(message.id, action, content);
  }

  return (
    <article className={`message-card user-input-card${pending ? "" : " decided"}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">MCP input</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>

      {message.request.mode === "url" ? (
        <p className="support-copy">
          <a href={message.request.url} target="_blank" rel="noreferrer">
            {renderHighlightedText(message.request.url, searchQuery, searchHighlightTone)}
          </a>
        </p>
      ) : (
        <div className="user-input-questions">
          {Object.entries(message.request.requestedSchema.properties).map(([fieldName, schema]) => {
            if (!schema) {
              return null;
            }
            const field = draft[fieldName] ?? { selectedOption: "", selections: [], text: "" };
            const label = schema.title ?? fieldName;
            const description = schema.description ?? message.request.message;
            const singleOptions = mcpSingleSelectOptions(schema);
            const multiOptions = mcpMultiSelectOptions(schema);
            return (
              <section key={fieldName} className="user-input-question">
                <div className="user-input-question-header">
                  {renderHighlightedText(label, searchQuery, searchHighlightTone)}
                </div>
                {description ? (
                  <p className="support-copy">
                    {renderHighlightedText(description, searchQuery, searchHighlightTone)}
                  </p>
                ) : null}

                {schema.type === "string" && singleOptions.length > 0 ? (
                  <div className="user-input-options">
                    {singleOptions.map((option) => (
                      <label key={option.value} className="user-input-option">
                        <input
                          type="radio"
                          name={`mcp-elicitation-${message.id}-${fieldName}`}
                          checked={field.selectedOption === option.value}
                          disabled={!pending}
                          onChange={() => updateField(fieldName, { selectedOption: option.value })}
                        />
                        <span>{renderHighlightedText(option.label, searchQuery, searchHighlightTone)}</span>
                      </label>
                    ))}
                  </div>
                ) : null}

                {schema.type === "array" ? (
                  <div className="user-input-options">
                    {multiOptions.map((option) => (
                      <label key={option.value} className="user-input-option">
                        <input
                          type="checkbox"
                          checked={field.selections.includes(option.value)}
                          disabled={!pending}
                          onChange={(event) =>
                            updateField(fieldName, {
                              selections: event.target.checked
                                ? [...field.selections, option.value]
                                : field.selections.filter((value) => value !== option.value),
                            })
                          }
                        />
                        <span>{renderHighlightedText(option.label, searchQuery, searchHighlightTone)}</span>
                      </label>
                    ))}
                  </div>
                ) : null}

                {schema.type === "boolean" ? (
                  <div className="user-input-options">
                    <label className="user-input-option">
                      <input
                        type="radio"
                        name={`mcp-elicitation-${message.id}-${fieldName}`}
                        checked={field.selectedOption === "true"}
                        disabled={!pending}
                        onChange={() => updateField(fieldName, { selectedOption: "true" })}
                      />
                      <span>Yes</span>
                    </label>
                    <label className="user-input-option">
                      <input
                        type="radio"
                        name={`mcp-elicitation-${message.id}-${fieldName}`}
                        checked={field.selectedOption === "false"}
                        disabled={!pending}
                        onChange={() => updateField(fieldName, { selectedOption: "false" })}
                      />
                      <span>No</span>
                    </label>
                  </div>
                ) : null}

                {(schema.type === "number" || schema.type === "integer" || (schema.type === "string" && singleOptions.length === 0)) ? (
                  <input
                    className="user-input-text"
                    type={schema.type === "number" || schema.type === "integer" ? "number" : "text"}
                    value={field.text}
                    min={schema.type === "number" || schema.type === "integer" ? (schema.minimum ?? undefined) : undefined}
                    max={schema.type === "number" || schema.type === "integer" ? (schema.maximum ?? undefined) : undefined}
                    minLength={schema.type === "string" ? (schema.minLength ?? undefined) : undefined}
                    maxLength={schema.type === "string" ? (schema.maxLength ?? undefined) : undefined}
                    disabled={!pending}
                    onChange={(event) => updateField(fieldName, { text: event.target.value })}
                  />
                ) : null}
              </section>
            );
          })}
        </div>
      )}

      {!pending ? (
        <div className="user-input-summary">
          <div className="user-input-summary-row">
            <div className="user-input-summary-header">Decision</div>
            <div className="user-input-summary-value">{message.submittedAction ?? message.state}</div>
          </div>
          {buildMcpElicitationSummary(message, searchQuery, searchHighlightTone)}
        </div>
      ) : null}

      {validationError ? <p className="approval-result">{validationError}</p> : null}

      {pending ? (
        <div className="approval-actions">
          <button className="approval-button" type="button" onClick={() => handleSubmit("accept")}>
            Accept
          </button>
          <button className="approval-button" type="button" onClick={() => handleSubmit("decline")}>
            Decline
          </button>
          <button
            className="approval-button approval-button-reject"
            type="button"
            onClick={() => handleSubmit("cancel")}
          >
            Cancel
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

function formatJsonValueForEditor(value: JsonValue | null | undefined) {
  return JSON.stringify(value ?? {}, null, 2);
}

function CodexAppRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CodexAppRequestMessage;
  onSubmit: (messageId: string, result: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const pending = message.state === "pending";
  const [draft, setDraft] = useState(() => formatJsonValueForEditor(message.submittedResult));
  const [validationError, setValidationError] = useState<string | null>(null);

  useEffect(() => {
    setDraft(formatJsonValueForEditor(message.submittedResult));
    setValidationError(null);
  }, [message]);

  function handleSubmit() {
    try {
      const parsed = JSON.parse(draft) as JsonValue;
      setValidationError(null);
      onSubmit(message.id, parsed);
    } catch {
      setValidationError("Response must be valid JSON.");
    }
  }

  return (
    <article className={`message-card user-input-card${pending ? "" : " decided"}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Codex request</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>

      <div className="user-input-summary">
        <div className="user-input-summary-row">
          <div className="user-input-summary-header">Method</div>
          <div className="user-input-summary-value">{message.method}</div>
        </div>
      </div>

      <div className="codex-request-json-block">
        <div className="user-input-summary-header">Request payload</div>
        <pre>{formatJsonValueForEditor(message.params)}</pre>
      </div>

      {pending ? (
        <label className="codex-request-editor">
          <span className="user-input-summary-header">JSON result</span>
          <textarea
            className="codex-request-textarea"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            spellCheck={false}
          />
        </label>
      ) : (
        <div className="codex-request-json-block">
          <div className="user-input-summary-header">Submitted result</div>
          <pre>{formatJsonValueForEditor(message.submittedResult)}</pre>
        </div>
      )}

      {validationError ? <p className="approval-result">{validationError}</p> : null}

      {pending ? (
        <div className="approval-actions">
          <button className="approval-button" type="button" onClick={handleSubmit}>
            Submit JSON result
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

type MarkdownAstNode = {
  type: string;
  value?: string;
  url?: string;
  children?: MarkdownAstNode[];
};

const MARKDOWN_BARE_FILE_REFERENCE_PATTERN =
  /(^|[\s([{"\'""`])((?:(?:\.\.?[\\/])|(?:\/[A-Za-z]:[\\/])|(?:[A-Za-z]:[\\/])|(?:\\\\)|\/)?(?:[\w.-]+[\\/])*[\w.-]+\.[A-Za-z0-9]{2,12}(?:\.?#L\d+(?:C\d+)?|:\d+(?::\d+)?))(?=$|[\s)\]},"\'"";!?`.])/g;
const MARKDOWN_BARE_FILE_REFERENCE_IGNORED_NODE_TYPES = new Set([
  "code",
  "definition",
  "html",
  "inlineCode",
  "link",
  "linkReference",
]);
const MARKDOWN_REMARK_PLUGINS = [remarkGfm, remarkAutolinkBareFileReferences];

function remarkAutolinkBareFileReferences() {
  return (tree: MarkdownAstNode) => {
    autolinkMarkdownBareFileReferences(tree);
  };
}

function autolinkMarkdownBareFileReferences(node: MarkdownAstNode) {
  if (!Array.isArray(node.children) || MARKDOWN_BARE_FILE_REFERENCE_IGNORED_NODE_TYPES.has(node.type)) {
    return;
  }

  for (let index = 0; index < node.children.length; index += 1) {
    const child = node.children[index];
    if (child.type === "text" && typeof child.value === "string") {
      const replacement = buildAutolinkedMarkdownTextNodes(child.value);
      if (replacement) {
        node.children.splice(index, 1, ...replacement);
        index += replacement.length - 1;
        continue;
      }
    }

    autolinkMarkdownBareFileReferences(child);
  }
}

function buildAutolinkedMarkdownTextNodes(text: string) {
  MARKDOWN_BARE_FILE_REFERENCE_PATTERN.lastIndex = 0;
  const nodes: MarkdownAstNode[] = [];
  let changed = false;
  let lastIndex = 0;
  let match = MARKDOWN_BARE_FILE_REFERENCE_PATTERN.exec(text);

  while (match) {
    const prefix = match[1] ?? "";
    const reference = match[2] ?? "";
    const matchIndex = match.index;
    const referenceStartIndex = matchIndex + prefix.length;
    if (matchIndex > lastIndex) {
      nodes.push(createMarkdownTextNode(text.slice(lastIndex, matchIndex)));
    }
    if (prefix) {
      nodes.push(createMarkdownTextNode(prefix));
    }
    nodes.push(createMarkdownLinkNode(reference));
    lastIndex = referenceStartIndex + reference.length;
    changed = true;
    match = MARKDOWN_BARE_FILE_REFERENCE_PATTERN.exec(text);
  }

  if (!changed) {
    return null;
  }

  if (lastIndex < text.length) {
    nodes.push(createMarkdownTextNode(text.slice(lastIndex)));
  }

  return nodes.filter((node) => node.type !== "text" || (node.value?.length ?? 0) > 0);
}

function createMarkdownLinkNode(reference: string): MarkdownAstNode {
  return {
    type: "link",
    url: reference,
    children: [createMarkdownTextNode(reference)],
  };
}

function createMarkdownTextNode(value: string): MarkdownAstNode {
  return {
    type: "text",
    value,
  };
}

export function MarkdownContent({
  markdown,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const highlightChildren = (children: ReactNode) =>
    highlightReactNodeText(children, searchQuery, searchHighlightTone);

  return (
    <div className="markdown-copy">
      <ReactMarkdown
        transformLinkUri={transformMarkdownLinkUri}
        components={{
          a: ({ href, children, ...props }) => {
            const isExternalLink = isExternalMarkdownHref(href ?? "");
            const fileLinkTarget = resolveMarkdownFileLinkTarget(href, workspaceRoot);
            const handleClick = (event: ReactMouseEvent<HTMLAnchorElement>) => {
              props.onClick?.(event);
              if (event.defaultPrevented || !fileLinkTarget || !onOpenSourceLink) {
                return;
              }

              event.preventDefault();
              onOpenSourceLink({
                ...fileLinkTarget,
                openInNewTab: event.ctrlKey || event.metaKey,
              });
            };

            return (
              <a
                {...props}
                href={href}
                target={isExternalLink ? "_blank" : undefined}
                rel={isExternalLink ? "noreferrer" : undefined}
                onClick={handleClick}
              >
                {highlightChildren(children)}
              </a>
            );
          },
          code: ({ children, className, inline, ...props }) => {
            const language = className?.match(/language-([\w-]+)/)?.[1] ?? null;
            const code = String(children).replace(/\n$/, "");
            const inlineFileLinkTarget = inline
              ? resolveMarkdownFileLinkTarget(code, workspaceRoot)
              : null;

            if (inline) {
              if (inlineFileLinkTarget && onOpenSourceLink) {
                const handleInlineCodeClick = (event: ReactMouseEvent<HTMLAnchorElement>) => {
                  event.preventDefault();
                  onOpenSourceLink({
                    ...inlineFileLinkTarget,
                    openInNewTab: event.ctrlKey || event.metaKey,
                  });
                };

                return (
                  <a className="inline-code-link" href={code} onClick={handleInlineCodeClick}>
                    <code className={className} {...props}>
                      {highlightChildren(children)}
                    </code>
                  </a>
                );
              }

              return (
                <code className={className} {...props}>
                  {highlightChildren(children)}
                </code>
              );
            }

            return (
              <HighlightedCodeBlock
                className="code-block"
                code={code}
                language={language}
                showCopyButton
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            );
          },
          p: ({ children, ...props }) => <p {...props}>{highlightChildren(children)}</p>,
          li: ({ children, ordered: _ordered, index: _index, checked: _checked, ...props }) => (
            <li {...props}>{highlightChildren(children)}</li>
          ),
          blockquote: ({ children, ...props }) => (
            <blockquote {...props}>{highlightChildren(children)}</blockquote>
          ),
          h1: ({ children, ...props }) => <h1 {...props}>{highlightChildren(children)}</h1>,
          h2: ({ children, ...props }) => <h2 {...props}>{highlightChildren(children)}</h2>,
          h3: ({ children, ...props }) => <h3 {...props}>{highlightChildren(children)}</h3>,
          h4: ({ children, ...props }) => <h4 {...props}>{highlightChildren(children)}</h4>,
          h5: ({ children, ...props }) => <h5 {...props}>{highlightChildren(children)}</h5>,
          h6: ({ children, ...props }) => <h6 {...props}>{highlightChildren(children)}</h6>,
          strong: ({ children, ...props }) => (
            <strong {...props}>{highlightChildren(children)}</strong>
          ),
          em: ({ children, ...props }) => <em {...props}>{highlightChildren(children)}</em>,
          del: ({ children, ...props }) => <del {...props}>{highlightChildren(children)}</del>,
          table: ({ children, ...props }) => (
            <div className="markdown-table-scroll">
              <table {...props}>{children}</table>
            </div>
          ),
          td: ({ children, isHeader: _isHeader, ...props }) => (
            <td {...props}>{highlightChildren(children)}</td>
          ),
          th: ({ children, isHeader: _isHeader, ...props }) => (
            <th {...props}>{highlightChildren(children)}</th>
          ),
        }}
        remarkPlugins={MARKDOWN_REMARK_PLUGINS}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}

function resolveMarkdownFileLinkTarget(
  href: string | undefined,
  workspaceRoot: string | null,
): Omit<MarkdownFileLinkTarget, "openInNewTab"> | null {
  const trimmedHref = href?.trim();
  if (!trimmedHref || isExternalMarkdownHref(trimmedHref) || trimmedHref.startsWith("#")) {
    return null;
  }

  let candidate = safeDecodeMarkdownHref(trimmedHref).replace(/^file:\/\//i, "");
  let line: number | undefined;
  let column: number | undefined;

  const hashIndex = candidate.indexOf("#");
  if (hashIndex >= 0) {
    const fragment = candidate.slice(hashIndex + 1);
    candidate = candidate.slice(0, hashIndex);
    const fragmentLocation = parseMarkdownFileLinkFragment(fragment);
    if (fragmentLocation) {
      line = fragmentLocation.line;
      column = fragmentLocation.column;
    }
  }

  const lineSuffixMatch = candidate.match(/^(.*?)(?::(\d+)(?::(\d+))?)$/);
  if (lineSuffixMatch) {
    candidate = lineSuffixMatch[1] ?? candidate;
    if (!line && lineSuffixMatch[2]) {
      line = Number(lineSuffixMatch[2]);
    }
    if (!column && lineSuffixMatch[3]) {
      column = Number(lineSuffixMatch[3]);
    }
  }

  let trimmedCandidate = candidate.trim();
  if ((line || column) && trimmedCandidate.endsWith(".")) {
    trimmedCandidate = trimmedCandidate.slice(0, -1).trimEnd();
  }
  if (!trimmedCandidate) {
    return null;
  }

  const resolvedPath = looksLikeAbsoluteMarkdownFilePath(trimmedCandidate, workspaceRoot)
    ? normalizeMarkdownFileLinkAbsolutePath(trimmedCandidate)
    : !workspaceRoot || !looksLikeRelativeMarkdownFilePath(trimmedCandidate)
      ? null
      : joinWorkspacePath(workspaceRoot, trimmedCandidate);
  if (!resolvedPath) {
    return null;
  }

  return {
    path: resolvedPath,
    ...(line ? { line } : {}),
    ...(column ? { column } : {}),
  };
}

function parseMarkdownFileLinkFragment(fragment: string) {
  const match = fragment.trim().match(/^L(\d+)(?:C(\d+))?$/i);
  if (!match) {
    return null;
  }

  return {
    line: Number(match[1]),
    ...(match[2] ? { column: Number(match[2]) } : {}),
  };
}

function isExternalMarkdownHref(href: string) {
  if (!href) {
    return false;
  }

  if (/^file:\/\//i.test(href) || /^[A-Za-z]:[\\/]/.test(href)) {
    return false;
  }

  return /^\/\//.test(href) || /^[a-z][a-z\d+.-]*:/i.test(href);
}

function transformMarkdownLinkUri(href: string) {
  return isExternalMarkdownHref(href) ? uriTransformer(href) : href;
}

function safeDecodeMarkdownHref(href: string) {
  try {
    return decodeURIComponent(href);
  } catch {
    return href;
  }
}

function looksLikeAbsoluteMarkdownFilePath(path: string, workspaceRoot: string | null) {
  if (/^\/[A-Za-z]:[\\/]/.test(path) || /^[A-Za-z]:[\\/]/.test(path) || /^\\\\/.test(path)) {
    return true;
  }

  if (!path.startsWith("/") || path.startsWith("//")) {
    return false;
  }

  const trimmedRoot = workspaceRoot?.trim();
  if (trimmedRoot) {
    const normalizedPath = normalizeDisplayPath(path);
    const normalizedRoot = normalizeDisplayPath(trimmedRoot).replace(/\/+$/, "");
    if (normalizedPath === normalizedRoot || normalizedPath.startsWith(`${normalizedRoot}/`)) {
      return true;
    }
  }

  return looksLikeFilePathReference(path);
}

function normalizeMarkdownFileLinkAbsolutePath(path: string) {
  return /^\/[A-Za-z]:[\\/]/.test(path) ? path.slice(1) : path;
}

function looksLikeRelativeMarkdownFilePath(path: string) {
  if (!path || path.startsWith("/") || /^[a-z][a-z\d+.-]*:/i.test(path)) {
    return false;
  }

  return path.startsWith("./") || path.startsWith("../") || /[\\/]/.test(path) || looksLikeFilePathReference(path);
}

function looksLikeFilePathReference(path: string) {
  const segments = path.trim().split(/[\\/]+/).filter(Boolean);
  const fileName = segments[segments.length - 1] ?? "";
  return fileName.startsWith(".") || fileName.includes(".");
}

function joinWorkspacePath(rootPath: string, relativePath: string) {
  const trimmedRoot = rootPath.trim().replace(/[\\/]+$/, "");
  const trimmedRelative = relativePath.trim().replace(/^[\\/]+/, "");
  if (!trimmedRoot) {
    return trimmedRelative;
  }

  if (!trimmedRelative) {
    return trimmedRoot;
  }

  const useBackslashSeparator = looksLikeWindowsPath(trimmedRoot);
  const normalizedRelative = useBackslashSeparator
    ? trimmedRelative.replace(/\//g, "\\")
    : trimmedRelative.replace(/\\/g, "/");
  return `${trimmedRoot}${useBackslashSeparator ? "\\" : "/"}${normalizedRelative}`;
}

function resolveDeferredRenderRoot(node: Element) {
  const root = node.closest(".message-stack");
  return root instanceof Element ? root : null;
}

function isElementNearRenderViewport(
  node: Element,
  root: Element | null,
  marginPx: number,
) {
  const nodeRect = node.getBoundingClientRect();
  const rootRect = root?.getBoundingClientRect() ?? {
    top: 0,
    bottom: window.innerHeight,
  };

  return nodeRect.bottom >= rootRect.top - marginPx && nodeRect.top <= rootRect.bottom + marginPx;
}

function measureTextBlock(text: string) {
  return {
    lineCount: text.length === 0 ? 1 : text.split("\n").length,
  };
}

function estimateCodeBlockHeight(lineCount: number) {
  return Math.min(MAX_DEFERRED_PLACEHOLDER_HEIGHT, Math.max(120, lineCount * 20 + 48));
}

function estimateMarkdownBlockHeight(lineCount: number) {
  return Math.min(MAX_DEFERRED_PLACEHOLDER_HEIGHT, Math.max(140, lineCount * 28 + 56));
}

function buildDeferredPreviewText(text: string) {
  const preview = text
    .split("\n")
    .slice(0, DEFERRED_PREVIEW_LINE_LIMIT)
    .join("\n")
    .slice(0, DEFERRED_PREVIEW_CHARACTER_LIMIT)
    .trimEnd();

  return preview.length < text.length ? `${preview}\n\u2026` : preview;
}

function buildMarkdownPreviewText(markdown: string) {
  const preview = markdown
    .replace(/```[\s\S]*?```/g, "[code block]")
    .replace(/\[(.*?)\]\((.*?)\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^>\s?/gm, "")
    .replace(/^[*-]\s+/gm, "")
    .replace(/`/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();

  return buildDeferredPreviewText(preview);
}

function renderDecision(decision: Exclude<ApprovalDecision, "pending">) {
  switch (decision) {
    case "accepted":
      return "approved";
    case "acceptedForSession":
      return "approved for this session";
    case "canceled":
      return "canceled";
    case "rejected":
      return "rejected";
    case "interrupted":
      return "expired after restart";
  }
}

function labelForStatus(status: Session["status"]) {
  switch (status) {
    case "active":
      return "Active";
    case "idle":
      return "Idle";
    case "approval":
      return "Awaiting approval";
    case "error":
      return "Error";
  }
}

function labelForPaneViewMode(viewMode: PaneViewMode) {
  switch (viewMode) {
    case "session":
      return "Session";
    case "prompt":
      return "Prompt";
    case "commands":
      return "Commands";
    case "diffs":
      return "Diffs";
    case "controlPanel":
      return "Control panel";
    case "source":
      return "Source";
    case "filesystem":
      return "Files";
    case "gitStatus":
      return "Git status";
    case "instructionDebugger":
      return "Instructions";
    case "diffPreview":
      return "Diff preview";
  }
}

function isHexColorDark(value: string) {
  const hex = value.trim().replace(/^#/, "");
  if (hex.length !== 6) {
    return false;
  }

  const red = Number.parseInt(hex.slice(0, 2), 16);
  const green = Number.parseInt(hex.slice(2, 4), 16);
  const blue = Number.parseInt(hex.slice(4, 6), 16);
  const luminance = (red * 299 + green * 587 + blue * 114) / 1000;
  return luminance < 148;
}

function mapCommandStatus(status: CommandMessage["status"]): Session["status"] {
  switch (status) {
    case "success":
      return "idle";
    case "running":
      return "active";
    case "error":
      return "error";
  }
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}

function readNavigatorOnline() {
  if (typeof navigator === "undefined") {
    return true;
  }

  return navigator.onLine !== false;
}

function describeBackendConnectionState(state: BackendConnectionState) {
  switch (state) {
    case "connecting":
      return {
        detail: "Connecting to the TermAl backend.",
        icon: "spinner" as const,
        label: "Connecting",
        tone: "active" as const,
      };
    case "connected":
      return {
        detail: "Live updates are connected.",
        icon: "connected" as const,
        label: "Connected",
        tone: "idle" as const,
      };
    case "reconnecting":
      return {
        detail: "Waiting for the backend connection to recover.",
        icon: "spinner" as const,
        label: "Reconnecting",
        tone: "active" as const,
      };
    case "offline":
      return {
        detail: "The browser is offline or cannot reach the backend.",
        icon: "offline" as const,
        label: "Offline",
        tone: "error" as const,
      };
  }
}

function BackendConnectionStatus({ state }: { state: BackendConnectionState }) {
  const descriptor = describeBackendConnectionState(state);

  return (
    <div className="workspace-connection-status" role="status" aria-live="polite" title={descriptor.detail}>
      <span className={`chip chip-status chip-status-${descriptor.tone} workspace-connection-chip`}>
        {descriptor.icon === "spinner" ? (
          <span className="activity-spinner workspace-connection-spinner" aria-hidden="true" />
        ) : (
          <BackendConnectionIcon state={descriptor.icon} />
        )}
        <span className="visually-hidden">{descriptor.label}</span>
      </span>
    </div>
  );
}

function BackendConnectionIcon({ state }: { state: "connected" | "offline" }) {
  if (state === "connected") {
    return (
      <span className="workspace-connection-icon" aria-hidden="true">
        <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
          <path
            d="m4 8.2 2.2 2.2L12 4.6"
            fill="none"
            stroke="currentColor"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth="1.8"
          />
        </svg>
      </span>
    );
  }

  return (
    <span className="workspace-connection-icon" aria-hidden="true">
      <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
        <path
          d="M4.5 4.5 11.5 11.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.8"
        />
        <path
          d="M11.5 4.5 4.5 11.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.8"
        />
      </svg>
    </span>
  );
}

function primaryModifierLabel() {
  if (typeof navigator === "undefined") {
    return "Ctrl";
  }

  return navigator.platform.toLowerCase().includes("mac") ? "Cmd" : "Ctrl";
}

type ConnectionRetryNotice = {
  attemptLabel: string | null;
  detail: string;
};

function parseConnectionRetryNotice(text: string): ConnectionRetryNotice | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("Connection dropped before the response finished.")) {
    return null;
  }

  const attemptMatch = trimmed.match(/Retrying automatically \(attempt (\d+) of (\d+)\)\.?$/);
  const attemptLabel = attemptMatch ? `Attempt ${attemptMatch[1]} of ${attemptMatch[2]}` : null;

  return {
    attemptLabel,
    detail: trimmed,
  };
}

function buildMessageListSignature(messages: Message[]) {
  const lastMessage = messages[messages.length - 1];

  return [
    messages.length.toString(),
    lastMessage?.id ?? "no-message",
    lastMessage ? messageChangeMarker(lastMessage) : "empty",
  ].join("|");
}

function buildSessionConversationSignature(session: Session) {
  const messages = session.messages;
  const pendingPrompts = session.pendingPrompts ?? [];
  const lastMessage = messages[messages.length - 1];
  const lastPendingPrompt = pendingPrompts[pendingPrompts.length - 1];

  return [
    (messages.length + pendingPrompts.length).toString(),
    lastMessage?.id ?? "no-message",
    lastMessage ? messageChangeMarker(lastMessage) : "empty",
    lastPendingPrompt?.id ?? "no-pending-prompt",
    lastPendingPrompt ? pendingPromptChangeMarker(lastPendingPrompt) : "empty",
  ].join("|");
}

function messageChangeMarker(message: Message) {
  switch (message.type) {
    case "text":
      return `${message.type}:${message.text.length}:${message.attachments?.length ?? 0}`;
    case "thinking":
      return `${message.type}:${message.lines.length}:${message.title.length}`;
    case "command":
      return `${message.type}:${message.status}:${message.output.length}`;
    case "diff":
      return `${message.type}:${message.filePath}:${message.diff.length}`;
    case "markdown":
      return `${message.type}:${message.title.length}:${message.markdown.length}`;
    case "parallelAgents":
      return `${message.type}:${message.agents.length}:${message.agents
        .map((agent) => `${agent.id}:${agent.status}:${agent.detail?.length ?? 0}`)
        .join("|")}`;
    case "subagentResult":
      return `${message.type}:${message.title.length}:${message.summary.length}`;
    case "approval":
      return `${message.type}:${message.decision}:${message.command.length}`;
  }
}

function pendingPromptChangeMarker(prompt: PendingPrompt) {
  return `${prompt.text.length}:${prompt.attachments?.length ?? 0}`;
}

function collectCandidateSourcePaths(session: Session) {
  const paths = session.messages
    .filter((message): message is DiffMessage => message.type === "diff")
    .map((message) => message.filePath);

  return Array.from(new Set(paths));
}

function findLastUserPrompt(session: Session) {
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const message = session.messages[index];
    if (message.type === "text" && message.author === "you") {
      const prompt = message.text.trim();
      if (prompt) {
        return prompt;
      }
    }
  }

  return null;
}

function setSessionFlag(current: SessionFlagMap, sessionId: string, value: boolean): SessionFlagMap {
  if (value) {
    return {
      ...current,
      [sessionId]: true,
    };
  }

  if (!current[sessionId]) {
    return current;
  }

  const next = { ...current };
  delete next[sessionId];
  return next;
}

function pruneSessionValues<T>(
  current: Record<string, T>,
  availableSessionIds: Set<string>,
  invalidSessionIds?: Set<string>,
): Record<string, T> {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    return availableSessionIds.has(sessionId) && !invalidSessionIds?.has(sessionId);
  });
  return Object.fromEntries(nextEntries) as Record<string, T>;
}

function pruneSessionCommandValues(
  current: SessionAgentCommandMap,
  availableSessionIds: Set<string>,
  invalidSessionIds: Set<string>,
): SessionAgentCommandMap {
  return pruneSessionValues(current, availableSessionIds, invalidSessionIds);
}

function pruneSessionAttachmentValues(
  current: Record<string, DraftImageAttachment[]>,
  availableSessionIds: Set<string>,
): Record<string, DraftImageAttachment[]> {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    const keep = availableSessionIds.has(sessionId);
    if (!keep) {
      releaseDraftAttachments(current[sessionId] ?? []);
    }
    return keep;
  });
  return Object.fromEntries(nextEntries);
}

function pruneSessionFlags(current: SessionFlagMap, availableSessionIds: Set<string>): SessionFlagMap {
  const nextEntries = Object.entries(current).filter(([sessionId]) => availableSessionIds.has(sessionId));
  return Object.fromEntries(nextEntries);
}

function pruneSessionFlagsWithInvalidation(
  current: SessionFlagMap,
  availableSessionIds: Set<string>,
  invalidSessionIds: Set<string>,
): SessionFlagMap {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    return availableSessionIds.has(sessionId) && !invalidSessionIds.has(sessionId);
  });
  return Object.fromEntries(nextEntries);
}

function removeQueuedPromptFromSessions(
  sessions: Session[],
  sessionId: string,
  promptId: string,
): Session[] {
  return sessions.map((session) => {
    if (session.id !== sessionId || !session.pendingPrompts?.length) {
      return session;
    }

    const pendingPrompts = session.pendingPrompts.filter((prompt) => prompt.id !== promptId);
    if (pendingPrompts.length === session.pendingPrompts.length) {
      return session;
    }

    if (pendingPrompts.length === 0) {
      const { pendingPrompts: _discard, ...rest } = session;
      return rest;
    }

    return {
      ...session,
      pendingPrompts,
    };
  });
}

function releaseDraftAttachments(attachments: DraftImageAttachment[]) {
  for (const attachment of attachments) {
    URL.revokeObjectURL(attachment.previewUrl);
  }
}

function collectClipboardImageFiles(clipboardData: DataTransfer | null): File[] {
  if (!clipboardData) {
    return [];
  }

  return Array.from(clipboardData.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null && file.type.startsWith("image/"));
}

async function createDraftAttachmentsFromFiles(files: File[]) {
  const attachments: DraftImageAttachment[] = [];
  const errors: string[] = [];

  for (const [index, file] of files.entries()) {
    try {
      attachments.push(await createDraftAttachment(file, index));
    } catch (error) {
      errors.push(getErrorMessage(error));
    }
  }

  return { attachments, errors };
}

async function createDraftAttachment(file: File, index: number): Promise<DraftImageAttachment> {
  const mediaType = file.type.trim().toLowerCase();
  if (!SUPPORTED_PASTED_IMAGE_TYPES.has(mediaType)) {
    throw new Error(`Unsupported pasted image type \`${mediaType || "unknown"}\`.`);
  }

  if (file.size > MAX_PASTED_IMAGE_BYTES) {
    throw new Error(
      `Pasted image exceeds the ${Math.round(MAX_PASTED_IMAGE_BYTES / (1024 * 1024))} MB limit.`,
    );
  }

  const dataUrl = await readFileAsDataUrl(file);
  const [, base64Data = ""] = dataUrl.split(",", 2);
  if (!base64Data) {
    throw new Error("Failed to read pasted image data.");
  }

  const fileName = file.name.trim() || defaultDraftAttachmentFileName(index, mediaType);

  return {
    id: crypto.randomUUID(),
    previewUrl: URL.createObjectURL(file),
    base64Data,
    byteSize: file.size,
    fileName,
    mediaType,
  };
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => {
      reject(new Error(`Failed to read pasted image \`${file.name || "image"}\`.`));
    };
    reader.onload = () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error(`Failed to decode pasted image \`${file.name || "image"}\`.`));
      }
    };
    reader.readAsDataURL(file);
  });
}

function defaultDraftAttachmentFileName(index: number, mediaType: string) {
  const extension = mediaType === "image/png"
    ? "png"
    : mediaType === "image/jpeg"
      ? "jpg"
      : mediaType === "image/gif"
        ? "gif"
        : mediaType === "image/webp"
          ? "webp"
          : "img";

  return `pasted-image-${index + 1}.${extension}`;
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

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

function normalizeWheelDelta(
  event: Pick<ReactWheelEvent<HTMLElement>, "deltaMode" | "deltaY">,
  container: HTMLElement,
) {
  if (event.deltaMode === 1) {
    const computedLineHeight = Number.parseFloat(window.getComputedStyle(container).lineHeight);
    const lineHeight = Number.isFinite(computedLineHeight) ? computedLineHeight : 16;
    return event.deltaY * lineHeight;
  }

  if (event.deltaMode === 2) {
    return event.deltaY * Math.max(container.clientHeight, 1);
  }

  return event.deltaY;
}

function canNestedScrollableConsumeWheel(
  target: EventTarget | null,
  container: HTMLElement,
  deltaY: number,
) {
  let current = target instanceof HTMLElement ? target : null;

  while (current && current !== container) {
    const style = window.getComputedStyle(current);
    const overflowY = style.overflowY;
    const canScrollY =
      (overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay") &&
      current.scrollHeight > current.clientHeight + 1;

    if (canScrollY) {
      const maxScrollTop = current.scrollHeight - current.clientHeight;
      if (deltaY < 0 && current.scrollTop > 0) {
        return true;
      }
      if (deltaY > 0 && current.scrollTop < maxScrollTop) {
        return true;
      }
    }

    current = current.parentElement;
  }

  return false;
}

function dropLabelForPlacement(placement: TabDropPlacement) {
  switch (placement) {
    case "tabs":
      return "Tabs";
    case "left":
      return "Left";
    case "right":
      return "Right";
    case "top":
      return "Top";
    case "bottom":
      return "Bottom";
  }
}
