export function shouldAdoptStateRevision(
  currentRevision: number | null,
  nextRevision: number,
): boolean {
  return currentRevision === null || nextRevision > currentRevision;
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
