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
     * both ids are non-empty and differ, the caller must also opt into
     * accepting unknown ids via `allowUnknownServerInstance`. Empty ids (from
     * older servers or fallback payloads) are treated as "unknown" and cannot
     * trigger the restart branch.
     */
    nextServerInstanceId?: string | null;
    /**
     * All non-empty server instance ids this browser tab has already
     * adopted. Used to reject late responses from older server
     * instances after a newer restart was already adopted.
     */
    seenServerInstanceIds?: ReadonlySet<string>;
    /**
     * Unknown mismatched instance ids are only trustworthy on explicit
     * restart-recovery paths. Ordinary snapshots/actions must not infer
     * "new backend" from an unseen id because a late old-process response can
     * have an id this tab never adopted.
     */
    allowUnknownServerInstance?: boolean;
  },
): boolean {
  // A previously seen mismatched id is always a late old-process response.
  // An unseen mismatched id is only accepted when the caller has independent
  // restart evidence (SSE reconnect state, fallback/manual recovery, or a
  // recovery probe requested after detecting a cross-instance response).
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
    return options?.allowUnknownServerInstance === true;
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
 * Returns true when a same-server-instance full snapshot is stale relative to
 * the state this tab already adopted. Action handlers use this to treat an
 * HTTP 200 response as UI success when SSE or another snapshot already landed
 * a newer same-instance revision.
 *
 * This relies on the backend's monotonic-revision invariant: within one
 * `serverInstanceId`, revision N represents a state at least as new as every
 * revision <= N. If two different snapshots could share the same
 * `(serverInstanceId, revision)`, stale action responses could be reported as
 * success without the corresponding mutation being visible locally.
 */
export function isStaleSameInstanceSnapshot(
  currentRevision: number | null,
  nextRevision: number,
  currentServerInstanceId: string | null | undefined,
  nextServerInstanceId: string | null | undefined,
): boolean {
  return (
    currentRevision !== null &&
    nextRevision <= currentRevision &&
    Boolean(currentServerInstanceId) &&
    currentServerInstanceId === nextServerInstanceId
  );
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
