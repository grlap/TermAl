import {
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { fetchGitStatus } from "./api";
import type { StandaloneControlSurfaceViewState } from "./app-shell-internals";
import {
  countSessionsByFilter,
  filterSessionListVisibleSessions,
  filterSessionsByListFilter,
  type SessionListFilter,
} from "./session-list-filter";
import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  type SessionListSearchResult,
} from "./session-find";
import {
  describeProjectScope,
  resolveControlPanelWorkspaceRoot,
  resolveRemoteConfig,
  remoteBadgeLabel,
  usesSessionModelPicker,
  NEW_SESSION_MODEL_OPTIONS,
  type ComboboxOption,
} from "./session-model-utils";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import { CREATE_SESSION_WORKSPACE_ID } from "./app-shell-internals";
import {
  LOCAL_REMOTE_ID,
  createBuiltinLocalRemote,
  isLocalRemoteId,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "./remotes";
import {
  findNearestSessionPaneId,
  rescopeControlSurfacePane,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";
import {
  getActiveWorkspacePaneTab,
  resolveWorkspaceTabProjectId,
} from "./workspace-queries";
import type { AgentReadiness, AgentType, Project, RemoteConfig, Session } from "./types";

type ControlSurfaceSectionId =
  | "files"
  | "sessions"
  | "projects"
  | "orchestrators"
  | "git";

type UseAppControlPanelStateParams = {
  remoteConfigs: RemoteConfig[];
  activeSession: Session | null;
  selectedProject: Project | null;
  selectedProjectId: string;
  projects: Project[];
  sessions: Session[];
  projectLookup: Map<string, Project>;
  paneLookup: Map<string, WorkspacePane>;
  sessionLookup: Map<string, Session>;
  workspace: WorkspaceState;
  newProjectRemoteId: string;
  setNewProjectRemoteId: Dispatch<SetStateAction<string>>;
  createSessionProjectId: string;
  setCreateSessionProjectId: Dispatch<SetStateAction<string>>;
  newSessionAgent: AgentType;
  agentReadinessByAgent: Map<AgentType, AgentReadiness>;
  sessionListFilter: SessionListFilter;
  sessionListSearchQuery: string;
  controlPanelFilesystemRoot: string | null;
  setControlPanelFilesystemRoot: Dispatch<SetStateAction<string | null>>;
  controlPanelGitWorkdir: string | null;
  setControlPanelGitWorkdir: Dispatch<SetStateAction<string | null>>;
  setControlPanelGitStatusCount: Dispatch<SetStateAction<number>>;
  lastDerivedControlPanelFilesystemRootRef: MutableRefObject<string | null>;
  lastDerivedControlPanelGitWorkdirRef: MutableRefObject<string | null>;
};

type UseAppControlPanelStateReturn = {
  remoteLookup: Map<string, RemoteConfig>;
  localRemoteConfig: RemoteConfig;
  enabledProjectRemotes: RemoteConfig[];
  newProjectSelectedRemote: RemoteConfig;
  newProjectUsesLocalRemote: boolean;
  createProjectRemoteOptions: readonly ComboboxOption[];
  newSessionModelOptions: readonly ComboboxOption[];
  createSessionSelectedProject: Project | null;
  createSessionWorkspaceProject: Project | null;
  createSessionEffectiveProject: Project | null;
  createSessionSelectedRemote: RemoteConfig;
  createSessionProjectOptions: readonly ComboboxOption[];
  controlPanelProjectOptions: readonly ComboboxOption[];
  createSessionProjectHint: string;
  createSessionUsesRemoteProject: boolean;
  createSessionProjectSelectionError: string | null;
  createSessionUsesSessionModelPicker: boolean;
  createSessionAgentReadiness: AgentReadiness | null;
  createSessionBlocked: boolean;
  projectScopedSessions: Session[];
  controlPanelContextSession: Session | null;
  controlPanelSessionId: string | null;
  sessionFilterCounts: Record<SessionListFilter, number>;
  hasSessionListSearch: boolean;
  sessionListSearchResults: Map<string, SessionListSearchResult>;
  filteredSessions: Session[];
  projectSessionCounts: Map<string, number>;
};

type UseControlSurfaceScopeParams = {
  paneId: string;
  fixedSection?: ControlSurfaceSectionId | null;
  paneLookup: Map<string, WorkspacePane>;
  sessionLookup: Map<string, Session>;
  workspace: WorkspaceState;
  standaloneControlSurfaceViewStateByTabId: Record<
    string,
    StandaloneControlSurfaceViewState
  >;
  projectLookup: Map<string, Project>;
  selectedProjectId: string;
  activeSession: Session | null;
  sessions: Session[];
  setStandaloneControlSurfaceViewStateByTabId: Dispatch<
    SetStateAction<Record<string, StandaloneControlSurfaceViewState>>
  >;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
};

type UseControlSurfaceScopeReturn = {
  controlSurfacePane: WorkspacePane | null;
  controlSurfaceActiveTab: WorkspaceTab | null;
  isStandaloneControlSurface: boolean;
  standaloneControlSurfaceTabId: string | null;
  standaloneControlSurfaceViewState: StandaloneControlSurfaceViewState | null;
  controlSurfaceSelectedProjectId: string;
  controlSurfaceSelectedProject: Project | null;
  controlSurfaceSession: Session | null;
  controlSurfaceWorkspaceRoot: string | null;
  controlPanelLauncherOriginProjectId: string | null;
  controlPanelLauncherOriginSessionId: string | null;
  updateStandaloneControlSurfaceViewState: (
    updates: Partial<StandaloneControlSurfaceViewState>,
  ) => void;
  handleControlSurfaceProjectScopeChange: (nextProjectId: string) => void;
};

function resolveProjectSelection(
  projectId: string,
  projectLookup: Map<string, Project>,
): Project | null {
  if (projectId === ALL_PROJECTS_FILTER_ID) {
    return null;
  }

  return projectLookup.get(projectId) ?? null;
}

function resolveScopedControlSurfaceSession(
  selectedProject: Project | null,
  sessionCandidates: readonly Session[],
  sessions: readonly Session[],
): Session | null {
  if (!selectedProject) {
    return sessionCandidates[0] ?? sessions[0] ?? null;
  }

  return (
    sessionCandidates.find(
      (session) => session.projectId === selectedProject.id,
    ) ??
    sessions.find((session) => session.projectId === selectedProject.id) ??
    null
  );
}

export function useAppControlPanelState({
  remoteConfigs,
  activeSession,
  selectedProject,
  selectedProjectId,
  projects,
  sessions,
  projectLookup,
  paneLookup,
  sessionLookup,
  workspace,
  newProjectRemoteId,
  setNewProjectRemoteId,
  createSessionProjectId,
  setCreateSessionProjectId,
  newSessionAgent,
  agentReadinessByAgent,
  sessionListFilter,
  sessionListSearchQuery,
  controlPanelFilesystemRoot,
  setControlPanelFilesystemRoot,
  controlPanelGitWorkdir,
  setControlPanelGitWorkdir,
  setControlPanelGitStatusCount,
  lastDerivedControlPanelFilesystemRootRef,
  lastDerivedControlPanelGitWorkdirRef,
}: UseAppControlPanelStateParams): UseAppControlPanelStateReturn {
  const remoteLookup = useMemo(
    () => new Map(remoteConfigs.map((remote) => [remote.id, remote])),
    [remoteConfigs],
  );
  const localRemoteConfig =
    remoteLookup.get(LOCAL_REMOTE_ID) ?? createBuiltinLocalRemote();
  const enabledProjectRemotes = useMemo(
    () =>
      remoteConfigs.filter(
        (remote) => remote.enabled || isLocalRemoteId(remote.id),
      ),
    [remoteConfigs],
  );
  const newProjectSelectedRemote = resolveRemoteConfig(
    remoteLookup,
    newProjectRemoteId,
  );
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
    ? resolveRemoteConfig(
        remoteLookup,
        resolveProjectRemoteId(createSessionEffectiveProject),
      )
    : localRemoteConfig;
  const createSessionProjectOptions = useMemo<readonly ComboboxOption[]>(() => {
    const workspaceLabel = activeSession?.workdir
      ? "Current workspace"
      : "Default workspace";

    return [
      { label: workspaceLabel, value: CREATE_SESSION_WORKSPACE_ID },
      ...projects.map((project) => {
        const remote = resolveRemoteConfig(
          remoteLookup,
          resolveProjectRemoteId(project),
        );
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
        const remote = resolveRemoteConfig(
          remoteLookup,
          resolveProjectRemoteId(project),
        );
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
    createSessionProjectId === "__workspace__" &&
    !!activeSession?.projectId &&
    !projectLookup.has(activeSession.projectId)
      ? "The current workspace is tied to a project that is no longer available. Choose a project before creating a session."
      : null;
  const createSessionUsesSessionModelPicker =
    usesSessionModelPicker(newSessionAgent);
  const createSessionAgentReadiness = createSessionUsesRemoteProject
    ? null
    : (agentReadinessByAgent.get(newSessionAgent) ?? null);
  const createSessionBlocked = createSessionAgentReadiness?.blocking ?? false;

  const sessionListVisibleSessions = useMemo(
    () => filterSessionListVisibleSessions(sessions),
    [sessions],
  );

  const projectScopedSessions = useMemo(() => {
    if (!selectedProject) {
      return sessionListVisibleSessions;
    }

    return sessionListVisibleSessions.filter(
      (session) => session.projectId === selectedProject.id,
    );
  }, [selectedProject, sessionListVisibleSessions]);
  const dockedControlPanelPane =
    workspace.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "controlPanel"),
    ) ?? null;
  const dockedControlPanelActiveTab = dockedControlPanelPane
    ? (dockedControlPanelPane.tabs.find(
        (tab) => tab.id === dockedControlPanelPane.activeTabId,
      ) ??
        dockedControlPanelPane.tabs[0] ??
        null)
    : null;
  const dockedControlPanelOriginSession =
    dockedControlPanelActiveTab &&
    "originSessionId" in dockedControlPanelActiveTab &&
    dockedControlPanelActiveTab.originSessionId
      ? (sessionLookup.get(dockedControlPanelActiveTab.originSessionId) ?? null)
      : null;
  const dockedControlPanelPaneSession = dockedControlPanelPane?.activeSessionId
    ? (sessionLookup.get(dockedControlPanelPane.activeSessionId) ?? null)
    : null;
  const dockedControlPanelNearestSessionPaneId = dockedControlPanelPane
    ? findNearestSessionPaneId(workspace, dockedControlPanelPane.id)
    : null;
  const dockedControlPanelNearestSessionPane =
    dockedControlPanelNearestSessionPaneId
      ? (paneLookup.get(dockedControlPanelNearestSessionPaneId) ?? null)
      : null;
  const dockedControlPanelNearestSessionTab =
    dockedControlPanelNearestSessionPane
      ? (dockedControlPanelNearestSessionPane.tabs.find(
          (tab) => tab.id === dockedControlPanelNearestSessionPane.activeTabId,
        ) ??
          dockedControlPanelNearestSessionPane.tabs[0] ??
          null)
      : null;
  const dockedControlPanelNearestSession =
    dockedControlPanelNearestSessionTab?.kind === "session"
      ? (sessionLookup.get(dockedControlPanelNearestSessionTab.sessionId) ??
        null)
      : null;
  const dockedControlPanelSessionCandidates = [
    dockedControlPanelOriginSession,
    dockedControlPanelPaneSession,
    dockedControlPanelNearestSession,
    activeSession,
  ].filter((session): session is Session => Boolean(session));
  const controlPanelContextSession = selectedProject
    ? (dockedControlPanelSessionCandidates.find(
        (session) => session.projectId === selectedProject.id,
      ) ??
        projectScopedSessions[0] ??
        null)
    : (dockedControlPanelSessionCandidates[0] ??
      sessionListVisibleSessions[0] ??
      null);
  const derivedControlPanelWorkspaceRoot = resolveControlPanelWorkspaceRoot(
    selectedProject,
    controlPanelContextSession?.workdir ?? null,
  );
  const derivedControlPanelFilesystemRoot = derivedControlPanelWorkspaceRoot;
  const derivedControlPanelGitWorkdir = derivedControlPanelWorkspaceRoot;
  const controlPanelSessionId = controlPanelContextSession?.id ?? null;
  const sessionFilterCounts = useMemo(
    () => countSessionsByFilter(projectScopedSessions),
    [projectScopedSessions],
  );
  const statusFilteredSessions = useMemo(() => {
    return filterSessionsByListFilter(projectScopedSessions, sessionListFilter);
  }, [projectScopedSessions, sessionListFilter]);
  const trimmedSessionListSearchQuery = sessionListSearchQuery.trim();
  const deferredSessionListSearchQuery = useDeferredValue(
    trimmedSessionListSearchQuery,
  );
  const effectiveSessionListSearchQuery =
    trimmedSessionListSearchQuery.length === 0
      ? ""
      : deferredSessionListSearchQuery;
  const hasSessionListSearch = effectiveSessionListSearchQuery.length > 0;
  const sessionListSearchIndex = useMemo(() => {
    if (!hasSessionListSearch) {
      return null;
    }

    return new Map(
      statusFilteredSessions.map(
        (session) => [session.id, buildSessionSearchIndex(session)] as const,
      ),
    );
  }, [hasSessionListSearch, statusFilteredSessions]);

  useEffect(() => {
    if (
      createSessionProjectId !== CREATE_SESSION_WORKSPACE_ID &&
      !projectLookup.has(createSessionProjectId)
    ) {
      setCreateSessionProjectId(CREATE_SESSION_WORKSPACE_ID);
    }
  }, [createSessionProjectId, projectLookup, setCreateSessionProjectId]);

  useEffect(() => {
    if (
      !enabledProjectRemotes.some((remote) => remote.id === newProjectRemoteId)
    ) {
      setNewProjectRemoteId(enabledProjectRemotes[0]?.id ?? LOCAL_REMOTE_ID);
    }
  }, [enabledProjectRemotes, newProjectRemoteId, setNewProjectRemoteId]);

  useEffect(() => {
    const previousDerived =
      lastDerivedControlPanelFilesystemRootRef.current?.trim() ?? "";
    lastDerivedControlPanelFilesystemRootRef.current =
      derivedControlPanelFilesystemRoot;

    setControlPanelFilesystemRoot((current) => {
      const trimmedCurrent = current?.trim() ?? "";
      if (!trimmedCurrent || trimmedCurrent === previousDerived) {
        return derivedControlPanelFilesystemRoot;
      }
      return current;
    });
  }, [
    derivedControlPanelFilesystemRoot,
    lastDerivedControlPanelFilesystemRootRef,
    setControlPanelFilesystemRoot,
  ]);

  useEffect(() => {
    const previousDerived =
      lastDerivedControlPanelGitWorkdirRef.current?.trim() ?? "";
    lastDerivedControlPanelGitWorkdirRef.current = derivedControlPanelGitWorkdir;

    setControlPanelGitWorkdir((current) => {
      const trimmedCurrent = current?.trim() ?? "";
      if (!trimmedCurrent || trimmedCurrent === previousDerived) {
        return derivedControlPanelGitWorkdir;
      }
      return current;
    });
  }, [
    derivedControlPanelGitWorkdir,
    lastDerivedControlPanelGitWorkdirRef,
    setControlPanelGitWorkdir,
  ]);

  useEffect(() => {
    const normalizedGitWorkdir = controlPanelGitWorkdir?.trim() ?? "";
    let cancelled = false;

    if (!normalizedGitWorkdir) {
      setControlPanelGitStatusCount(0);
      return;
    }

    void fetchGitStatus(normalizedGitWorkdir, controlPanelSessionId, {
      projectId: selectedProject?.id ?? null,
    })
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
  }, [
    controlPanelGitWorkdir,
    controlPanelSessionId,
    selectedProject?.id,
    setControlPanelGitStatusCount,
  ]);

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

    return statusFilteredSessions.filter((session) =>
      sessionListSearchResults.has(session.id),
    );
  }, [hasSessionListSearch, sessionListSearchResults, statusFilteredSessions]);
  const projectSessionCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const session of sessionListVisibleSessions) {
      if (!session.projectId) {
        continue;
      }
      counts.set(session.projectId, (counts.get(session.projectId) ?? 0) + 1);
    }
    return counts;
  }, [sessionListVisibleSessions]);

  return {
    remoteLookup,
    localRemoteConfig,
    enabledProjectRemotes,
    newProjectSelectedRemote,
    newProjectUsesLocalRemote,
    createProjectRemoteOptions,
    newSessionModelOptions,
    createSessionSelectedProject,
    createSessionWorkspaceProject,
    createSessionEffectiveProject,
    createSessionSelectedRemote,
    createSessionProjectOptions,
    controlPanelProjectOptions,
    createSessionProjectHint,
    createSessionUsesRemoteProject,
    createSessionProjectSelectionError,
    createSessionUsesSessionModelPicker,
    createSessionAgentReadiness,
    createSessionBlocked,
    projectScopedSessions,
    controlPanelContextSession,
    controlPanelSessionId,
    sessionFilterCounts,
    hasSessionListSearch,
    sessionListSearchResults,
    filteredSessions,
    projectSessionCounts,
  };
}

