// Pure state/data helpers for the control panel's "control surface"
// views (filesystem, git status, projects, sessions, orchestrators).
//
// What this file owns:
//   - `createControlPanelSectionLauncherTab` — given a section id and
//     the originating context (filesystem root, git workdir, origin
//     project / session), returns a `WorkspaceTab` for that section
//     or `null` when the inputs don't satisfy the section (e.g.,
//     filesystem without a root).
//   - `resolveWorkspaceScopedProjectId` — picks the project id a
//     control surface should be scoped to: prefers an explicit
//     origin project when it still exists, otherwise falls back to
//     the origin session's project.
//   - `resolveWorkspaceScopedSessionId` — picks a session id for a
//     control surface scoped to a project: prefers a preferred
//     session if it belongs to the project, then the active
//     session, then the first session in the project. Returns
//     `null` if none match.
//   - `ControlSurfaceSessionListEntry` — discriminated union of
//     plain session rows and orchestrator-group rows, used by the
//     control surface's session list to render orchestrator
//     bundles inline alongside standalone sessions.
//   - `formatSessionOrchestratorGroupName` — cleans an orchestrator
//     template name into the heading shown above its session
//     group, falling back to the template id when the name is
//     blank.
//   - `buildControlSurfaceSessionListEntries` — groups sessions by
//     their orchestrator instance (newest-first) while preserving
//     the input session order within each group; sessions not
//     bound to an orchestrator pass through as plain entries.
//   - `buildControlSurfaceSessionListState` — derives the
//     project-scoped session list, status-filter counts,
//     search-filtered session subset, and the search-result map
//     used by the control panel session list.
//   - `mergeOrchestratorDeltaSessions` — updates an in-memory
//     sessions array in place with a delta payload, keeping
//     previously-known sessions stable and appending unknown ones,
//     then running the reconciliation pass so identity is
//     preserved for unchanged entries.
//
// What this file does NOT own:
//   - React state, hooks, or components — these helpers are pure
//     and have no side effects.
//   - The control surface components themselves
//     (`ControlPanelSurface`, `FileSystemPanel`, `GitStatusPanel`,
//     etc.).
//   - Session filtering / search primitives — those live in
//     `./session-list-filter` and `./session-find`. This module
//     composes them.
//
// Split out of `ui/src/App.tsx`. Same function signatures and
// behaviour as the inline definitions they replaced; consumers
// (including `App.test.tsx`) import from here directly — App.tsx
// does not re-export these symbols.

import type { ControlPanelSectionId } from "./panels/ControlPanelSurface";
import { reconcileSessions } from "./session-reconcile";
import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  type SessionListSearchResult,
} from "./session-find";
import {
  countSessionsByFilter,
  filterSessionListVisibleSessions,
  filterSessionsByListFilter,
  type SessionListFilter,
} from "./session-list-filter";
import type {
  OrchestratorInstance,
  Project,
  Session,
} from "./types";
import {
  createFilesystemTab,
  createGitStatusTab,
  createOrchestratorListTab,
  createProjectListTab,
  createSessionListTab,
  type WorkspaceTab,
} from "./workspace";

export function createControlPanelSectionLauncherTab(
  sectionId: ControlPanelSectionId,
  options: {
    filesystemRoot: string | null;
    gitWorkdir: string | null;
    originProjectId: string | null;
    originSessionId: string | null;
  },
): WorkspaceTab | null {
  const { filesystemRoot, gitWorkdir, originProjectId, originSessionId } =
    options;
  switch (sectionId) {
    case "files":
      return (filesystemRoot?.trim() ?? "")
        ? createFilesystemTab(filesystemRoot, originSessionId, originProjectId)
        : null;
    case "git":
      return (gitWorkdir?.trim() ?? "")
        ? createGitStatusTab(gitWorkdir, originSessionId, originProjectId)
        : null;
    case "projects":
      return createProjectListTab(originSessionId, originProjectId);
    case "sessions":
      return createSessionListTab(originSessionId, originProjectId);
    case "orchestrators":
      return createOrchestratorListTab(originSessionId, originProjectId);
  }
}

export function resolveWorkspaceScopedProjectId(
  originProjectId: string | null,
  originSessionId: string | null,
  sessionLookup: ReadonlyMap<string, Session>,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const normalizedOriginProjectId = originProjectId?.trim() ?? "";
  if (
    normalizedOriginProjectId &&
    projectLookup.has(normalizedOriginProjectId)
  ) {
    return normalizedOriginProjectId;
  }

  const originSessionProjectId = originSessionId
    ? (sessionLookup.get(originSessionId)?.projectId?.trim() ?? "")
    : "";
  return originSessionProjectId && projectLookup.has(originSessionProjectId)
    ? originSessionProjectId
    : null;
}

