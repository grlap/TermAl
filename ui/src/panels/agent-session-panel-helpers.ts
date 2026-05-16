// Owns: small pure helpers used by AgentSessionPanel orchestration.
// Does not own: panel rendering, composer state, or backend request side effects.
// Split from: ui/src/panels/AgentSessionPanel.tsx.

import type { ResolveAgentCommandResponse } from "../api";
import type { CreateComposerDelegationOptions } from "../delegation-commands";
import type { ConversationMarker } from "../types";

export type PendingCreatedConversationMarker = {
  localId: number;
  messageId: string;
  name: string | null;
  existingMarkerIds: ReadonlySet<string>;
  resolvedMarkerId?: string;
};

export type SpawnDelegationOptions = CreateComposerDelegationOptions;

export function findNewPendingCreatedConversationMarker(
  markers: readonly ConversationMarker[],
  pendingMarker: PendingCreatedConversationMarker,
  usedMarkerIds: ReadonlySet<string>,
) {
  for (const marker of markers) {
    if (
      pendingMarker.existingMarkerIds.has(marker.id) ||
      usedMarkerIds.has(marker.id)
    ) {
      continue;
    }
    if (pendingMarker.name && marker.name.trim() !== pendingMarker.name) {
      continue;
    }
    return marker;
  }
  return null;
}

export function spawnDelegationOptionsFromResolvedCommand(
  resolved: ResolveAgentCommandResponse,
): SpawnDelegationOptions | undefined {
  const title = resolved.delegation?.title ?? resolved.title ?? undefined;
  const mode = resolved.delegation?.mode ?? undefined;
  const writePolicy = resolved.delegation?.writePolicy ?? undefined;
  if (!title && !mode && !writePolicy) {
    return undefined;
  }
  return {
    ...(title ? { title } : {}),
    ...(mode ? { mode } : {}),
    ...(writePolicy ? { writePolicy } : {}),
  };
}

export function isSpaceKey(event: {
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
