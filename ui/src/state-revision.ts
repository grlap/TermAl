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
  },
): boolean {
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