export function useControlSurfaceScope({
  paneId,
  fixedSection = null,
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
}: UseControlSurfaceScopeParams): UseControlSurfaceScopeReturn {
  const controlSurfacePane = paneLookup.get(paneId) ?? null;
  const controlSurfaceActiveTab = controlSurfacePane
    ? getActiveWorkspacePaneTab(controlSurfacePane)
    : null;
  const controlSurfaceOriginSession =
    controlSurfaceActiveTab &&
    "originSessionId" in controlSurfaceActiveTab &&
    controlSurfaceActiveTab.originSessionId
      ? (sessionLookup.get(controlSurfaceActiveTab.originSessionId) ?? null)
      : null;
  const controlSurfacePaneSession = controlSurfacePane?.activeSessionId
    ? (sessionLookup.get(controlSurfacePane.activeSessionId) ?? null)
    : null;
  const nearestSessionPaneId = findNearestSessionPaneId(workspace, paneId);
  const nearestSessionPane = nearestSessionPaneId
    ? (paneLookup.get(nearestSessionPaneId) ?? null)
    : null;
  const nearestSessionTab = nearestSessionPane
    ? getActiveWorkspacePaneTab(nearestSessionPane)
    : null;
  const nearestSession =
    nearestSessionTab?.kind === "session"
      ? (sessionLookup.get(nearestSessionTab.sessionId) ?? null)
      : null;
  const isStandaloneControlSurface = fixedSection !== null;
  const standaloneControlSurfaceTabId =
    isStandaloneControlSurface && controlSurfaceActiveTab
      ? controlSurfaceActiveTab.id
      : null;
  const standaloneControlSurfaceViewState = standaloneControlSurfaceTabId
    ? (standaloneControlSurfaceViewStateByTabId[
        standaloneControlSurfaceTabId
      ] ?? null)
    : null;
  const controlSurfaceTabProjectId = resolveWorkspaceTabProjectId(
    controlSurfaceActiveTab ?? undefined,
    sessionLookup,
  );
  const controlSurfaceSelectedProjectId = isStandaloneControlSurface
    ? (standaloneControlSurfaceViewState?.projectId ??
      (controlSurfaceTabProjectId &&
      projectLookup.has(controlSurfaceTabProjectId)
        ? controlSurfaceTabProjectId
        : ALL_PROJECTS_FILTER_ID))
    : selectedProjectId;
  const controlSurfaceSelectedProject = resolveProjectSelection(
    controlSurfaceSelectedProjectId,
    projectLookup,
  );
  const controlSurfaceSessionCandidates = (
    isStandaloneControlSurface
      ? [controlSurfaceOriginSession, controlSurfacePaneSession]
      : [
          controlSurfaceOriginSession,
          controlSurfacePaneSession,
          nearestSession,
          activeSession,
        ]
  ).filter((session): session is Session => Boolean(session));
  const controlSurfaceSession = resolveScopedControlSurfaceSession(
    controlSurfaceSelectedProject,
    controlSurfaceSessionCandidates,
    sessions,
  );
  const controlPanelLauncherOriginProjectId =
    controlSurfaceSelectedProject?.id ??
    controlSurfaceSession?.projectId ??
    controlSurfaceTabProjectId ??
    null;
  const controlPanelLauncherOriginSessionId = controlSurfaceSession?.id ?? null;
  const controlSurfaceWorkspaceRoot = resolveControlPanelWorkspaceRoot(
    controlSurfaceSelectedProject,
    controlSurfaceSession?.workdir ?? null,
  );

  const updateStandaloneControlSurfaceViewState = useCallback(
    (updates: Partial<StandaloneControlSurfaceViewState>) => {
      if (!standaloneControlSurfaceTabId) {
        return;
      }

      setStandaloneControlSurfaceViewStateByTabId((current) => {
        const previous = current[standaloneControlSurfaceTabId] ?? {};
        const next = { ...previous, ...updates };
        if (
          previous.projectId === next.projectId &&
          previous.sessionListFilter === next.sessionListFilter &&
          previous.sessionListSearchQuery === next.sessionListSearchQuery
        ) {
          return current;
        }

        return {
          ...current,
          [standaloneControlSurfaceTabId]: next,
        };
      });
    },
    [setStandaloneControlSurfaceViewStateByTabId, standaloneControlSurfaceTabId],
  );

  const handleControlSurfaceProjectScopeChange = useCallback(
    (nextProjectId: string) => {
      if (!isStandaloneControlSurface || !controlSurfaceActiveTab) {
        setSelectedProjectId(nextProjectId);
        return;
      }

      updateStandaloneControlSurfaceViewState({ projectId: nextProjectId });
      const nextSelectedProject = resolveProjectSelection(
        nextProjectId,
        projectLookup,
      );
      const preferredStandaloneSession =
        controlSurfaceOriginSession ?? controlSurfacePaneSession ?? null;
      const nextScopedSession = resolveScopedControlSurfaceSession(
        nextSelectedProject,
        preferredStandaloneSession ? [preferredStandaloneSession] : [],
        sessions,
      );
      const nextWorkspaceRoot = resolveControlPanelWorkspaceRoot(
        nextSelectedProject,
        nextScopedSession?.workdir ?? null,
      );

      setWorkspace((current) =>
        rescopeControlSurfacePane(
          current,
          paneId,
          nextScopedSession?.id ?? null,
          nextSelectedProject?.id ?? null,
          nextWorkspaceRoot,
        ),
      );
    },
    [
      controlSurfaceActiveTab,
      controlSurfaceOriginSession,
      controlSurfacePaneSession,
      isStandaloneControlSurface,
      paneId,
      projectLookup,
      sessions,
      setSelectedProjectId,
      setWorkspace,
      updateStandaloneControlSurfaceViewState,
    ],
  );

  return {
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
  };
}
