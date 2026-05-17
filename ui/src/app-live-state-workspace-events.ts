// Owns: the buffered workspace-files-changed event gate used by app live state.
// Does not own: EventSource listeners, React state ownership, or workspace UI consumers.
// Split from: ui/src/app-live-state.ts.

import { startTransition, type MutableRefObject } from "react";
import type { WorkspaceFilesChangedEvent } from "./types";
import { mergeWorkspaceFilesChangedEvents } from "./workspace-file-events";

export type WorkspaceFilesChangedEventGateRefs = {
  bufferRef: MutableRefObject<WorkspaceFilesChangedEvent | null>;
  flushTimeoutRef: MutableRefObject<number | null>;
  lastRevisionRef: MutableRefObject<number | null>;
};

type FlushWorkspaceFilesChangedEventBufferOptions = {
  gateRefs: WorkspaceFilesChangedEventGateRefs;
  isMountedRef: MutableRefObject<boolean>;
  setWorkspaceFilesChangedEvent: (
    event: WorkspaceFilesChangedEvent,
  ) => void;
};

export function flushWorkspaceFilesChangedEventBuffer({
  gateRefs,
  isMountedRef,
  setWorkspaceFilesChangedEvent,
}: FlushWorkspaceFilesChangedEventBufferOptions) {
  gateRefs.flushTimeoutRef.current = null;
  const bufferedEvent = gateRefs.bufferRef.current;
  gateRefs.bufferRef.current = null;
  if (!bufferedEvent || !isMountedRef.current) {
    return;
  }

  startTransition(() => {
    setWorkspaceFilesChangedEvent(bufferedEvent);
  });
}

export function clearWorkspaceFilesChangedEventBuffer(
  gateRefs: WorkspaceFilesChangedEventGateRefs,
) {
  if (gateRefs.flushTimeoutRef.current !== null) {
    window.clearTimeout(gateRefs.flushTimeoutRef.current);
    gateRefs.flushTimeoutRef.current = null;
  }
  gateRefs.bufferRef.current = null;
}

export function resetWorkspaceFilesChangedEventGate(
  gateRefs: WorkspaceFilesChangedEventGateRefs,
) {
  gateRefs.lastRevisionRef.current = null;
  clearWorkspaceFilesChangedEventBuffer(gateRefs);
}

export function enqueueWorkspaceFilesChangedEvent(
  gateRefs: WorkspaceFilesChangedEventGateRefs,
  filesChanged: WorkspaceFilesChangedEvent,
  flushBuffer: () => void,
) {
  const lastRevision = gateRefs.lastRevisionRef.current;
  if (lastRevision !== null && filesChanged.revision < lastRevision) {
    return;
  }

  gateRefs.lastRevisionRef.current = filesChanged.revision;
  gateRefs.bufferRef.current = mergeWorkspaceFilesChangedEvents(
    gateRefs.bufferRef.current,
    filesChanged,
  );

  if (gateRefs.flushTimeoutRef.current !== null) {
    return;
  }

  gateRefs.flushTimeoutRef.current = window.setTimeout(flushBuffer, 0);
}
