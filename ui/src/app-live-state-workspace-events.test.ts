import { describe, expect, it, vi } from "vitest";
import {
  clearWorkspaceFilesChangedEventBuffer,
  enqueueWorkspaceFilesChangedEvent,
  flushWorkspaceFilesChangedEventBuffer,
  resetWorkspaceFilesChangedEventGate,
  type WorkspaceFilesChangedEventGateRefs,
} from "./app-live-state-workspace-events";
import type { WorkspaceFilesChangedEvent } from "./types";

function event(
  revision: number,
  path: string,
): WorkspaceFilesChangedEvent {
  return {
    revision,
    changes: [{ path, kind: "modified" }],
  };
}

function ref<T>(current: T) {
  return { current };
}

function createGateRefs(): WorkspaceFilesChangedEventGateRefs {
  return {
    bufferRef: ref<WorkspaceFilesChangedEvent | null>(null),
    flushTimeoutRef: ref<number | null>(null),
    lastRevisionRef: ref<number | null>(null),
  };
}

describe("app live state workspace file event gate", () => {
  it("buffers same-tick events and ignores stale revisions", () => {
    vi.useFakeTimers();
    try {
      const gateRefs = createGateRefs();
      const observed: WorkspaceFilesChangedEvent[] = [];
      const flushBuffer = () =>
        flushWorkspaceFilesChangedEventBuffer({
          gateRefs,
          isMountedRef: ref(true),
          setWorkspaceFilesChangedEvent: (nextEvent) => {
            observed.push(nextEvent);
          },
        });

      enqueueWorkspaceFilesChangedEvent(gateRefs, event(4, "/repo/a.ts"), flushBuffer);
      enqueueWorkspaceFilesChangedEvent(gateRefs, event(3, "/repo/stale.ts"), flushBuffer);
      enqueueWorkspaceFilesChangedEvent(gateRefs, event(4, "/repo/b.ts"), flushBuffer);

      expect(observed).toEqual([]);
      expect(gateRefs.flushTimeoutRef.current).not.toBeNull();

      vi.runOnlyPendingTimers();

      expect(observed).toEqual([
        {
          revision: 4,
          changes: [
            { path: "/repo/a.ts", kind: "modified" },
            { path: "/repo/b.ts", kind: "modified" },
          ],
        },
      ]);
      expect(gateRefs.bufferRef.current).toBeNull();
      expect(gateRefs.flushTimeoutRef.current).toBeNull();
      expect(gateRefs.lastRevisionRef.current).toBe(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears or resets buffered events without publishing them", () => {
    vi.useFakeTimers();
    try {
      const gateRefs = createGateRefs();
      const observed: WorkspaceFilesChangedEvent[] = [];
      const flushBuffer = () =>
        flushWorkspaceFilesChangedEventBuffer({
          gateRefs,
          isMountedRef: ref(true),
          setWorkspaceFilesChangedEvent: (nextEvent) => {
            observed.push(nextEvent);
          },
        });

      enqueueWorkspaceFilesChangedEvent(gateRefs, event(2, "/repo/a.ts"), flushBuffer);
      clearWorkspaceFilesChangedEventBuffer(gateRefs);
      vi.runOnlyPendingTimers();

      expect(observed).toEqual([]);
      expect(gateRefs.lastRevisionRef.current).toBe(2);

      enqueueWorkspaceFilesChangedEvent(gateRefs, event(3, "/repo/b.ts"), flushBuffer);
      resetWorkspaceFilesChangedEventGate(gateRefs);
      vi.runOnlyPendingTimers();

      expect(observed).toEqual([]);
      expect(gateRefs.bufferRef.current).toBeNull();
      expect(gateRefs.flushTimeoutRef.current).toBeNull();
      expect(gateRefs.lastRevisionRef.current).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
