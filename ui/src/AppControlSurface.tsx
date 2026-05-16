import {
  type Dispatch,
  type DragEvent as ReactDragEvent,
  type JSX,
  type RefObject,
  type SetStateAction,
} from "react";
import { AgentIcon } from "./agent-icon";
import {
  describeProjectScope,
  type ComboboxOption,
} from "./session-model-utils";
import { ProjectListSection } from "./ProjectListSection";
import {
  ControlPanelConnectionIndicator,
  WorkspaceSwitcher,
} from "./workspace-shell-controls";
import { OrchestratorRuntimeActionButton } from "./OrchestratorRuntimeActionButton";
import {
  buildControlSurfaceSessionListEntries,
  buildControlSurfaceSessionListState,
  createControlPanelSectionLauncherTab,
  formatSessionOrchestratorGroupName,
} from "./control-surface-state";
import {
  ControlPanelSurface,
  type ControlPanelSectionId,
  type ControlPanelSurfaceHandle,
} from "./panels/ControlPanelSurface";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { OrchestratorTemplateLibraryPanel } from "./panels/OrchestratorTemplateLibraryPanel";
import { ThemedCombobox } from "./preferences-panels";
import { attachSessionDragData } from "./session-drag";
import { primaryModifierLabel, type SessionFlagMap } from "./app-utils";
import {
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";
import { useControlSurfaceScope } from "./app-control-panel-state";
import type { BackendConnectionState } from "./backend-connection";
import type {
  GitDiffRequestPayload,
  GitDiffSection,
  OpenPathOptions,
  StateResponse,
  WorkspaceLayoutSummary,
} from "./api";
import type {
  OrchestratorInstance,
  Project,
  RemoteConfig,
  Session,
  WorkspaceFilesChangedEvent,
} from "./types";
import {
  filterSessionListVisibleSessions,
  type SessionListFilter,
} from "./session-list-filter";
import type { SessionListSearchResult } from "./session-find";
import type {
  OrchestratorRuntimeAction,
  StandaloneControlSurfaceViewState,
} from "./app-shell-internals";

type AppControlSurfaceProps = {
  paneId: string;
  fixedSection?: ControlPanelSectionId | null;
  controlPanelSurfaceRef: RefObject<ControlPanelSurfaceHandle | null>;
  collapsedSessionOrchestratorIdsBySurfaceId: Record<string, string[]>;
  paneLookup: Map<string, WorkspacePane>;
  sessionLookup: Map<string, Session>;
  workspace: WorkspaceState;
  standaloneControlSurfaceViewStateByTabId: Record<string, StandaloneControlSurfaceViewState>;
  projectLookup: Map<string, Project>;
  selectedProjectId: string;
  activeSession: Session | null;
  sessions: Session[];
  orchestrators: OrchestratorInstance[];
  openSessionIds: Set<string>;
  sessionListFilter: SessionListFilter;
  setSessionListFilter: Dispatch<SetStateAction<SessionListFilter>>;
  sessionListSearchQuery: string;
  setSessionListSearchQuery: Dispatch<SetStateAction<string>>;
  sessionFilterCounts: Record<SessionListFilter, number>;
  hasSessionListSearch: boolean;
  sessionListSearchResults: Map<string, SessionListSearchResult>;
  filteredSessions: Session[];
  controlPanelFilesystemRoot: string | null;
  controlPanelGitWorkdir: string | null;
  controlPanelGitStatusCount: number;
  setControlPanelGitStatusCount: Dispatch<SetStateAction<number>>;
  workspaceFilesChangedEvent: WorkspaceFilesChangedEvent | null;
  projects: Project[];
  projectSessionCounts: Map<string, number>;
  remoteLookup: Map<string, RemoteConfig>;
  projectScopedSessions: Session[];
  isSettingsOpen: boolean;
  setIsSettingsOpen: Dispatch<SetStateAction<boolean>>;
  isCreateSessionOpen: boolean;
  isCreating: boolean;
  isCreatingProject: boolean;
  controlPanelProjectOptions: readonly ComboboxOption[];
  controlPanelInlineIssueDetail: string | null;
  backendConnectionState: BackendConnectionState;
  workspaceViewId: string;
  deletingWorkspaceIds: string[];
  workspaceSwitcherError: string | null;
  isWorkspaceSwitcherLoading: boolean;
  isWorkspaceSwitcherOpen: boolean;
  workspaceSummaries: WorkspaceLayoutSummary[];
  workspaceSwitcherRef: RefObject<HTMLDivElement | null>;
  windowId: string;
  pendingOrchestratorActionById: Record<string, OrchestratorRuntimeAction | undefined>;
  killingSessionIds: SessionFlagMap;
  pendingKillSessionId: string | null;
  killRevealSessionId: string | null;
  sessionListSearchInputRef: RefObject<HTMLInputElement | null>;
  setKillRevealSessionId: Dispatch<SetStateAction<string | null>>;
  setControlPanelFilesystemRoot: Dispatch<SetStateAction<string | null>>;
  setControlPanelGitWorkdir: Dispatch<SetStateAction<string | null>>;
  setStandaloneControlSurfaceViewStateByTabId: Dispatch<SetStateAction<Record<string, StandaloneControlSurfaceViewState>>>;
  setCollapsedSessionOrchestratorIdsBySurfaceId: Dispatch<SetStateAction<Record<string, string[]>>>;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  handleSidebarSessionClick: (sessionId: string, preferredPaneId?: string | null, syncControlPanelProject?: boolean) => void;
  handleKillSession: (sessionId: string, trigger?: HTMLButtonElement | null) => void;
  handleSessionRenameRequest: (sessionId: string, clientX: number, clientY: number, trigger?: HTMLElement | null) => void;
  handleControlPanelLauncherDragStart: (event: ReactDragEvent<HTMLButtonElement>, paneId: string, sectionId: ControlPanelSectionId, tab: WorkspaceTab) => void;
  handleControlPanelLauncherDragEnd: () => void;
  handleOpenFilesystemTab: (paneId: string, rootPath: string | null, originSessionId: string | null, originProjectId: string | null) => void;
  handleOpenGitStatusTab: (paneId: string, workdir: string | null, originSessionId: string | null, originProjectId: string | null) => void;
  handleOpenGitStatusDiffPreviewTab: (paneId: string, request: GitDiffRequestPayload, originSessionId: string | null, originProjectId: string | null, options?: { openInNewTab?: boolean; sectionId?: GitDiffSection }) => Promise<void>;
  handleOpenProjectListTab: (paneId: string, originSessionId: string | null, originProjectId: string | null) => void;
  handleOpenOrchestratorListTab: (paneId: string, originSessionId: string | null, originProjectId: string | null) => void;
  handleOpenOrchestratorCanvasTab: (paneId: string, originSessionId: string | null, originProjectId: string | null, options?: { startMode?: "new" | null; templateId?: string | null }) => void;
  handleOpenSessionListTab: (paneId: string, originSessionId: string | null, originProjectId: string | null) => void;
  handleOpenCanvasTab: (paneId: string, originSessionId: string | null, originProjectId: string | null) => void;
  openCreateProjectDialog: () => void;
  openCreateSessionDialog: (preferredPaneId?: string | null, defaultProjectSelectionId?: string | null) => void;
  handleOpenSourceTab: (paneId: string, path: string | null, originSessionId: string | null, originProjectId: string | null, options?: OpenPathOptions) => void;
  handleOrchestratorStateUpdated: (state: StateResponse) => void;
  handleProjectMenuRemoveProject: (project: Project) => Promise<void>;
  handleProjectMenuStartSession: (paneId: string | null, projectId: string) => void;
  handleOrchestratorRuntimeAction: (instanceId: string, action: OrchestratorRuntimeAction) => Promise<void>;
  handleDeleteWorkspace: (workspaceId: string) => Promise<void>;
  handleOpenNewWorkspaceHere: () => void;
  handleOpenNewWorkspaceWindow: () => void;
  handleOpenWorkspaceHere: (nextWorkspaceViewId: string) => void;
  handleWorkspaceSwitcherToggle: () => void;
  handleRetryBackendConnection: () => void;
};

export function AppControlSurface({
  paneId,
  fixedSection = null,
  controlPanelSurfaceRef,
  collapsedSessionOrchestratorIdsBySurfaceId,
  paneLookup,
  sessionLookup,
  workspace,
  standaloneControlSurfaceViewStateByTabId,
  projectLookup,
  selectedProjectId,
  activeSession,
  sessions,
  orchestrators,
  openSessionIds,
  sessionListFilter,
  setSessionListFilter,
  sessionListSearchQuery,
  setSessionListSearchQuery,
  sessionFilterCounts,
  hasSessionListSearch,
  sessionListSearchResults,
  filteredSessions,
  controlPanelFilesystemRoot,
  controlPanelGitWorkdir,
  controlPanelGitStatusCount,
  setControlPanelGitStatusCount,
  workspaceFilesChangedEvent,
  projects,
  projectSessionCounts,
  remoteLookup,
  projectScopedSessions,
  isSettingsOpen,
  setIsSettingsOpen,
  isCreateSessionOpen,
  isCreating,
  isCreatingProject,
  controlPanelProjectOptions,
  controlPanelInlineIssueDetail,
  backendConnectionState,
  workspaceViewId,
  deletingWorkspaceIds,
  workspaceSwitcherError,
  isWorkspaceSwitcherLoading,
  isWorkspaceSwitcherOpen,
  workspaceSummaries,
  workspaceSwitcherRef,
  windowId,
  pendingOrchestratorActionById,
  killingSessionIds,
  pendingKillSessionId,
  killRevealSessionId,
  sessionListSearchInputRef,
  setKillRevealSessionId,
  setControlPanelFilesystemRoot,
  setControlPanelGitWorkdir,
  setStandaloneControlSurfaceViewStateByTabId,
  setCollapsedSessionOrchestratorIdsBySurfaceId,
  setSelectedProjectId,
  setWorkspace,
  handleSidebarSessionClick,
  handleKillSession,
  handleSessionRenameRequest,
  handleControlPanelLauncherDragStart,
  handleControlPanelLauncherDragEnd,
  handleOpenFilesystemTab,
  handleOpenGitStatusTab,
  handleOpenGitStatusDiffPreviewTab,
  handleOpenProjectListTab,
  handleOpenOrchestratorListTab,
  handleOpenOrchestratorCanvasTab,
  handleOpenSessionListTab,
  handleOpenCanvasTab,
  openCreateProjectDialog,
  openCreateSessionDialog,
  handleOpenSourceTab,
  handleOrchestratorStateUpdated,
  handleProjectMenuRemoveProject,
  handleProjectMenuStartSession,
  handleOrchestratorRuntimeAction,
  handleDeleteWorkspace,
  handleOpenNewWorkspaceHere,
  handleOpenNewWorkspaceWindow,
  handleOpenWorkspaceHere,
  handleWorkspaceSwitcherToggle,
  handleRetryBackendConnection,
}: AppControlSurfaceProps): JSX.Element {
    const surfaceId = fixedSection ? `${paneId}-${fixedSection}` : paneId;
    const controlPanelProjectFilterId = `control-panel-project-scope-${surfaceId}`;
    const controlSurfaceCollapsedOrchestratorIds =
      collapsedSessionOrchestratorIdsBySurfaceId[surfaceId] ?? [];
    const {
      controlSurfacePane,
      controlSurfaceActiveTab,
      isStandaloneControlSurface,
      standaloneControlSurfaceTabId,
      standaloneControlSurfaceViewState,
      controlSurfaceSelectedProjectId,
      controlSurfaceSelectedProject,
      controlSurfaceSession,
      controlSurfaceWorkspaceRoot,
      controlPanelLauncherOriginProjectId,
      controlPanelLauncherOriginSessionId,
      updateStandaloneControlSurfaceViewState,
      handleControlSurfaceProjectScopeChange,
    } = useControlSurfaceScope({
      paneId,
      fixedSection,
      paneLookup,
      sessionLookup,
      workspace,
      standaloneControlSurfaceViewStateByTabId,
      projectLookup,
      selectedProjectId,
      activeSession,
      sessions,
      setStandaloneControlSurfaceViewStateByTabId,
      setSelectedProjectId,
      setWorkspace,
    });
    const isStandaloneSessionList =
      fixedSection === "sessions" && standaloneControlSurfaceTabId !== null;
    const sessionListVisibleSessionCount =
      filterSessionListVisibleSessions(sessions).length;
    const standaloneSessionListState = isStandaloneSessionList
      ? buildControlSurfaceSessionListState(
          sessions,
          controlSurfaceSelectedProject,
          standaloneControlSurfaceViewState?.sessionListFilter ?? "all",
          standaloneControlSurfaceViewState?.sessionListSearchQuery ?? "",
        )
      : null;
    const controlSurfaceSessionListFilter = standaloneSessionListState
      ? (standaloneControlSurfaceViewState?.sessionListFilter ?? "all")
      : sessionListFilter;
    const controlSurfaceSessionListSearchQuery = standaloneSessionListState
      ? (standaloneControlSurfaceViewState?.sessionListSearchQuery ?? "")
      : sessionListSearchQuery;
    const controlSurfaceSessionFilterCounts = standaloneSessionListState
      ? standaloneSessionListState.sessionFilterCounts
      : sessionFilterCounts;
    const controlSurfaceHasSessionListSearch = standaloneSessionListState
      ? standaloneSessionListState.hasSessionListSearch
      : hasSessionListSearch;
    const controlSurfaceSessionListSearchResults = standaloneSessionListState
      ? standaloneSessionListState.sessionListSearchResults
      : sessionListSearchResults;
    const controlSurfaceFilteredSessions = standaloneSessionListState
      ? standaloneSessionListState.filteredSessions
      : filteredSessions;
    const controlSurfaceSearchDefiniteMatchCount = controlSurfaceHasSessionListSearch
      ? controlSurfaceFilteredSessions.filter(
          (session) =>
            controlSurfaceSessionListSearchResults.get(session.id)?.hasMatch,
        ).length
      : 0;
    const controlSurfaceSearchIncompleteOnlyCount =
      controlSurfaceHasSessionListSearch
        ? controlSurfaceFilteredSessions.filter((session) => {
            const result = controlSurfaceSessionListSearchResults.get(session.id);
            return result?.transcriptIncomplete && !result.hasMatch;
          }).length
        : 0;
    const controlSurfaceSessionListSearchMeta =
      controlSurfaceSearchIncompleteOnlyCount > 0
        ? `${controlSurfaceSearchDefiniteMatchCount} matching ${
            controlSurfaceSearchDefiniteMatchCount === 1 ? "session" : "sessions"
          }, ${controlSurfaceSearchIncompleteOnlyCount} transcript${
            controlSurfaceSearchIncompleteOnlyCount === 1 ? "" : "s"
          } not loaded`
        : controlSurfaceSearchDefiniteMatchCount === 1
          ? "1 matching session"
          : `${controlSurfaceSearchDefiniteMatchCount} matching sessions`;
    const controlSurfaceSessionListEntries =
      buildControlSurfaceSessionListEntries(
        controlSurfaceFilteredSessions,
        orchestrators,
      );
    const controlSurfaceFilesystemRoot =
      controlSurfaceActiveTab?.kind === "filesystem"
        ? controlSurfaceActiveTab.rootPath
        : fixedSection === "files"
          ? controlSurfaceWorkspaceRoot
          : controlPanelFilesystemRoot;
    const controlSurfaceGitWorkdir =
      controlSurfaceActiveTab?.kind === "gitStatus"
        ? controlSurfaceActiveTab.workdir
        : fixedSection === "git"
          ? controlSurfaceWorkspaceRoot
          : controlPanelGitWorkdir;

    function renderControlSurfaceSessionRow(session: Session) {
      const isActive = session.id === activeSession?.id;
      const isOpen = openSessionIds.has(session.id);
      const isKilling = Boolean(killingSessionIds[session.id]);
      const isKillConfirmationOpen = pendingKillSessionId === session.id;
      const isKillVisible =
        isKilling ||
        isKillConfirmationOpen ||
        killRevealSessionId === session.id;
      const searchResult = controlSurfaceSessionListSearchResults.get(
        session.id,
      );

      return (
        <div
          key={`${surfaceId}-${session.id}`}
          className={`session-row-shell ${isActive ? "selected" : ""} ${isOpen ? "open" : ""} ${isKillVisible ? "kill-armed" : ""}`}
          onMouseLeave={() => {
            if (!isKilling && !isKillConfirmationOpen) {
              setKillRevealSessionId((current) =>
                current === session.id ? null : current,
              );
            }
          }}
          onBlur={(event) => {
            const nextTarget = event.relatedTarget;
            if (
              !isKilling &&
              !isKillConfirmationOpen &&
              (!(nextTarget instanceof Node) ||
                !event.currentTarget.contains(nextTarget))
            ) {
              setKillRevealSessionId((current) =>
                current === session.id ? null : current,
              );
            }
          }}
        >
          <button
            className={`session-row ${isActive ? "selected" : ""} ${isOpen ? "open" : ""}`}
            type="button"
            draggable
            onClick={() =>
              handleSidebarSessionClick(session.id, paneId, !fixedSection)
            }
            title={`${session.agent} / ${session.workdir}`}
            onDragStart={(event) => {
              event.dataTransfer.effectAllowed = "copy";
              attachSessionDragData(
                event.dataTransfer,
                session.id,
                session.name,
              );
              const rect = event.currentTarget.getBoundingClientRect();
              event.dataTransfer.setDragImage(
                event.currentTarget,
                Math.max(12, event.clientX - rect.left),
                Math.max(12, event.clientY - rect.top),
              );
            }}
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
                    {searchResult.transcriptIncomplete &&
                    searchResult.matchCount === 0
                      ? "Transcript not loaded"
                      : `${searchResult.matchCount}${
                          searchResult.transcriptIncomplete ? "+" : ""
                        } hit${searchResult.matchCount === 1 ? "" : "s"}`}
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
            <span
              className="status-agent-badge session-row-status-badge"
              data-status={session.status}
            >
              <AgentIcon
                agent={session.agent}
                className="session-row-status-icon"
              />
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
              isKillConfirmationOpen
                ? `kill-session-popover-${session.id}`
                : undefined
            }
            aria-label={`Kill ${session.name}`}
          >
            {isKilling ? "Killing" : "Kill"}
          </button>
        </div>
      );
    }

    function buildControlPanelLauncherTab(sectionId: ControlPanelSectionId) {
      return createControlPanelSectionLauncherTab(sectionId, {
        filesystemRoot: controlSurfaceFilesystemRoot,
        gitWorkdir: controlSurfaceGitWorkdir,
        originProjectId: controlPanelLauncherOriginProjectId,
        originSessionId: controlPanelLauncherOriginSessionId,
      });
    }
    function handleControlPanelSectionTabDragStart(
      event: ReactDragEvent<HTMLButtonElement>,
      sectionId: ControlPanelSectionId,
    ) {
      const tab = buildControlPanelLauncherTab(sectionId);
      if (!tab) {
        return;
      }

      handleControlPanelLauncherDragStart(event, paneId, sectionId, tab);
    }

    function toggleControlSurfaceOrchestratorGroup(orchestratorId: string) {
      setCollapsedSessionOrchestratorIdsBySurfaceId((current) => {
        const previous = current[surfaceId] ?? [];
        const next = previous.includes(orchestratorId)
          ? previous.filter((candidateId) => candidateId !== orchestratorId)
          : [...previous, orchestratorId];

        if (!next.length) {
          if (!(surfaceId in current)) {
            return current;
          }

          const { [surfaceId]: _discard, ...rest } = current;
          return rest;
        }

        return {
          ...current,
          [surfaceId]: next,
        };
      });
    }

    function renderControlPanelProjectScope() {
      return (
        <div className="control-panel-scope-control">
          <label
            className="control-panel-scope-label"
            htmlFor={controlPanelProjectFilterId}
          >
            Project
          </label>
          <ThemedCombobox
            id={controlPanelProjectFilterId}
            className="control-panel-scope-combobox"
            value={controlSurfaceSelectedProjectId}
            options={controlPanelProjectOptions}
            onChange={handleControlSurfaceProjectScopeChange}
            aria-label="Project"
          />
        </div>
      );
    }

    function renderOpenTabAction(
      sectionId: ControlPanelSectionId,
      onClick: () => void,
      disabled: boolean,
      tab: WorkspaceTab | null,
    ): JSX.Element {
      return (
        <button
          className="control-panel-header-action control-panel-header-open-button"
          type="button"
          draggable={!disabled && tab !== null}
          aria-label="Open tab"
          title={
            disabled ? "Open tab" : "Open tab or drag it into the workspace"
          }
          onClick={onClick}
          onDragStart={(event) => {
            if (!tab) {
              event.preventDefault();
              return;
            }

            handleControlPanelLauncherDragStart(event, paneId, sectionId, tab);
          }}
          onDragEnd={handleControlPanelLauncherDragEnd}
          disabled={disabled}
        >
          <span
            className="control-panel-header-action-icon control-panel-header-action-icon-open-tab"
            aria-hidden="true"
          >
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
        </button>
      );
    }

    function renderCanvasTabAction(onClick: () => void): JSX.Element {
      return (
        <button
          className="control-panel-header-action control-panel-header-open-button"
          type="button"
          onClick={onClick}
          aria-label="Canvas"
          title="Canvas"
        >
          <span
            className="control-panel-header-action-icon control-panel-header-action-icon-open-tab"
            aria-hidden="true"
          >
            <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
              <rect
                x="2.5"
                y="4"
                width="11"
                height="7.5"
                rx="0.8"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.35"
              />
              <path
                d="M5.5 11.5L4 14.5M10.5 11.5l1.5 3M8 11.5V14"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.2"
                strokeLinecap="round"
              />
              <path
                d="M7 2v2M9 2v2"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.2"
                strokeLinecap="round"
              />
            </svg>
          </span>
        </button>
      );
    }

    function renderControlPanelHeaderActions(sectionId: ControlPanelSectionId) {
      switch (sectionId) {
        case "files":
          return fixedSection
            ? null
            : renderOpenTabAction(
                "files",
                () =>
                  handleOpenFilesystemTab(
                    paneId,
                    controlSurfaceFilesystemRoot,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                  ),
                !(controlSurfaceFilesystemRoot?.trim() ?? ""),
                buildControlPanelLauncherTab("files"),
              );

        case "git":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "git",
                    () =>
                      handleOpenGitStatusTab(
                        paneId,
                        controlSurfaceGitWorkdir,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    !(controlSurfaceGitWorkdir?.trim() ?? ""),
                    buildControlPanelLauncherTab("git"),
                  )}
            </>
          );

        case "projects":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "projects",
                    () =>
                      handleOpenProjectListTab(
                        paneId,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    false,
                    buildControlPanelLauncherTab("projects"),
                  )}
              <button
                className="control-panel-header-action control-panel-header-new-session-button"
                type="button"
                onClick={() => openCreateProjectDialog()}
                aria-label="Add project"
                title="Add project"
                disabled={isCreatingProject}
              >
                <span
                  className="control-panel-header-action-icon control-panel-header-action-icon-new"
                  aria-hidden="true"
                >
                  <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
                    <circle
                      cx="8"
                      cy="8"
                      r="6.5"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="1.3"
                    />
                    <path
                      d="M8 5v6M5 8h6"
                      stroke="currentColor"
                      strokeWidth="1.3"
                      strokeLinecap="round"
                    />
                  </svg>
                </span>
              </button>
            </>
          );

        case "orchestrators":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "orchestrators",
                    () =>
                      handleOpenOrchestratorListTab(
                        paneId,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    false,
                    buildControlPanelLauncherTab("orchestrators"),
                  )}
              <button
                className="control-panel-header-action control-panel-header-new-session-button"
                type="button"
                onClick={() =>
                  handleOpenOrchestratorCanvasTab(
                    paneId,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                    { startMode: "new" },
                  )
                }
              >
                <span
                  className="control-panel-header-action-icon control-panel-header-action-icon-canvas"
                  aria-hidden="true"
                >
                  <svg
                    viewBox="-9 0 64 64"
                    focusable="false"
                    aria-hidden="true"
                  >
                    <g
                      transform="translate(1,1)"
                      stroke="currentColor"
                      strokeWidth="3.5"
                      fill="none"
                    >
                      <path d="M12.5,45 L7.8,62 L2.9,62 L7.6,45" />
                      <path d="M30.5,45 L35.2,62 L40.1,62 L35.4,45" />
                      <rect x="20" y="45" width="4" height="11" />
                      <rect x="19" y="0" width="4" height="9" />
                      <path d="M42,37 C43.1,37 44,37.9 44,39 L44,43 C44,44.1 43.1,45 42,45 L2,45 C0.9,45 0,44.1 0,43 L0,39 C0,37.9 0.9,37 2,37" />
                      <path d="M40.2,41 L4,41 C2.9,41 2,40.1 2,39 L2,11 C2,9.9 2.9,9 4,9 L40.2,9 C41.3,9 42,9.9 42,11 L42,39 C42,40.1 41.3,41 40.2,41 Z" />
                    </g>
                    <line
                      x1="24"
                      y1="20"
                      x2="34"
                      y2="20"
                      stroke="currentColor"
                      strokeWidth="3"
                      strokeLinecap="round"
                    />
                    <line
                      x1="29"
                      y1="15"
                      x2="29"
                      y2="25"
                      stroke="currentColor"
                      strokeWidth="3"
                      strokeLinecap="round"
                    />
                  </svg>
                </span>
              </button>
            </>
          );

        case "sessions":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "sessions",
                    () =>
                      handleOpenSessionListTab(
                        paneId,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    false,
                    buildControlPanelLauncherTab("sessions"),
                  )}
              {renderCanvasTabAction(() =>
                handleOpenCanvasTab(
                  paneId,
                  controlPanelLauncherOriginSessionId,
                  controlPanelLauncherOriginProjectId,
                ),
              )}
              <button
                className="control-panel-header-action control-panel-header-new-session-button"
                type="button"
                onClick={() =>
                  openCreateSessionDialog(
                    paneId,
                    controlSurfaceSelectedProjectId,
                  )
                }
                aria-label="New"
                title="New session"
                aria-haspopup="dialog"
                aria-expanded={isCreateSessionOpen}
                aria-controls="create-session-dialog"
                disabled={isCreating}
              >
                <span
                  className="control-panel-header-action-icon control-panel-header-action-icon-new"
                  aria-hidden="true"
                >
                  <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
                    <circle
                      cx="8"
                      cy="8"
                      r="6.5"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="1.3"
                    />
                    <path
                      d="M8 5v6M5 8h6"
                      stroke="currentColor"
                      strokeWidth="1.3"
                      strokeLinecap="round"
                    />
                  </svg>
                </span>
              </button>
            </>
          );

        default:
          return null;
      }
    }

    function renderControlPanelSection(sectionId: ControlPanelSectionId) {
      switch (sectionId) {
        case "files":
          return (
            <section
              className="control-panel-section-stack control-panel-section-files"
              aria-label="Files"
            >
              {renderControlPanelProjectScope()}
              <FileSystemPanel
                rootPath={controlSurfaceFilesystemRoot}
                sessionId={controlPanelLauncherOriginSessionId}
                projectId={controlPanelLauncherOriginProjectId}
                workspaceFilesChangedEvent={workspaceFilesChangedEvent}
                showPathControls={false}
                onOpenPath={(path, options) =>
                  handleOpenSourceTab(
                    paneId,
                    path,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                    options,
                  )
                }
                onOpenRootPath={(path) => {
                  if (!fixedSection) {
                    setControlPanelFilesystemRoot(path.trim() || null);
                  }
                }}
              />
            </section>
          );

        case "orchestrators":
          return (
            <OrchestratorTemplateLibraryPanel
              orchestrators={orchestrators}
              onStateUpdated={handleOrchestratorStateUpdated}
              onNewCanvas={() =>
                handleOpenOrchestratorCanvasTab(
                  paneId,
                  controlPanelLauncherOriginSessionId,
                  controlPanelLauncherOriginProjectId,
                  { startMode: "new" },
                )
              }
              onOpenCanvas={(templateId) =>
                handleOpenOrchestratorCanvasTab(
                  paneId,
                  controlPanelLauncherOriginSessionId,
                  controlPanelLauncherOriginProjectId,
                  { templateId },
                )
              }
            />
          );

        case "git":
          return (
            <section
              className="control-panel-section-stack control-panel-section-git"
              aria-label="Git status"
            >
              {renderControlPanelProjectScope()}
              <GitStatusPanel
                projectId={controlPanelLauncherOriginProjectId}
                sessionId={controlPanelLauncherOriginSessionId}
                workdir={controlSurfaceGitWorkdir}
                showPathControls={false}
                onStatusChange={(status) =>
                  setControlPanelGitStatusCount(status?.files.length ?? 0)
                }
                onOpenDiff={(diff, options) =>
                  handleOpenGitStatusDiffPreviewTab(
                    paneId,
                    diff,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                    options,
                  )
                }
                onOpenWorkdir={(path) => {
                  if (!fixedSection) {
                    setControlPanelGitWorkdir(path.trim() || null);
                  }
                }}
              />
            </section>
          );

        case "projects":
          return (
            <ProjectListSection
              paneId={paneId}
              projectSessionCounts={projectSessionCounts}
              projects={projects}
              remoteLookup={remoteLookup}
              selectedProjectId={controlSurfaceSelectedProjectId}
              sessionCount={sessionListVisibleSessionCount}
              onProjectScopeChange={handleControlSurfaceProjectScopeChange}
              onRemoveProject={(project) =>
                void handleProjectMenuRemoveProject(project)
              }
              onStartSession={handleProjectMenuStartSession}
            />
          );

        case "sessions":
        default:
          return (
            <section
              className="control-panel-section-stack control-panel-section-sessions"
              aria-label="Sessions"
            >
              <section className="session-list-shell" aria-label="Sessions">
                <div className="session-list-tools">
                  {renderControlPanelProjectScope()}
                  <input
                    ref={
                      fixedSection
                        ? undefined
                        : (sessionListSearchInputRef as RefObject<HTMLInputElement>)
                    }
                    className="themed-input session-list-search-input"
                    type="search"
                    value={controlSurfaceSessionListSearchQuery}
                    placeholder="Search sessions"
                    spellCheck={false}
                    aria-label="Search sessions"
                    title={`Search across visible sessions (${primaryModifierLabel()}+Shift+F)`}
                    onChange={(event) => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListSearchQuery: event.currentTarget.value,
                        });
                      } else {
                        setSessionListSearchQuery(event.currentTarget.value);
                      }
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.preventDefault();
                        if (controlSurfaceSessionListSearchQuery) {
                          if (standaloneControlSurfaceTabId) {
                            updateStandaloneControlSurfaceViewState({
                              sessionListSearchQuery: "",
                            });
                          } else {
                            setSessionListSearchQuery("");
                          }
                        } else {
                          event.currentTarget.blur();
                        }
                      }
                    }}
                  />
                  {controlSurfaceHasSessionListSearch ? (
                    <div
                      className="session-list-search-meta"
                      aria-live="polite"
                    >
                      {controlSurfaceSessionListSearchMeta}
                    </div>
                  ) : null}
                </div>
                <div className="session-list">
                  {controlSurfaceFilteredSessions.length > 0 ? (
                    controlSurfaceSessionListEntries.map((entry) => {
                      if (entry.kind === "session") {
                        return renderControlSurfaceSessionRow(entry.session);
                      }

                      const groupName = formatSessionOrchestratorGroupName(
                        entry.orchestrator,
                      );

                      const isGroupCollapsed =
                        controlSurfaceCollapsedOrchestratorIds.includes(
                          entry.orchestrator.id,
                        );
                      const groupListId = `${surfaceId}-orchestrator-group-list-${entry.orchestrator.id}`;
                      const pendingOrchestratorAction =
                        pendingOrchestratorActionById[entry.orchestrator.id];
                      const hasPendingOrchestratorAction = Boolean(
                        pendingOrchestratorAction,
                      );

                      return (
                        <section
                          key={`${surfaceId}-orchestrator-group-${entry.orchestrator.id}`}
                          className="session-orchestrator-group"
                          role="group"
                          aria-label={`Orchestration ${groupName}`}
                          data-status={entry.orchestrator.status}
                        >
                          <header className="session-orchestrator-group-header">
                            <button
                              className="session-orchestrator-group-toggle"
                              type="button"
                              onClick={() =>
                                toggleControlSurfaceOrchestratorGroup(
                                  entry.orchestrator.id,
                                )
                              }
                              aria-expanded={!isGroupCollapsed}
                              aria-controls={
                                !isGroupCollapsed ? groupListId : undefined
                              }
                              aria-label={`${isGroupCollapsed ? "Expand" : "Collapse"} ${groupName} sessions`}
                              title={
                                isGroupCollapsed
                                  ? "Expand sessions"
                                  : "Collapse sessions"
                              }
                            >
                              <svg
                                className={`session-orchestrator-group-chevron${!isGroupCollapsed ? " expanded" : ""}`}
                                viewBox="0 0 12 12"
                                focusable="false"
                                aria-hidden="true"
                              >
                                <path
                                  d="M4 2.75 7.75 6 4 9.25"
                                  fill="none"
                                  stroke="currentColor"
                                  strokeLinecap="round"
                                  strokeLinejoin="round"
                                  strokeWidth="1.4"
                                />
                              </svg>
                            </button>
                            <div className="session-orchestrator-group-copy">
                              <span className="session-orchestrator-group-label">
                                Orchestration
                              </span>
                              <div className="session-orchestrator-group-title-row">
                                <strong className="session-orchestrator-group-name">
                                  {groupName}
                                </strong>
                                <span className="session-orchestrator-group-count">
                                  {entry.sessions.length === 1
                                    ? "1 session"
                                    : `${entry.sessions.length} sessions`}
                                </span>
                              </div>
                            </div>
                            <div className="session-orchestrator-group-meta">
                              {entry.orchestrator.status === "running" ? (
                                <div className="session-orchestrator-group-actions">
                                  <OrchestratorRuntimeActionButton
                                    action="pause"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "pause"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "pause",
                                      )
                                    }
                                  />
                                  <OrchestratorRuntimeActionButton
                                    action="stop"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "stop"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "stop",
                                      )
                                    }
                                  />
                                </div>
                              ) : entry.orchestrator.status === "paused" ? (
                                <div className="session-orchestrator-group-actions">
                                  <OrchestratorRuntimeActionButton
                                    action="resume"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "resume"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "resume",
                                      )
                                    }
                                  />
                                  <OrchestratorRuntimeActionButton
                                    action="stop"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "stop"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "stop",
                                      )
                                    }
                                  />
                                </div>
                              ) : null}
                            </div>
                          </header>
                          {!isGroupCollapsed ? (
                            <div
                              id={groupListId}
                              className="session-orchestrator-group-list"
                            >
                              {entry.sessions.map((session) =>
                                renderControlSurfaceSessionRow(session),
                              )}
                            </div>
                          ) : null}
                        </section>
                      );
                    })
                  ) : (
                    <div className="session-filter-empty">
                      {sessions.length === 0
                        ? "No sessions yet."
                        : controlSurfaceHasSessionListSearch
                          ? controlSurfaceSelectedProject
                            ? `No sessions match this search in ${controlSurfaceSelectedProject.name}.`
                            : "No sessions match this search."
                          : controlSurfaceSelectedProject
                            ? `No ${controlSurfaceSessionListFilter === "all" ? "" : `${controlSurfaceSessionListFilter} `}sessions in ${controlSurfaceSelectedProject.name}.`
                            : "No sessions match this filter."}
                    </div>
                  )}
                </div>
              </section>

              <section className="sidebar-status" aria-label="Session filters">
                <div className="session-control-label">Status</div>
                <div className="sidebar-status-chips">
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "all" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "all",
                        });
                      } else {
                        setSessionListFilter("all");
                      }
                    }}
                    aria-pressed={controlSurfaceSessionListFilter === "all"}
                  >
                    No filter ({controlSurfaceSessionFilterCounts.all})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "working" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "working",
                        });
                      } else {
                        setSessionListFilter("working");
                      }
                    }}
                    aria-pressed={controlSurfaceSessionListFilter === "working"}
                  >
                    Working ({controlSurfaceSessionFilterCounts.working})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "asking" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "asking",
                        });
                      } else {
                        setSessionListFilter("asking");
                      }
                    }}
                    aria-pressed={controlSurfaceSessionListFilter === "asking"}
                  >
                    Asking ({controlSurfaceSessionFilterCounts.asking})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "completed" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "completed",
                        });
                      } else {
                        setSessionListFilter("completed");
                      }
                    }}
                    aria-pressed={
                      controlSurfaceSessionListFilter === "completed"
                    }
                  >
                    Completed ({controlSurfaceSessionFilterCounts.completed})
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
          ref={
            fixedSection
              ? undefined
              : (controlPanelSurfaceRef as RefObject<ControlPanelSurfaceHandle>)
          }
          fixedSection={fixedSection}
          gitStatusCount={controlPanelGitStatusCount}
          isPreferencesOpen={isSettingsOpen}
          onOpenPreferences={() => setIsSettingsOpen(true)}
          onSectionTabDragEnd={handleControlPanelLauncherDragEnd}
          onSectionTabDragStart={handleControlPanelSectionTabDragStart}
          projectCount={projects.length}
          sessionCount={projectScopedSessions.length}
          renderHeaderActions={renderControlPanelHeaderActions}
          renderSection={renderControlPanelSection}
          sectionLauncherTabs={{
            files: buildControlPanelLauncherTab("files"),
            git: buildControlPanelLauncherTab("git"),
            projects: buildControlPanelLauncherTab("projects"),
            sessions: buildControlPanelLauncherTab("sessions"),
            orchestrators: buildControlPanelLauncherTab("orchestrators"),
          }}
          windowId={windowId}
          launcherPaneId={paneId}
        />
      </div>
    );
}
