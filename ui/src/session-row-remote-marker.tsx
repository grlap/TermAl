// session-row-remote-marker.tsx
//
// Owns: deciding whether a session list tile belongs to a remote-hosted
// session, the wording of its remote label, and the small marker the tile
// shows for it (a globe glyph with an accessible label and a tooltip).
//
// Does not own: the tile itself or its placement rules (the tile is rendered
// by AppControlSurface.tsx, and styles.css places the marker just left of the
// tile's agent-icon status button), the remote settings, or how a session's
// remote is resolved (remotes.ts).
//
// New module, not split from an existing file.

import { isLocalRemoteId, remoteConnectionLabel, remoteDisplayName, resolveSessionRemoteId } from "./remotes";
import type { Project, RemoteConfig, Session } from "./types";

export type SessionRemoteDescription = {
  remoteId: string;
  /** Names the session as remote, with the host where the remote config gives one. */
  label: string;
};

/**
 * Describes the remote host of a session, or returns null for a local one.
 * A session's own remote id wins over its project's, as in the session tab
 * tooltip's location row.
 */
export function describeSessionRemote(
  session: Pick<Session, "remoteId">,
  project: Pick<Project, "remoteId"> | null | undefined,
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
): SessionRemoteDescription | null {
  const remoteId = resolveSessionRemoteId(session, project);
  if (isLocalRemoteId(remoteId)) {
    return null;
  }
  const remote = remoteLookup.get(remoteId);
  const host = remote ? `${remoteDisplayName(remote, remoteId)} (${remoteConnectionLabel(remote)})` : remoteId;
  return { remoteId, label: `Remote session on ${host}` };
}

export function SessionRowRemoteMarker({ remote }: { remote: SessionRemoteDescription }) {
  return (
    <span
      className="session-row-remote-marker"
      role="img"
      aria-label={remote.label}
      title={remote.label}
      data-remote-id={remote.remoteId}
    >
      <svg
        className="session-row-remote-marker-symbol"
        viewBox="0 0 16 16"
        aria-hidden="true"
        focusable="false"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
      >
        <circle cx="8" cy="8" r="6.25" />
        <path d="M1.75 8h12.5" />
        <path d="M8 1.75c1.7 1.75 2.55 3.83 2.55 6.25S9.7 12.5 8 14.25" />
        <path d="M8 1.75C6.3 3.5 5.45 5.58 5.45 8S6.3 12.5 8 14.25" />
      </svg>
    </span>
  );
}
