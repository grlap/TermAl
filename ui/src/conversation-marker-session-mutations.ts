// Owns pure local-session marker insert/update/delete transformations.
// Does not own marker HTTP requests, stale-response recovery, or marker UI.
// Split from app-session-actions.ts to keep action orchestration smaller.
import type { ConversationMarker, Session } from "./types";

export function upsertConversationMarkerLocally(
  session: Session,
  marker: ConversationMarker,
  sessionMutationStamp?: number | null,
): Session {
  const markers = session.markers ?? [];
  const markerIndex = markers.findIndex((entry) => entry.id === marker.id);
  if (markerIndex === -1) {
    return {
      ...session,
      markers: [...markers, marker],
      ...(sessionMutationStamp !== undefined ? { sessionMutationStamp } : {}),
    };
  }

  const updatedMarkers = markers.slice();
  updatedMarkers[markerIndex] = marker;
  return {
    ...session,
    markers: updatedMarkers,
    ...(sessionMutationStamp !== undefined ? { sessionMutationStamp } : {}),
  };
}

export function deleteConversationMarkerLocally(
  session: Session,
  markerId: string,
  sessionMutationStamp?: number | null,
): Session {
  const nextMarkers = (session.markers ?? []).filter(
    (marker) => marker.id !== markerId,
  );
  return {
    ...session,
    markers: nextMarkers,
    ...(sessionMutationStamp !== undefined ? { sessionMutationStamp } : {}),
  };
}
