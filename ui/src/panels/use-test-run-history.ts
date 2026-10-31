// Panel-local pages from the existing read-only history API. Live summaries
// overlay loaded identities; this never merges complete history into app state.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { readTestRuns } from "../test-runs-api";
import { sortTestRuns, type TestRunSummary } from "../test-runs";
import { useStableEvent } from "./use-stable-event";

interface HistoryOwner {
  projectId: string;
  controller: AbortController;
  rows: Map<string, TestRunSummary>;
  overrides: Map<string, TestRunSummary>;
  duringRead: Map<string, TestRunSummary>;
  superseded: Set<string>;
  cursor: string | null;
  failedCursor: string | undefined;
  busy: boolean;
  loaded: boolean;
  error: string | null;
}

export function useTestRunHistory(projectId: string, live: readonly TestRunSummary[]) {
  const owner = useRef<HistoryOwner | null>(null);
  const [page, setPage] = useState({ projectId, rows: [] as TestRunSummary[], cursor: null as string | null,
    busy: true, loaded: false, error: null as string | null });
  const scopedLive = useMemo(() => live.filter(run => !projectId || run.projectId === projectId), [live, projectId]);
  const currentLive = useStableEvent(() => scopedLive);
  const publish = useCallback((identity: HistoryOwner) => {
    if (owner.current !== identity || identity.controller.signal.aborted) return;
    setPage({ projectId: identity.projectId, rows: [...identity.rows.values()], cursor: identity.cursor,
      busy: identity.busy, loaded: identity.loaded, error: identity.error });
  }, []);
  const load = useStableEvent(async (identity: HistoryOwner, after?: string) => {
    if (identity.busy || identity.controller.signal.aborted) return;
    identity.busy = true; identity.error = null; identity.failedCursor = after;
    identity.duringRead = new Map(currentLive().map(run => [run.runId, run]));
    publish(identity);
    try {
      const next = await readTestRuns(identity.projectId || undefined, identity.controller.signal, after);
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      // A live completion received during the read owns that identity even
      // after a later bounded snapshot omits it. Keep overrides only for pages.
      for (const run of [...identity.duringRead.values(), ...currentLive()]) {
        identity.duringRead.set(run.runId, run);
        identity.superseded.delete(run.runId);
        for (const id of run.supersededRunIds ?? []) identity.superseded.add(id);
      }
      for (const run of next.runs) {
        if (identity.superseded.has(run.runId)) continue;
        const fresh = identity.duringRead.get(run.runId) ?? identity.overrides.get(run.runId);
        identity.rows.set(run.runId, fresh ?? run);
        if (fresh) identity.overrides.set(run.runId, fresh);
      }
      for (const id of identity.superseded) identity.rows.delete(id);
      identity.cursor = next.nextCursor; identity.loaded = true;
    } catch (error) {
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      identity.error = error instanceof Error ? error.message : String(error);
    } finally {
      identity.busy = false; identity.duringRead.clear();
      publish(identity);
    }
  });
  useEffect(() => {
    const identity: HistoryOwner = { projectId, controller: new AbortController(), rows: new Map(), overrides: new Map(),
      duringRead: new Map(), superseded: new Set(), cursor: null, failedCursor: undefined,
      busy: false, loaded: false, error: null };
    owner.current = identity;
    void load(identity);
    return () => { identity.controller.abort(); if (owner.current === identity) owner.current = null; };
  }, [projectId, load]);
  useEffect(() => {
    const identity = owner.current;
    if (!identity || identity.projectId !== projectId) return;
    let changed = false;
    for (const run of scopedLive) {
      // The existing stream can restore an observation when its artifact's
      // exact association disappears. An earlier alias eviction is not a tombstone.
      identity.superseded.delete(run.runId);
      if (identity.busy) identity.duringRead.set(run.runId, run);
      if (identity.rows.has(run.runId) && identity.rows.get(run.runId) !== run) {
        identity.rows.set(run.runId, run); identity.overrides.set(run.runId, run); changed = true;
      }
      for (const id of run.supersededRunIds ?? []) {
        identity.superseded.add(id);
        changed = identity.rows.delete(id) || changed;
        identity.overrides.delete(id);
      }
    }
    if (changed) publish(identity);
  }, [projectId, scopedLive, publish]);
  const rows = page.projectId === projectId ? page.rows : [];
  const runs = useMemo(() => {
    const merged = new Map(rows.map(run => [run.runId, run]));
    for (const run of scopedLive) merged.set(run.runId, run);
    for (const run of scopedLive) for (const id of run.supersededRunIds ?? []) merged.delete(id);
    return sortTestRuns([...merged.values()]);
  }, [rows, scopedLive]);
  return { runs, busy: page.projectId !== projectId || page.busy,
    loaded: page.projectId === projectId && page.loaded,
    error: page.projectId === projectId ? page.error : null,
    hasMore: page.projectId === projectId && page.cursor !== null,
    more: () => { const identity = owner.current; if (identity?.cursor) void load(identity, identity.cursor); },
    retry: () => { const identity = owner.current; if (identity) void load(identity, identity.failedCursor); },
  };
}
