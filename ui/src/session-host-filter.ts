// Owns host matching and choices for session lists using already loaded data.
// Does not fetch host/session state or own project, status, search or row windows.
import { LOCAL_REMOTE_ID, remoteDisplayName, resolveSessionRemoteId } from "./remotes";
import type { ComboboxOption } from "./session-model-utils";
import type { Project, RemoteConfig, Session } from "./types";

export const ALL_SESSION_HOSTS = "__all_hosts__";

export function filterSessionsByHost(
  sessions: Session[], host: string, projects: ReadonlyMap<string, Project>,
): Session[] {
  if (host === ALL_SESSION_HOSTS) return sessions;
  return sessions.filter(session =>
    resolveSessionRemoteId(session, session.projectId ? projects.get(session.projectId) : null) === host);
}

export function sessionHostOptions(remotes: ReadonlyMap<string, RemoteConfig>): ComboboxOption[] {
  return [
    { value: ALL_SESSION_HOSTS, label: "All hosts" },
    { value: LOCAL_REMOTE_ID, label: "Local", description: "This machine" },
    ...Array.from(remotes.values()).filter(remote => remote.id !== LOCAL_REMOTE_ID).map(remote => ({
      value: remote.id,
      label: remoteDisplayName(remote),
      description: remote.host || undefined,
    })),
  ];
}
