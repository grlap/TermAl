export function shouldAdoptStateRevision(
  currentRevision: number | null,
  nextRevision: number,
): boolean {
  return currentRevision === null || nextRevision > currentRevision;
}

export function shouldAdoptSnapshotRevision(
  currentRevision: number | null,
  nextRevision: number,
  options?: {
    force?: boolean;
    allowRevisionDowngrade?: boolean;
    /**
     * The `serverInstanceId` the client last adopted. Used together
     * with `nextServerInstanceId` below to deterministically detect a
     * server restart. Pass `null` (or omit) when you have not yet
     * observed any snapshot.
     */
    lastSeenServerInstanceId?: string | null;
    /**
     * The `serverInstanceId` carried by the incoming snapshot. When
     * both ids are non-empty and differ, the server has restarted and
     * its revision counter has rewound to whatever value SQLite held
     * — accept the snapshot unconditionally regardless of
     * `force` / `allowRevisionDowngrade` and regardless of whether
     * `nextRevision < currentRevision`. Empty ids (from older servers
     * or fallback payloads) are treated as "unknown" and cannot
     * trigger the restart branch.
     */
    nextServerInstanceId?: string | null;
  },
): boolean {
  // Server restart detection is authoritative: if the server id just
  // changed, every monotonic assumption about the revision counter is
  // invalid (the counter just rewound to a stored value on the fresh
  // instance). Accept the snapshot so the client resyncs to the
  // restarted server. This path closes both the "prompt invisible
  // after server restart" bug and the "safety-net poll forces
  // downgrade every tick" bug.
  if (
    isServerInstanceMismatch(
      options?.lastSeenServerInstanceId,
      options?.nextServerInstanceId,
    )
  ) {
    return true;
  }

  if (!options?.force) {
    return shouldAdoptStateRevision(currentRevision, nextRevision);
  }

  if (
    !options.allowRevisionDowngrade &&
    currentRevision !== null &&
    nextRevision < currentRevision
  ) {
    return false;
  }

  return true;
}

/**
 * Returns true when both ids are non-empty AND differ. Empty ids mean
 * "unknown instance" (older server, fallback payload) and cannot
 * trigger a restart branch — the caller stays on the revision-ordered
 * path.
 */
export function isServerInstanceMismatch(
  current: string | null | undefined,
  next: string | null | undefined,
): boolean {
  if (!current || !next) {
    return false;
  }
  return current !== next;
}

export type DeltaRevisionAction = "apply" | "ignore" | "resync";

export function decideDeltaRevisionAction(
  currentRevision: number | null,
  nextRevision: number,
): DeltaRevisionAction {
  if (currentRevision === null) {
    return "resync";
  }

  if (nextRevision <= currentRevision) {
    return "ignore";
  }

  if (nextRevision !== currentRevision + 1) {
    return "resync";
  }

  return "apply";
}
