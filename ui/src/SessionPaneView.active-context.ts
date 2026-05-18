import { useMemo } from "react";
import {
  resolveWorkspaceScopedProjectId,
  resolveWorkspaceScopedSessionId,
} from "./control-surface-state";
import { isLocalSessionRemote } from "./remotes";
import {
  resolveControlPanelWorkspaceRoot,
  type ComboboxOption,
} from "./session-model-utils";
import { resolveSessionPaneActiveTab } from "./SessionPaneView.active-tab";
import type { Project, Session } from "./types";
import type { WorkspacePane } from "./workspace";

type SessionPaneActiveContextOptions = {
  pane: WorkspacePane;
  projectLookup: ReadonlyMap<string, Project>;
  sessionLookup: ReadonlyMap<string, Session>;
};

export function useSessionPaneActiveContext({
  pane,
  projectLookup,
  sessionLookup,
}: SessionPaneActiveContextOptions) {
  const activeTab = resolveSessionPaneActiveTab(pane);
  const activeControlPanelTab =
    activeTab?.kind === "controlPanel" ? activeTab : null;
  const activeOrchestratorListTab =
    activeTab?.kind === "orchestratorList" ? activeTab : null;
  const activeSessionListTab =
    activeTab?.kind === "sessionList" ? activeTab : null;
  const activeProjectListTab =
    activeTab?.kind === "projectList" ? activeTab : null;
  const activeCanvasTab = activeTab?.kind === "canvas" ? activeTab : null;
  const activeOrchestratorCanvasTab =
    activeTab?.kind === "orchestratorCanvas" ? activeTab : null;
  const activeControlSurfaceTab =
    activeControlPanelTab ??
    activeOrchestratorListTab ??
    activeSessionListTab ??
    activeProjectListTab;
  const activeSourceTab = activeTab?.kind === "source" ? activeTab : null;
  const activeFilesystemTab =
    activeTab?.kind === "filesystem" ? activeTab : null;
  const activeGitStatusTab = activeTab?.kind === "gitStatus" ? activeTab : null;
  const activeTerminalTab = activeTab?.kind === "terminal" ? activeTab : null;
  const activeInstructionDebuggerTab =
    activeTab?.kind === "instructionDebugger" ? activeTab : null;
  const activeDiffPreviewTab =
    activeTab?.kind === "diffPreview" ? activeTab : null;
  const activeSourceOriginSessionId = activeSourceTab?.originSessionId ?? null;
  const activeSourceOriginProjectId = activeSourceTab?.originProjectId ?? null;
  const activeFilesystemOriginSessionId =
    activeFilesystemTab?.originSessionId ?? null;
  const activeFilesystemOriginProjectId =
    activeFilesystemTab?.originProjectId ?? null;
  const activeGitStatusOriginSessionId =
    activeGitStatusTab?.originSessionId ?? null;
  const activeGitStatusOriginProjectId =
    activeGitStatusTab?.originProjectId ?? null;
  const activeTerminalOriginSessionId =
    activeTerminalTab?.originSessionId ?? null;
  const activeTerminalOriginProjectId =
    activeTerminalTab?.originProjectId ?? null;
  const activeInstructionDebuggerOriginSessionId =
    activeInstructionDebuggerTab?.originSessionId ?? null;
  const activeInstructionDebuggerOriginProjectId =
    activeInstructionDebuggerTab?.originProjectId ?? null;
  const activeInstructionDebuggerSession =
    activeInstructionDebuggerOriginSessionId
      ? (sessionLookup.get(activeInstructionDebuggerOriginSessionId) ?? null)
      : null;
  const activeDiffOriginSessionId =
    activeDiffPreviewTab?.originSessionId ?? null;
  const activeDiffOriginProjectId =
    activeDiffPreviewTab?.originProjectId ?? null;
  const activeDiffWorkspaceRoot =
    (activeDiffOriginSessionId
      ? (sessionLookup.get(activeDiffOriginSessionId)?.workdir ?? null)
      : null) ??
    (activeDiffOriginProjectId
      ? (projectLookup.get(activeDiffOriginProjectId)?.rootPath ?? null)
      : null);
  const activeSourceWorkspaceRoot =
    (activeSourceOriginSessionId
      ? (sessionLookup.get(activeSourceOriginSessionId)?.workdir ?? null)
      : null) ??
    (activeSourceOriginProjectId
      ? (projectLookup.get(activeSourceOriginProjectId)?.rootPath ?? null)
      : null);
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
  const activeSessionProject =
    activeSession?.projectId != null
      ? (projectLookup.get(activeSession.projectId) ?? null)
      : null;
  const enableLocalDelegationActions = isLocalSessionRemote(
    activeSession,
    activeSessionProject,
  );
  const allKnownSessions = useMemo(
    () => Array.from(sessionLookup.values()),
    [sessionLookup],
  );
  const workspaceProjectOptions = useMemo<readonly ComboboxOption[]>(
    () =>
      Array.from(projectLookup.values()).map((project) => ({
        label: project.name,
        value: project.id,
        description: project.rootPath,
      })),
    [projectLookup],
  );
  const sessions = useMemo(
    () => sessionTabs.map(({ session }) => session),
    [sessionTabs],
  );
  const activeFilesystemScopeProjectId = activeFilesystemTab
    ? resolveWorkspaceScopedProjectId(
        activeFilesystemOriginProjectId,
        activeFilesystemOriginSessionId,
        sessionLookup,
        projectLookup,
      )
    : null;
  const activeGitScopeProjectId = activeGitStatusTab
    ? resolveWorkspaceScopedProjectId(
        activeGitStatusOriginProjectId,
        activeGitStatusOriginSessionId,
        sessionLookup,
        projectLookup,
      )
    : null;
  const activeTerminalScopeProjectId = activeTerminalTab
    ? resolveWorkspaceScopedProjectId(
        activeTerminalOriginProjectId,
        activeTerminalOriginSessionId,
        sessionLookup,
        projectLookup,
      )
    : null;
  const activeFilesystemScopedSessionId = activeFilesystemScopeProjectId
    ? resolveWorkspaceScopedSessionId(
        activeFilesystemScopeProjectId,
        activeFilesystemOriginSessionId,
        activeSession,
        allKnownSessions,
        sessionLookup,
      )
    : activeFilesystemOriginSessionId;
  const activeGitScopedSessionId = activeGitScopeProjectId
    ? resolveWorkspaceScopedSessionId(
        activeGitScopeProjectId,
        activeGitStatusOriginSessionId,
        activeSession,
        allKnownSessions,
        sessionLookup,
      )
    : activeGitStatusOriginSessionId;
  const activeTerminalScopedSessionId = activeTerminalScopeProjectId
    ? resolveWorkspaceScopedSessionId(
        activeTerminalScopeProjectId,
        activeTerminalOriginSessionId,
        activeSession,
        allKnownSessions,
        sessionLookup,
      )
    : activeTerminalOriginSessionId;
  const activeFilesystemScopedRootPath =
    activeFilesystemTab?.rootPath ??
    (activeFilesystemScopeProjectId
      ? resolveControlPanelWorkspaceRoot(
          projectLookup.get(activeFilesystemScopeProjectId) ?? null,
          null,
        )
      : null);
  const activeGitScopedWorkdir =
    activeGitStatusTab?.workdir ??
    (activeGitScopeProjectId
      ? resolveControlPanelWorkspaceRoot(
          projectLookup.get(activeGitScopeProjectId) ?? null,
          null,
        )
      : null);
  const activeTerminalScopedWorkdir =
    activeTerminalTab?.workdir ??
    (activeTerminalScopeProjectId
      ? resolveControlPanelWorkspaceRoot(
          projectLookup.get(activeTerminalScopeProjectId) ?? null,
          null,
        )
      : null);
  const shouldRenderFilesystemProjectScope =
    !!activeFilesystemScopeProjectId && workspaceProjectOptions.length > 0;
  const shouldRenderGitProjectScope =
    !!activeGitScopeProjectId && workspaceProjectOptions.length > 0;
  const shouldRenderTerminalProjectScope =
    !!activeTerminalScopeProjectId && workspaceProjectOptions.length > 0;

  return {
    activeTab,
    activeControlPanelTab,
    activeOrchestratorListTab,
    activeSessionListTab,
    activeProjectListTab,
    activeCanvasTab,
    activeOrchestratorCanvasTab,
    activeControlSurfaceTab,
    activeSourceTab,
    activeFilesystemTab,
    activeGitStatusTab,
    activeTerminalTab,
    activeInstructionDebuggerTab,
    activeDiffPreviewTab,
    activeSourceOriginSessionId,
    activeSourceOriginProjectId,
    activeFilesystemOriginSessionId,
    activeFilesystemOriginProjectId,
    activeGitStatusOriginSessionId,
    activeGitStatusOriginProjectId,
    activeTerminalOriginSessionId,
    activeTerminalOriginProjectId,
    activeInstructionDebuggerOriginSessionId,
    activeInstructionDebuggerOriginProjectId,
    activeInstructionDebuggerSession,
    activeDiffOriginSessionId,
    activeDiffOriginProjectId,
    activeDiffWorkspaceRoot,
    activeSourceWorkspaceRoot,
    isSessionTabActive,
    sessionTabs,
    activeSession,
    enableLocalDelegationActions,
    allKnownSessions,
    workspaceProjectOptions,
    sessions,
    activeFilesystemScopeProjectId,
    activeGitScopeProjectId,
    activeTerminalScopeProjectId,
    activeFilesystemScopedSessionId,
    activeGitScopedSessionId,
    activeTerminalScopedSessionId,
    activeFilesystemScopedRootPath,
    activeGitScopedWorkdir,
    activeTerminalScopedWorkdir,
    shouldRenderFilesystemProjectScope,
    shouldRenderGitProjectScope,
    shouldRenderTerminalProjectScope,
  };
}