export function resolveWorkspaceScopedSessionId(
  projectId: string,
  preferredSessionId: string | null,
  activeSession: Session | null,
  sessions: readonly Session[],
  sessionLookup: ReadonlyMap<string, Session>,
) {
  const preferredSession = preferredSessionId
    ? (sessionLookup.get(preferredSessionId) ?? null)
    : null;
  if (preferredSession?.projectId === projectId) {
    return preferredSession.id;
  }

  if (activeSession?.projectId === projectId) {
    return activeSession.id;
  }

  return (
    sessions.find((session) => session.projectId === projectId)?.id ?? null
  );
}

export function buildControlSurfaceSessionListState(
  sessions: readonly Session[],
  selectedProject: Project | null,
  sessionListFilter: SessionListFilter,
  sessionListSearchQuery: string,
) {
  const sessionListVisibleSessions = filterSessionListVisibleSessions(sessions);
  const projectScopedSessions = selectedProject
    ? sessionListVisibleSessions.filter(
        (session) => session.projectId === selectedProject.id,
      )
    : sessionListVisibleSessions;
  const mutableProjectScopedSessions = [...projectScopedSessions];
  const sessionFilterCounts = countSessionsByFilter(
    mutableProjectScopedSessions,
  );
  const statusFilteredSessions = filterSessionsByListFilter(
    mutableProjectScopedSessions,
    sessionListFilter,
  );
  const trimmedSearchQuery = sessionListSearchQuery.trim();
  const hasSessionListSearch = trimmedSearchQuery.length > 0;

  if (!hasSessionListSearch) {
    return {
      projectScopedSessions,
      sessionFilterCounts,
      hasSessionListSearch,
      sessionListSearchResults: new Map<string, SessionListSearchResult>(),
      filteredSessions: statusFilteredSessions,
    };
  }

  const sessionListSearchResults = new Map(
    statusFilteredSessions.flatMap((session) => {
      const result = buildSessionListSearchResultFromIndex(
        buildSessionSearchIndex(session),
        trimmedSearchQuery,
      );
      return result ? ([[session.id, result]] as const) : [];
    }),
  );

  return {
    projectScopedSessions,
    sessionFilterCounts,
    hasSessionListSearch,
    sessionListSearchResults,
    filteredSessions: statusFilteredSessions.filter((session) =>
      sessionListSearchResults.has(session.id),
    ),
  };
}

export type ControlSurfaceSessionListEntry =
  | { kind: "session"; session: Session }
  | {
      kind: "orchestratorGroup";
      orchestrator: OrchestratorInstance;
      sessions: Session[];
    };

export function formatSessionOrchestratorGroupName(
  orchestrator: OrchestratorInstance,
) {
  const trimmedName = orchestrator.templateSnapshot.name.trim();
  return trimmedName.length > 0 ? trimmedName : orchestrator.templateId;
}

export function buildControlSurfaceSessionListEntries(
  sessions: readonly Session[],
  orchestrators: readonly OrchestratorInstance[],
): ControlSurfaceSessionListEntry[] {
  if (!sessions.length) {
    return [];
  }

  if (!orchestrators.length) {
    return sessions.map((session) => ({ kind: "session", session }));
  }

  const sessionOrchestrators = new Map<string, OrchestratorInstance>();
  const orderedOrchestrators = [...orchestrators].sort((left, right) =>
    right.createdAt.localeCompare(left.createdAt),
  );

  for (const orchestrator of orderedOrchestrators) {
    for (const sessionInstance of orchestrator.sessionInstances) {
      if (!sessionOrchestrators.has(sessionInstance.sessionId)) {
        sessionOrchestrators.set(sessionInstance.sessionId, orchestrator);
      }
    }
  }

  const groupedSessionsByOrchestratorId = new Map<string, Session[]>();
  const entries: ControlSurfaceSessionListEntry[] = [];

  for (const session of sessions) {
    const orchestrator = sessionOrchestrators.get(session.id);

    if (!orchestrator) {
      entries.push({ kind: "session", session });
      continue;
    }

    const groupedSessions = groupedSessionsByOrchestratorId.get(
      orchestrator.id,
    );
    if (groupedSessions) {
      groupedSessions.push(session);
      continue;
    }

    const nextGroupedSessions = [session];
    groupedSessionsByOrchestratorId.set(orchestrator.id, nextGroupedSessions);
    entries.push({
      kind: "orchestratorGroup",
      orchestrator,
      sessions: nextGroupedSessions,
    });
  }

  return entries;
}

export function mergeOrchestratorDeltaSessions(
  previousSessions: Session[],
  deltaSessions: Session[] | undefined,
) {
  if (!deltaSessions?.length) {
    return previousSessions;
  }

  const deltaSessionsById = new Map(
    deltaSessions.map((session) => [session.id, session]),
  );
  const nextSessions = previousSessions.map(
    (session) => deltaSessionsById.get(session.id) ?? session,
  );
  const knownSessionIds = new Set(nextSessions.map((session) => session.id));
  for (const session of deltaSessions) {
    if (!knownSessionIds.has(session.id)) {
      nextSessions.push(session);
      knownSessionIds.add(session.id);
    }
  }

  return reconcileSessions(previousSessions, nextSessions);
}
