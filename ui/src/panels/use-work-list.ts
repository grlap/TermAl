// New Work list lifetime and pagination owner. Does not render rows or change
// source data. Abort plus generation fencing rejects late project/filter reads.
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiRequestError } from "../api-request";
import { readProjectWork, type WorkFilters, type WorkListResponse } from "../work-visualizer-api";

export function useWorkList(projectId: string, filters: WorkFilters) {
  const [result, setResult] = useState<WorkListResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const owner = useRef<{ controller: AbortController; busy: boolean } | null>(null);
  const current = useRef<WorkListResponse | null>(null);
  const { search, label, availability } = filters;

  const load = useCallback(async (identity: NonNullable<typeof owner.current>, more: boolean) => {
    if (identity.busy || identity.controller.signal.aborted) return;
    const previous = current.current;
    if (more && (!previous?.page?.after || !previous.readerSessionId)) return;
    identity.busy = true; setBusy(true); setError(null);
    try {
      const next = await readProjectWork(projectId, { search, label, availability }, identity.controller.signal,
        more ? { after: previous!.page!.after!, readerSessionId: previous!.readerSessionId! } : undefined);
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      if (more) {
        const oldPage = previous?.page;
        const page = next.page;
        if (!oldPage || !page || next.readerSessionId !== previous.readerSessionId
          || page.total !== oldPage.total || page.shownBefore !== oldPage.items.length
          || page.items.some(row => oldPage.items.some(old => old.id === row.id))) {
          current.current = null; setResult(null);
          throw new Error("Work page generation changed. Refresh to read a fresh list.");
        }
        page.items = [...oldPage.items, ...page.items];
      }
      current.current = next; setResult(next);
    } catch (e) {
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      if (e instanceof ApiRequestError && e.status === 409) { current.current = null; setResult(null); }
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      identity.busy = false;
      if (owner.current === identity && !identity.controller.signal.aborted) setBusy(false);
    }
  }, [projectId, search, label, availability]);

  useEffect(() => {
    const identity = { controller: new AbortController(), busy: false };
    owner.current = identity; current.current = null; setResult(null); setError(null); setBusy(false);
    if (projectId) void load(identity, false);
    return () => { identity.controller.abort(); if (owner.current === identity) owner.current = null; };
  }, [load, projectId, refresh]);

  return { result, error, busy, reload: () => setRefresh(v => v + 1), more: () => { if (owner.current) void load(owner.current, true); } };
}
