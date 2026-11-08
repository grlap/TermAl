// Modern panel-local history: explicit pagination and refresh of the first
// and visible pages. No background drain, global rows, or alias tombstones.
import { useEffect, useRef, useState } from "react";
import { readTestRuns, type TestRunFilters, type TestRunPage } from "../test-runs-api";
import { sortTestRuns, type TestRunSummary } from "../test-runs";
import type { TestRunViewRevision } from "../test-run-overview";
import { useStableEvent } from "./use-stable-event";

type Page = { after: string | undefined; data: TestRunPage; version: number; revision: number };
interface PageOwner {
  key: string; projectId: string; filters: TestRunFilters; revision: TestRunViewRevision;
  controller: AbortController; pages: Page[]; visible: ReadonlySet<string>;
  requests: Map<string | undefined, AbortController>; error: string | null; failed: string | undefined;
  continuations: Map<string, number>; failedCount: number;
}

export function useTestRunPages(projectId: string, filters: TestRunFilters, revision: TestRunViewRevision | undefined) {
  const key = JSON.stringify([projectId, filters.sessionId, filters.state, filters.view, revision?.serverInstanceId]);
  const owner = useRef<PageOwner | undefined>(undefined);
  const [page, setPage] = useState({ key, runs: [] as TestRunSummary[], busy: false, loaded: false,
    cursor: null as string | null, error: null as string | null });
  const publish = useStableEvent((identity: PageOwner) => {
    if (owner.current !== identity || identity.controller.signal.aborted) return;
    const rows = new Map<string, { run: TestRunSummary; revision: number }>();
    for (const part of identity.pages) {
      for (const run of part.data.runs) {
        if ((rows.get(run.runId)?.revision ?? -1) <= part.revision) rows.set(run.runId, { run, revision: part.revision });
      }
    }
    // Canonical associations belong to the server; discard aliases only while
    // their canonical row is present, so a later restoration can be read again.
    for (const { run, revision } of rows.values()) for (const id of run.supersededRunIds ?? []) {
      // A newer exact page may restore an observation after an association
      // disappears. An older canonical page cannot reapply that eviction.
      if ((rows.get(id)?.revision ?? Infinity) <= revision) rows.delete(id);
    }
    setPage({ key: identity.key, runs: sortTestRuns([...rows.values()].map(row => row.run)), busy: identity.requests.size > 0,
      loaded: identity.pages.length > 0, cursor: identity.pages[identity.pages.length - 1]?.data.nextCursor ?? null, error: identity.error });
  });
  const load = useStableEvent(async (identity: PageOwner, after?: string): Promise<void> => {
    if (owner.current !== identity || identity.controller.signal.aborted || identity.requests.has(after)) return;
    const controller = new AbortController();
    identity.requests.set(after, controller);
    identity.error = null;
    const expected = identity.revision;
    publish(identity);
    try {
      const data = await readTestRuns(identity.projectId || undefined, controller.signal, after, identity.filters);
      if (owner.current !== identity || controller.signal.aborted || identity.revision !== expected) return;
      if (data.serverInstanceId !== expected.serverInstanceId || data.revision === undefined || data.revision < expected.revision) {
        throw new Error("History response is older than the current state. Refresh history to try again.");
      }
      const index = identity.pages.findIndex(part => part.after === after);
      const next = { after, data, version: expected.version, revision: data.revision };
      let remaining = (after ? identity.continuations.get(after) ?? 1 : 1) - 1;
      if (after) identity.continuations.delete(after);
      if (index >= 0 && identity.pages[index].data.nextCursor !== data.nextCursor) {
        // A refreshed boundary can move. Reconnect only the pages already
        // loaded or explicitly requested, starting at the new strict cursor.
        const removed = identity.pages.splice(index + 1);
        remaining += removed.length;
        const obsolete = [...removed.map(part => part.after), ...identity.continuations.keys()];
        for (const cursor of obsolete) {
          identity.requests.get(cursor)?.abort();
          identity.requests.delete(cursor);
        }
        for (const count of identity.continuations.values()) remaining += count;
        identity.continuations.clear();
      }
      if (index < 0) identity.pages.push(next); else identity.pages[index] = next;
      if (data.nextCursor && remaining > 0) {
        identity.continuations.set(data.nextCursor, remaining);
        void load(identity, data.nextCursor);
      }
    } catch (error) {
      if (!controller.signal.aborted && owner.current === identity && identity.revision === expected) {
        identity.error = error instanceof Error ? error.message : String(error);
        identity.failed = after;
        identity.failedCount = after ? identity.continuations.get(after) ?? 1 : 1;
        if (after) identity.continuations.delete(after);
      }
    } finally {
      if (identity.requests.get(after) === controller) identity.requests.delete(after);
      publish(identity);
    }
  });
  const refreshVisible = useStableEvent((identity: PageOwner) => {
    if (!identity.pages.length) { void load(identity); return; }
    for (const part of identity.pages) {
      if (part.version !== identity.revision.version && (part.after === undefined || part.data.runs.some(run => identity.visible.has(run.runId)))) {
        void load(identity, part.after);
      }
    }
    for (const after of identity.continuations.keys()) void load(identity, after);
  });
  useEffect(() => {
    if (!revision) return;
    const identity: PageOwner = { key, projectId, filters, revision, controller: new AbortController(),
      pages: [], visible: new Set(), requests: new Map(), continuations: new Map(), error: null, failed: undefined, failedCount: 1 };
    owner.current = identity;
    void load(identity);
    return () => {
      identity.controller.abort();
      for (const request of identity.requests.values()) request.abort();
      if (owner.current === identity) owner.current = undefined;
    };
    // Filters are represented by key; revisions invalidate existing pages.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, !!revision, load]);
  useEffect(() => {
    const identity = owner.current;
    if (!revision || !identity || identity.key !== key || identity.revision === revision) return;
    for (const request of identity.requests.values()) request.abort();
    identity.requests.clear();
    identity.revision = revision;
    refreshVisible(identity);
  }, [key, revision, refreshVisible]);
  const visible = useStableEvent((ids: readonly string[]) => {
    const identity = owner.current;
    if (!identity || identity.key !== key) return;
    identity.visible = new Set(ids);
    refreshVisible(identity);
  });
  const current = page.key === key;
  return { runs: current ? page.runs : [], busy: !current || page.busy,
    loaded: current && page.loaded, error: current ? page.error : null, hasMore: current && page.cursor !== null,
    more: () => { const identity = owner.current; const after = identity?.pages[identity.pages.length - 1]?.data.nextCursor;
      if (identity && after && !identity.requests.size) { identity.continuations.set(after, 1); void load(identity, after); } },
    retry: () => { const identity = owner.current; if (identity) {
      if (identity.failed) identity.continuations.set(identity.failed, identity.failedCount);
      void load(identity, identity.failed);
    } }, visible };
}
