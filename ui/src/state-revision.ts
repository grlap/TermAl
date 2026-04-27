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
     * both ids are non-empty and differ, the server has restarted only
     * if the incoming id is new to this browser tab. Empty ids (from
     * older servers or fallback payloads) are treated as "unknown" and
     * cannot trigger the restart branch.
     */
    nextServerInstanceId?: string | null;
    /**
     * All non-empty server instance ids this browser tab has already
     * adopted. Used to reject late responses from older server
     * instances after a newer restart was already adopted.
     */
    seenServerInstanceIds?: ReadonlySet<string>;
  },
): boolean {
  // Server restart detection is authoritative only for unseen ids. If
  // the id changed to a new instance, every monotonic assumption about
  // the revision counter is invalid. If the changed id was already
  // seen, it is a late response from an older instance and must not
  // bypass the monotonic guard.
  if (
    isServerInstanceMismatch(
      options?.lastSeenServerInstanceId,
      options?.nextServerInstanceId,
    )
  ) {
    if (
      options?.nextServerInstanceId &&
      options.seenServerInstanceIds?.has(options.nextServerInstanceId)
    ) {
      return false;
    }
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
