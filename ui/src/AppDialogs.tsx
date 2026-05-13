import {
  type ComponentProps,
  type CSSProperties,
  type RefObject,
  type ReactNode,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { isDialogBackdropDismissMouseDown } from "./dialog-backdrop-dismiss";
import { DialogCloseIcon } from "./message-card-icons";
import {
  createSessionModelHint,
  CLAUDE_EFFORT_OPTIONS,
  CODEX_REASONING_EFFORT_OPTIONS,
  type ComboboxOption,
} from "./session-model-utils";
import {
  remoteConnectionLabel,
  remoteDisplayName,
} from "./remotes";
import {
  ThemePreferencesPanel,
  AppearancePreferencesPanel,
  MarkdownPreferencesPanel,
  RemotePreferencesPanel,
  TelegramPreferencesPanel,
  ClaudeApprovalsPreferencesPanel,
  CodexPromptPreferencesPanel,
  CursorPreferencesPanel,
  GeminiPreferencesPanel,
  ThemedCombobox,
  CURSOR_MODE_OPTIONS,
  GEMINI_APPROVAL_OPTIONS,
} from "./preferences-panels";
import { SettingsDialogShell } from "./preferences/SettingsDialogShell";
import { SettingsTabBar } from "./preferences/SettingsTabBar";
import type { PreferencesTabId } from "./preferences/preferences-tabs";
import { OrchestratorTemplatesPanel } from "./panels/OrchestratorTemplatesPanel";
import type { StateResponse } from "./api";
import type {
  AgentReadiness,
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CursorMode,
  GeminiApprovalMode,
  Project,
  RemoteConfig,
  SandboxMode,
  Session,
} from "./types";
import {
  clampDensityPreference,
  clampEditorFontSizePreference,
  clampFontSizePreference,
} from "./themes";
import { NEW_SESSION_AGENT_OPTIONS } from "./app-shell-internals";

type ThemePanelProps = ComponentProps<typeof ThemePreferencesPanel>;
type MarkdownPanelProps = ComponentProps<typeof MarkdownPreferencesPanel>;
type AppearancePanelProps = ComponentProps<typeof AppearancePreferencesPanel>;
type RemotePanelProps = ComponentProps<typeof RemotePreferencesPanel>;
type TelegramPanelProps = ComponentProps<typeof TelegramPreferencesPanel>;
type CodexPromptPanelProps = ComponentProps<typeof CodexPromptPreferencesPanel>;
type ClaudeApprovalsPanelProps = ComponentProps<typeof ClaudeApprovalsPreferencesPanel>;

type AppDialogsProps = {
  pendingKillSession: Session | null;
  pendingKillPopoverRef: RefObject<HTMLDivElement | null>;
  pendingKillPopoverStyle: CSSProperties | null;
  pendingKillConfirmButtonRef: RefObject<HTMLButtonElement | null>;
  schedulePendingKillConfirmationClose: () => void;
  clearPendingKillCloseTimeout: () => void;
  closePendingKillConfirmation: (restoreFocus?: boolean) => void;
  confirmKillSession: () => Promise<void>;
  pendingSessionRenameSession: Session | null;
  pendingSessionRenamePopoverRef: RefObject<HTMLFormElement | null>;
  pendingSessionRenameStyle: CSSProperties | null;
  pendingSessionRenameInputRef: RefObject<HTMLInputElement | null>;
  pendingSessionRenameDraft: string;
  pendingSessionRenameValue: string;
  isPendingSessionRenameCreating: boolean;
  isPendingSessionRenameSubmitting: boolean;
  isPendingSessionRenameKilling: boolean;
  schedulePendingSessionRenameClose: () => void;
  clearPendingSessionRenameCloseTimeout: () => void;
  closePendingSessionRename: (restoreFocus?: boolean) => void;
  confirmSessionRename: () => Promise<void>;
  setPendingSessionRenameDraft: (nextValue: string) => void;
  handlePendingSessionRenameNew: () => Promise<void>;
  handlePendingSessionRenameKill: () => Promise<void>;
  requestError: string | null;
  isCreateSessionOpen: boolean;
  isCreating: boolean;
  closeCreateSessionDialog: () => void;
  handleCreateSessionDialogSubmit: () => Promise<void>;
  newSessionAgent: AgentType;
  onChangeNewSessionAgent: (nextValue: AgentType) => void;
  createSessionUsesSessionModelPicker: boolean;
  newSessionModel: string;
  newSessionModelOptions: readonly ComboboxOption[];
  onChangeNewSessionModel: (nextValue: string) => void;
  defaultCodexModel: string;
  handleDefaultCodexModelChange: (nextValue: string) => void;
  defaultCodexReasoningEffort: CodexReasoningEffort;
  handleDefaultCodexReasoningEffortChange: (nextValue: CodexReasoningEffort) => void;
  defaultClaudeModel: string;
  handleDefaultClaudeModelChange: (nextValue: string) => void;
  defaultClaudeEffort: ClaudeEffortLevel;
  handleDefaultClaudeEffortChange: (nextValue: ClaudeEffortLevel) => void;
  defaultCursorModel: string;
  handleDefaultCursorModelChange: (nextValue: string) => void;
  defaultCursorMode: CursorMode;
  onChangeDefaultCursorMode: (nextValue: CursorMode) => void;
  defaultGeminiModel: string;
  handleDefaultGeminiModelChange: (nextValue: string) => void;
  defaultGeminiApprovalMode: GeminiApprovalMode;
  onChangeDefaultGeminiApprovalMode: (nextValue: GeminiApprovalMode) => void;
  createSessionProjectId: string;
  createSessionProjectOptions: readonly ComboboxOption[];
  onChangeCreateSessionProjectId: (nextValue: string) => void;
  createSessionProjectHint: string;
  createSessionProjectSelectionError: string | null;
  createSessionAgentReadiness: AgentReadiness | null;
  createSessionBlocked: boolean;
  isCreateProjectOpen: boolean;
  isCreatingProject: boolean;
  closeCreateProjectDialog: () => void;
  handleCreateProject: () => Promise<boolean>;
  newProjectRemoteId: string;
  createProjectRemoteOptions: readonly ComboboxOption[];
  onChangeNewProjectRemoteId: (nextValue: string) => void;
  newProjectSelectedRemote: RemoteConfig | null;
  newProjectUsesLocalRemote: boolean;
  newProjectRootPath: string;
  onChangeNewProjectRootPath: (nextValue: string) => void;
  handlePickProjectRoot: () => Promise<void>;
  isSettingsOpen: boolean;
  closeSettingsDialog: () => void;
  settingsTab: PreferencesTabId;
  setSettingsTab: (nextTab: PreferencesTabId) => void;
  activeStyle: ThemePanelProps["activeStyle"];
  activeTheme: ThemePanelProps["activeTheme"];
  styleId: ThemePanelProps["styleId"];
  themeId: ThemePanelProps["themeId"];
  setStyleId: ThemePanelProps["onSelectStyle"];
  setThemeId: ThemePanelProps["onSelectTheme"];
  activeMarkdownTheme: MarkdownPanelProps["activeMarkdownTheme"];
  activeMarkdownStyle: MarkdownPanelProps["activeMarkdownStyle"];
  markdownThemeId: MarkdownPanelProps["markdownThemeId"];
  markdownStyleId: MarkdownPanelProps["markdownStyleId"];
  diagramThemeOverrideMode: MarkdownPanelProps["diagramThemeOverrideMode"];
  diagramLook: MarkdownPanelProps["diagramLook"];
  diagramPalette: MarkdownPanelProps["diagramPalette"];
  setMarkdownThemeId: MarkdownPanelProps["onSelectMarkdownTheme"];
  setMarkdownStyleId: MarkdownPanelProps["onSelectMarkdownStyle"];
  setDiagramThemeOverrideMode: MarkdownPanelProps["onSelectDiagramThemeOverrideMode"];
  setDiagramLook: MarkdownPanelProps["onSelectDiagramLook"];
  setDiagramPalette: MarkdownPanelProps["onSelectDiagramPalette"];
  densityPercent: AppearancePanelProps["densityPercent"];
  editorFontSizePx: AppearancePanelProps["editorFontSizePx"];
  fontSizePx: AppearancePanelProps["fontSizePx"];
  setDensityPercent: (nextValue: number) => void;
  setEditorFontSizePx: (nextValue: number) => void;
  setFontSizePx: (nextValue: number) => void;
  remoteConfigs: RemotePanelProps["remotes"];
  onSaveRemotes: RemotePanelProps["onSaveRemotes"];
  projects: TelegramPanelProps["projects"];
  sessions: TelegramPanelProps["sessions"];
  handleOrchestratorStateUpdated: (state: StateResponse) => void;
  defaultCodexApprovalPolicy: ApprovalPolicy;
  defaultCodexSandboxMode: SandboxMode;
  setDefaultCodexApprovalPolicy: CodexPromptPanelProps["onSelectApprovalPolicy"];
  setDefaultCodexSandboxMode: CodexPromptPanelProps["onSelectSandboxMode"];
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  setDefaultClaudeApprovalMode: ClaudeApprovalsPanelProps["onSelectMode"];
};

function SettingsTabPanelScrollFrame({
  activeTabId,
  className,
  children,
}: {
  activeTabId: PreferencesTabId;
  className: string;
  children: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const dragStateRef = useRef<{
    pointerId: number;
    startPointerY: number;
    startScrollTop: number;
  } | null>(null);
  const [scrollState, setScrollState] = useState({
    thumbHeight: 0,
    thumbOffset: 0,
    visible: false,
  });
  const [isDraggingScrollbar, setIsDraggingScrollbar] = useState(false);

  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel) {
      return;
    }
    const panelElement = panel;

    function updateScrollState() {
      const scrollable = panelElement.scrollHeight - panelElement.clientHeight;
      if (scrollable <= 1) {
        setScrollState({ thumbHeight: 0, thumbOffset: 0, visible: false });
        return;
      }

      const trackHeight = panelElement.clientHeight;
      const thumbHeight = Math.max(
        Math.round((panelElement.clientHeight / panelElement.scrollHeight) * trackHeight),
        34,
      );
      const maxThumbOffset = Math.max(trackHeight - thumbHeight, 0);
      const thumbOffset =
        maxThumbOffset <= 0
          ? 0
          : Math.round((panelElement.scrollTop / scrollable) * maxThumbOffset);

      setScrollState({
        thumbHeight,
        thumbOffset,
        visible: true,
      });
    }

    updateScrollState();
    const ResizeObserverCtor = globalThis.ResizeObserver;
    const resizeObserver =
      typeof ResizeObserverCtor === "function"
        ? new ResizeObserverCtor(updateScrollState)
        : null;
    if (resizeObserver) {
      resizeObserver.observe(panelElement);
      for (const child of Array.from(panelElement.children)) {
        resizeObserver.observe(child);
      }
    } else {
      window.addEventListener("resize", updateScrollState);
    }
    panelElement.addEventListener("scroll", updateScrollState, { passive: true });

    return () => {
      resizeObserver?.disconnect();
      if (!resizeObserver) {
        window.removeEventListener("resize", updateScrollState);
      }
      panelElement.removeEventListener("scroll", updateScrollState);
      dragStateRef.current = null;
      setIsDraggingScrollbar(false);
    };
  }, [activeTabId, children]);

  function scrollPanelByTrackPosition(clientY: number) {
    const panel = panelRef.current;
    if (!panel || !scrollState.visible) {
      return;
    }

    const rect = panel.getBoundingClientRect();
    const trackHeight = rect.height;
    const maxThumbOffset = Math.max(trackHeight - scrollState.thumbHeight, 1);
    const nextThumbOffset = Math.min(
      Math.max(clientY - rect.top - scrollState.thumbHeight / 2, 0),
      maxThumbOffset,
    );
    const scrollable = panel.scrollHeight - panel.clientHeight;
    panel.scrollTop = (nextThumbOffset / maxThumbOffset) * scrollable;
  }

  return (
    <div className="settings-tab-panel-frame">
      <div
        ref={panelRef}
        id={`settings-panel-${activeTabId}`}
        className={className}
        role="tabpanel"
        aria-labelledby={`settings-tab-${activeTabId}`}
      >
        {children}
      </div>
      {scrollState.visible ? (
        <div
          className={`settings-scrollbar ${isDraggingScrollbar ? "dragging" : ""}`}
          aria-hidden="true"
          onPointerDown={(event) => {
            event.preventDefault();
            event.currentTarget.setPointerCapture(event.pointerId);
            setIsDraggingScrollbar(true);
            dragStateRef.current = {
              pointerId: event.pointerId,
              startPointerY: event.clientY,
              startScrollTop: panelRef.current?.scrollTop ?? 0,
            };
            scrollPanelByTrackPosition(event.clientY);
          }}
          onPointerMove={(event) => {
            const dragState = dragStateRef.current;
            const panel = panelRef.current;
            if (!dragState || dragState.pointerId !== event.pointerId || !panel) {
              return;
            }

            event.preventDefault();
            const trackHeight = panel.clientHeight;
            const maxThumbOffset = Math.max(
              trackHeight - scrollState.thumbHeight,
              1,
            );
            const scrollable = panel.scrollHeight - panel.clientHeight;
            const deltaY = event.clientY - dragState.startPointerY;
            panel.scrollTop =
              dragState.startScrollTop + (deltaY / maxThumbOffset) * scrollable;
          }}
          onPointerUp={(event) => {
            if (dragStateRef.current?.pointerId === event.pointerId) {
              dragStateRef.current = null;
              setIsDraggingScrollbar(false);
            }
          }}
          onPointerCancel={() => {
            dragStateRef.current = null;
            setIsDraggingScrollbar(false);
          }}
        >
          <div
            className="settings-scrollbar-thumb"
            style={{
              height: `${scrollState.thumbHeight}px`,
              transform: `translateY(${scrollState.thumbOffset}px)`,
            }}
          />
        </div>
      ) : null}
    </div>
  );
}

