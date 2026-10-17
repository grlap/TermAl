// New Work list lifetime and pagination owner. Does not render rows or change
// source data. Display filters never own or restart a loading generation.
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiRequestError } from "../api-request";
import { readProjectWork, type WorkListResponse } from "../work-visualizer-api";

export type WorkListResult = WorkListResponse;

// A stale reader invalidates every row and detail in the loaded generation.
// Keep its explicit source error so the view can restore keyboard focus.
function withoutEngramGeneration(previous: WorkListResult | null, message: string): WorkListResult | null {
  return previous && {
    ...previous, page: null, readerId: null,
    sources: previous.sources.map(source => ({ ...source, state: "error", message })),
  };
}

export function useWorkList(projectId: string) {
  const [result, setResult] = useState<WorkListResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [needsReload, setNeedsReload] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const owner = useRef<{ controller: AbortController; busy: boolean; failed: boolean } | null>(null);
  const current = useRef<WorkListResult | null>(null);

  const load = useCallback(async (identity: NonNullable<typeof owner.current>, more: boolean, retry = false) => {
    if (identity.busy || identity.controller.signal.aborted || (identity.failed && !retry)) return;
    const previous = current.current;
    if (more && (!previous?.page?.more || !previous.page.after || !previous.readerId)) return;
    identity.busy = true; identity.failed = false; setBusy(true); setError(null);
    try {
      let next: WorkListResult = await readProjectWork(projectId, { search: "", label: "", availability: "" }, identity.controller.signal,
        more ? { after: previous!.page!.after!, readerId: previous!.readerId! } : undefined);
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      if (more) {
        const oldPage = previous?.page;
        const page = next.page;
        if (!oldPage || !page || next.readerId !== previous.readerId
          || page.total !== oldPage.total || page.shownBefore !== oldPage.items.length
          || page.items.some(row => oldPage.items.some(old => old.id === row.id))) {
          const message = "Work page generation changed. Refresh to read a fresh list.";
          const kept = withoutEngramGeneration(previous, message);
          current.current = kept; setResult(kept);
          setNeedsReload(true);
          throw new Error(message);
        }
        if (page.more && page.after && (page.after === oldPage.after || page.items.length === 0)) {
          throw new Error("Work page made no progress. Retry this page or refresh the list.");
        }
        next = { ...next, page: { ...page, items: [...oldPage.items, ...page.items] } };
      }
      current.current = next; setResult(next);
    } catch (e) {
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      const message = e instanceof Error ? e.message : String(e);
      identity.failed = true;
      if (e instanceof ApiRequestError && e.status === 409) {
        const kept = withoutEngramGeneration(previous, "Work list changed. Reload the list to continue.");
        current.current = kept; setResult(kept);
        setNeedsReload(true);
        setError(`Work list changed. Reload the list to continue. ${message}`);
      } else {
        setError(message);
      }
    } finally {
      identity.busy = false;
      if (owner.current === identity && !identity.controller.signal.aborted) setBusy(false);
    }
  }, [projectId]);

  useEffect(() => {
    const identity = { controller: new AbortController(), busy: false, failed: false };
    owner.current = identity; current.current = null; setResult(null); setError(null); setBusy(false); setNeedsReload(false);
    if (projectId) void load(identity, false);
    return () => { identity.controller.abort(); if (owner.current === identity) owner.current = null; };
  }, [load, projectId, refresh]);

  const more = useCallback(() => { if (owner.current) void load(owner.current, true); }, [load]);
  // Receipts can be shorter than the requested limit. Only the cursor's own
  // more/after pair advances the serial chain; failures pause until Retry.
  useEffect(() => {
    if (!error && result?.page?.more && result.page.after) more();
  }, [result, error, more]);
  const retry = () => { if (owner.current && !needsReload) void load(owner.current, !!current.current?.page?.after, true); };
  return { result, error, busy, needsReload, retry, reload: () => setRefresh(v => v + 1) };
}