export function AppDialogs({
  pendingKillSession,
  pendingKillPopoverRef,
  pendingKillPopoverStyle,
  pendingKillConfirmButtonRef,
  schedulePendingKillConfirmationClose,
  clearPendingKillCloseTimeout,
  closePendingKillConfirmation,
  confirmKillSession,
  pendingSessionRenameSession,
  pendingSessionRenamePopoverRef,
  pendingSessionRenameStyle,
  pendingSessionRenameInputRef,
  pendingSessionRenameDraft,
  pendingSessionRenameValue,
  isPendingSessionRenameCreating,
  isPendingSessionRenameSubmitting,
  isPendingSessionRenameKilling,
  schedulePendingSessionRenameClose,
  clearPendingSessionRenameCloseTimeout,
  closePendingSessionRename,
  confirmSessionRename,
  setPendingSessionRenameDraft,
  handlePendingSessionRenameNew,
  handlePendingSessionRenameKill,
  requestError,
  isCreateSessionOpen,
  isCreating,
  closeCreateSessionDialog,
  handleCreateSessionDialogSubmit,
  newSessionAgent,
  onChangeNewSessionAgent,
  createSessionUsesSessionModelPicker,
  newSessionModel,
  newSessionModelOptions,
  onChangeNewSessionModel,
  defaultCodexModel,
  handleDefaultCodexModelChange,
  defaultCodexReasoningEffort,
  handleDefaultCodexReasoningEffortChange,
  defaultClaudeModel,
  handleDefaultClaudeModelChange,
  defaultClaudeEffort,
  handleDefaultClaudeEffortChange,
  defaultCursorModel,
  handleDefaultCursorModelChange,
  defaultCursorMode,
  onChangeDefaultCursorMode,
  defaultGeminiModel,
  handleDefaultGeminiModelChange,
  defaultGeminiApprovalMode,
  onChangeDefaultGeminiApprovalMode,
  createSessionProjectId,
  createSessionProjectOptions,
  onChangeCreateSessionProjectId,
  createSessionProjectHint,
  createSessionProjectSelectionError,
  createSessionAgentReadiness,
  createSessionBlocked,
  isCreateProjectOpen,
  isCreatingProject,
  closeCreateProjectDialog,
  handleCreateProject,
  newProjectRemoteId,
  createProjectRemoteOptions,
  onChangeNewProjectRemoteId,
  newProjectSelectedRemote,
  newProjectUsesLocalRemote,
  newProjectRootPath,
  onChangeNewProjectRootPath,
  handlePickProjectRoot,
  isSettingsOpen,
  closeSettingsDialog,
  settingsTab,
  setSettingsTab,
  activeStyle,
  activeTheme,
  styleId,
  themeId,
  setStyleId,
  setThemeId,
  activeMarkdownTheme,
  activeMarkdownStyle,
  markdownThemeId,
  markdownStyleId,
  diagramThemeOverrideMode,
  diagramLook,
  diagramPalette,
  setMarkdownThemeId,
  setMarkdownStyleId,
  setDiagramThemeOverrideMode,
  setDiagramLook,
  setDiagramPalette,
  densityPercent,
  editorFontSizePx,
  fontSizePx,
  setDensityPercent,
  setEditorFontSizePx,
  setFontSizePx,
  remoteConfigs,
  onSaveRemotes,
  projects,
  sessions,
  handleOrchestratorStateUpdated,
  defaultCodexApprovalPolicy,
  defaultCodexSandboxMode,
  setDefaultCodexApprovalPolicy,
  setDefaultCodexSandboxMode,
  defaultClaudeApprovalMode,
  setDefaultClaudeApprovalMode,
}: AppDialogsProps): JSX.Element {
  return (
    <>
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
                ref={pendingKillPopoverRef as RefObject<HTMLDivElement>}
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
                    ref={pendingKillConfirmButtonRef as RefObject<HTMLButtonElement>}
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
                ref={pendingSessionRenamePopoverRef as RefObject<HTMLFormElement>}
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
                  ref={pendingSessionRenameInputRef as RefObject<HTMLInputElement>}
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
          onMouseDown={(event) => {
            if (!isDialogBackdropDismissMouseDown(event.nativeEvent)) {
              return;
            }
            if (!isCreating) {
              closeCreateSessionDialog();
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
                  Pick the assistant, project, and any startup settings before
                  opening the session. Session-specific controls stay with the
                  session after it starts.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={closeCreateSessionDialog}
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
                <label
                  className="session-control-label"
                  htmlFor="create-session-agent"
                >
                  Assistant
                </label>
                <ThemedCombobox
                  id="create-session-agent"
                  value={newSessionAgent}
                  options={NEW_SESSION_AGENT_OPTIONS as readonly ComboboxOption[]}
                  onChange={(nextValue) =>
                    onChangeNewSessionAgent(nextValue as AgentType)
                  }
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
                  <label
                    className="session-control-label"
                    htmlFor="create-session-model"
                  >
                    Model
                  </label>
                  <ThemedCombobox
                    id="create-session-model"
                    value={newSessionModel}
                    options={newSessionModelOptions}
                    onChange={(nextValue) =>
                      onChangeNewSessionModel(nextValue)
                    }
                    disabled={isCreating}
                  />
                </div>
              )}

              {newSessionAgent === "Codex" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-codex-reasoning-effort"
                  >
                    Codex reasoning effort
                  </label>
                  <ThemedCombobox
                    id="create-session-codex-reasoning-effort"
                    value={defaultCodexReasoningEffort}
                    options={
                      CODEX_REASONING_EFFORT_OPTIONS as readonly ComboboxOption[]
                    }
                    onChange={(nextValue) =>
                      handleDefaultCodexReasoningEffortChange(
                        nextValue as CodexReasoningEffort,
                      )
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    New Codex sessions start with this reasoning effort, and you
                    can still change it per session later.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Claude" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-claude-effort"
                  >
                    Claude effort
                  </label>
                  <ThemedCombobox
                    id="create-session-claude-effort"
                    value={defaultClaudeEffort}
                    options={CLAUDE_EFFORT_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      handleDefaultClaudeEffortChange(
                        nextValue as ClaudeEffortLevel,
                      )
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    New Claude sessions start with this effort, and you can
                    still change it per session later.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Cursor" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-cursor-mode"
                  >
                    Cursor mode
                  </label>
                  <ThemedCombobox
                    id="create-session-cursor-mode"
                    value={defaultCursorMode}
                    options={CURSOR_MODE_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      onChangeDefaultCursorMode(nextValue as CursorMode)
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    Agent auto-approves tool requests and can edit, Ask keeps
                    approval cards, and Plan stays read-only.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Gemini" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-gemini-mode"
                  >
                    Gemini approvals
                  </label>
                  <ThemedCombobox
                    id="create-session-gemini-mode"
                    value={defaultGeminiApprovalMode}
                    options={GEMINI_APPROVAL_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      onChangeDefaultGeminiApprovalMode(
                        nextValue as GeminiApprovalMode,
                      )
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    Default prompts for approval, Auto edit approves edit tools,
                    YOLO approves all tools, and Plan keeps Gemini read-only.
                  </p>
                </div>
              ) : null}

              <div className="create-session-field">
                <label
                  className="session-control-label"
                  htmlFor="create-session-project"
                >
                  Project
                </label>
                <ThemedCombobox
                  id="create-session-project"
                  value={createSessionProjectId}
                  options={createSessionProjectOptions}
                  onChange={onChangeCreateSessionProjectId}
                  disabled={isCreating}
                />
                <p className="create-session-field-hint">
                  {createSessionProjectHint}
                </p>
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
                    {createSessionAgentReadiness.blocking
                      ? "Setup Required"
                      : "Ready"}
                  </div>
                  <p>{createSessionAgentReadiness.detail}</p>
                  {createSessionAgentReadiness.commandPath ? (
                    <p className="create-session-field-hint">
                      Binary: {createSessionAgentReadiness.commandPath}
                    </p>
                  ) : null}
                </article>
              ) : null}

              {createSessionAgentReadiness?.warningDetail ? (
                <article className="thread-notice create-session-readiness">
                  <div className="card-label">Warning</div>
                  <p>{createSessionAgentReadiness.warningDetail}</p>
                </article>
              ) : null}

              <div className="dialog-actions create-session-dialog-actions">
                <button
                  className="ghost-button"
                  type="button"
                  onClick={closeCreateSessionDialog}
                  disabled={isCreating}
                >
                  Cancel
                </button>
                <button
                  className="send-button create-session-submit"
                  type="submit"
                  disabled={
                    isCreating ||
                    createSessionBlocked ||
                    !!createSessionProjectSelectionError
                  }
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
          onMouseDown={(event) => {
            if (!isDialogBackdropDismissMouseDown(event.nativeEvent)) {
              return;
            }
            if (!isCreatingProject) {
              closeCreateProjectDialog();
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
                  Choose a local folder or enter a remote root path to add a
                  scoped project.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={closeCreateProjectDialog}
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
                    closeCreateProjectDialog();
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
                <label
                  className="session-control-label"
                  htmlFor="create-project-remote"
                >
                  Remote
                </label>
                <ThemedCombobox
                  id="create-project-remote"
                  value={newProjectRemoteId}
                  options={createProjectRemoteOptions}
                  onChange={onChangeNewProjectRemoteId}
                  disabled={isCreatingProject}
                />
                <p className="create-session-field-hint">
                  {remoteDisplayName(
                    newProjectSelectedRemote,
                    newProjectRemoteId,
                  )}{" "}
                  - {remoteConnectionLabel(newProjectSelectedRemote)}
                </p>
              </div>

              <div className="create-project-field">
                <label
                  className="session-control-label"
                  htmlFor="create-project-root"
                >
                  {newProjectUsesLocalRemote ? "Folder" : "Remote root path"}
                </label>
                <input
                  id="create-project-root"
                  className="themed-input project-root-input"
                  type="text"
                  value={newProjectRootPath}
                  placeholder={
                    newProjectUsesLocalRemote
                      ? "/path/to/project"
                      : "/remote/path/to/project"
                  }
                  onChange={(event) =>
                    onChangeNewProjectRootPath(event.target.value)
                  }
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
                  onClick={closeCreateProjectDialog}
                  disabled={isCreatingProject}
                >
                  Cancel
                </button>
                <button
                  className="send-button create-project-submit"
                  type="submit"
                  disabled={isCreatingProject}
                >
                  {isCreatingProject ? "Adding..." : "Add project"}
                </button>
              </div>
            </form>
          </section>
        </div>
      ) : null}
      {isSettingsOpen ? (
        <SettingsDialogShell onClose={closeSettingsDialog}>
          <div className="settings-dialog-content">
            <SettingsTabBar
              activeTabId={settingsTab}
              onSelectTab={setSettingsTab}
            />

            <SettingsTabPanelScrollFrame
              activeTabId={settingsTab}
              className={`settings-tab-panel ${settingsTab === "themes" ? "theme-settings-panel" : ""}`.trim()}
            >
              {settingsTab === "themes" ? (
                <ThemePreferencesPanel
                  activeStyle={activeStyle}
                  activeTheme={activeTheme}
                  styleId={styleId}
                  themeId={themeId}
                  onSelectStyle={setStyleId}
                  onSelectTheme={setThemeId}
                />
              ) : settingsTab === "markdown" ? (
                <MarkdownPreferencesPanel
                  activeMarkdownTheme={activeMarkdownTheme}
                  activeMarkdownStyle={activeMarkdownStyle}
                  markdownThemeId={markdownThemeId}
                  markdownStyleId={markdownStyleId}
                  diagramThemeOverrideMode={diagramThemeOverrideMode}
                  diagramLook={diagramLook}
                  diagramPalette={diagramPalette}
                  onSelectMarkdownTheme={setMarkdownThemeId}
                  onSelectMarkdownStyle={setMarkdownStyleId}
                  onSelectDiagramThemeOverrideMode={setDiagramThemeOverrideMode}
                  onSelectDiagramLook={setDiagramLook}
                  onSelectDiagramPalette={setDiagramPalette}
                />
              ) : settingsTab === "appearance" ? (
                <AppearancePreferencesPanel
                  densityPercent={densityPercent}
                  editorFontSizePx={editorFontSizePx}
                  fontSizePx={fontSizePx}
                  onSelectDensity={(nextValue) =>
                    setDensityPercent(clampDensityPreference(nextValue))
                  }
                  onSelectEditorFontSize={(nextValue) =>
                    setEditorFontSizePx(
                      clampEditorFontSizePreference(nextValue),
                    )
                  }
                  onSelectFontSize={(nextValue) =>
                    setFontSizePx(clampFontSizePreference(nextValue))
                  }
                />
              ) : settingsTab === "remotes" ? (
                <RemotePreferencesPanel
                  remotes={remoteConfigs}
                  onSaveRemotes={onSaveRemotes}
                />
              ) : settingsTab === "telegram" ? (
                <TelegramPreferencesPanel projects={projects} sessions={sessions} />
              ) : settingsTab === "orchestrators" ? (
                <OrchestratorTemplatesPanel
                  projects={projects}
                  sessions={sessions}
                  onStateUpdated={handleOrchestratorStateUpdated}
                />
              ) : settingsTab === "codex-prompts" ? (
                <CodexPromptPreferencesPanel
                  defaultApprovalPolicy={defaultCodexApprovalPolicy}
                  defaultModel={defaultCodexModel}
                  defaultReasoningEffort={defaultCodexReasoningEffort}
                  defaultSandboxMode={defaultCodexSandboxMode}
                  onSelectApprovalPolicy={setDefaultCodexApprovalPolicy}
                  onSelectModel={handleDefaultCodexModelChange}
                  onSelectReasoningEffort={
                    handleDefaultCodexReasoningEffortChange
                  }
                  onSelectSandboxMode={setDefaultCodexSandboxMode}
                  sessions={sessions}
                />
              ) : settingsTab === "claude-approvals" ? (
                <ClaudeApprovalsPreferencesPanel
                  defaultClaudeApprovalMode={defaultClaudeApprovalMode}
                  defaultClaudeEffort={defaultClaudeEffort}
                  defaultClaudeModel={defaultClaudeModel}
                  onSelectEffort={handleDefaultClaudeEffortChange}
                  onSelectModel={handleDefaultClaudeModelChange}
                  onSelectMode={setDefaultClaudeApprovalMode}
                  sessions={sessions}
                />
              ) : settingsTab === "cursor" ? (
                <CursorPreferencesPanel
                  defaultCursorModel={defaultCursorModel}
                  defaultCursorMode={defaultCursorMode}
                  onSelectModel={handleDefaultCursorModelChange}
                  onSelectMode={onChangeDefaultCursorMode}
                />
              ) : settingsTab === "gemini" ? (
                <GeminiPreferencesPanel
                  defaultGeminiApprovalMode={defaultGeminiApprovalMode}
                  defaultGeminiModel={defaultGeminiModel}
                  onSelectApprovalMode={onChangeDefaultGeminiApprovalMode}
                  onSelectModel={handleDefaultGeminiModelChange}
                />
              ) : (
                (() => {
                  const _exhaustive: never = settingsTab;
                  return _exhaustive;
                })()
              )}
            </SettingsTabPanelScrollFrame>
          </div>
        </SettingsDialogShell>
      ) : null}
    </>
  );
}
