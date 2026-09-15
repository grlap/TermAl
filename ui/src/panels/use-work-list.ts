// New Work list lifetime and pagination owner. Does not render rows or change
// source data. Abort plus generation fencing rejects late project/filter reads.
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiRequestError } from "../api-request";
import { readProjectWork, type WorkFilters, type WorkListResponse } from "../work-visualizer-api";

// The list as the panel sees it: the server's response plus, once a
// continuation has re-read only Engram, the time the retained Beads snapshot
// was actually read, so the caption never dates it to a later Engram page.
export type WorkListResult = WorkListResponse & { beadsObservedAt?: string };

// Engram fits each receipt into 12 KiB, so a verbose page holds about a
// dozen rows and the first read alone is a thin slice. The list follows the
// continuation cursor on its own until it holds this many Engram rows or the
// source has no more, at most this many pages per generation; a page without
// a cursor (byte-limited) stops here with its hint, and the manual control
// continues past the target.
export const WORK_AUTO_LOAD_TARGET_ROWS = 200;
export const WORK_AUTO_LOAD_MAX_PAGES = 40;

// The Engram generation is gone (invalid cursor, changed reader, changed
// counts). The Beads snapshot was read independently on the first page and
// stays; Engram becomes an explicit per-source error until the next refresh.
function withoutEngramGeneration(previous: WorkListResult | null, message: string): WorkListResult | null {
  if (!previous?.beads) return null;
  return {
    ...previous,
    page: null,
    readerId: null,
    sources: previous.sources.map(source => source.source === "engram" ? { ...source, state: "error", message } : source),
  };
}

export function useWorkList(projectId: string, filters: WorkFilters) {
  const [result, setResult] = useState<WorkListResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const owner = useRef<{ controller: AbortController; busy: boolean } | null>(null);
  const current = useRef<WorkListResult | null>(null);
  const autoPages = useRef(0);
  // What the last automatic continuation was launched from: a page that
  // returns the same cursor or no new rows is not progress, and the chain
  // stops rather than re-reading it until the page cap.
  const autoLaunch = useRef<{ cursor: string; rows: number } | null>(null);
  const { search, label, availability } = filters;

  const load = useCallback(async (identity: NonNullable<typeof owner.current>, more: boolean) => {
    if (identity.busy || identity.controller.signal.aborted) return;
    const previous = current.current;
    if (more && (!previous?.page?.after || !previous.readerId)) return;
    identity.busy = true; setBusy(true); setError(null);
    try {
      const next: WorkListResult = await readProjectWork(projectId, { search, label, availability }, identity.controller.signal,
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
          throw new Error(message);
        }
        page.items = [...oldPage.items, ...page.items];
        // A continuation re-reads only the Engram page; the Beads snapshot,
        // its source status and its read time stay exactly as the first page
        // reported them.
        next.beads = previous.beads;
        if (previous.beads) next.beadsObservedAt = previous.beadsObservedAt ?? previous.observedAt;
        const beadsStatus = previous.sources.find(source => source.source === "beads");
        if (beadsStatus) next.sources = next.sources.map(source => source.source === "beads" ? beadsStatus : source);
      }
      current.current = next; setResult(next);
    } catch (e) {
      if (owner.current !== identity || identity.controller.signal.aborted) return;
      const message = e instanceof Error ? e.message : String(e);
      if (e instanceof ApiRequestError && e.status === 409) {
        const kept = withoutEngramGeneration(previous, message);
        current.current = kept; setResult(kept);
      }
      setError(message);
    } finally {
      identity.busy = false;
      if (owner.current === identity && !identity.controller.signal.aborted) setBusy(false);
    }
  }, [projectId, search, label, availability]);

  useEffect(() => {
    const identity = { controller: new AbortController(), busy: false };
    owner.current = identity; current.current = null; autoPages.current = 0; autoLaunch.current = null; setResult(null); setError(null); setBusy(false);
    if (projectId) void load(identity, false);
    return () => { identity.controller.abort(); if (owner.current === identity) owner.current = null; };
  }, [load, projectId, refresh]);

  // Each delivered page decides whether the next one is read automatically;
  // an error or a dropped generation ends it, and so does the page cap.
  useEffect(() => {
    const page = result?.page;
    if (error || !page?.more || !page.after || page.items.length >= WORK_AUTO_LOAD_TARGET_ROWS || autoPages.current >= WORK_AUTO_LOAD_MAX_PAGES) return;
    const launched = autoLaunch.current;
    if (launched && (page.after === launched.cursor || page.items.length <= launched.rows)) return;
    const identity = owner.current;
    if (!identity || identity.busy) return;
    autoPages.current += 1;
    autoLaunch.current = { cursor: page.after, rows: page.items.length };
    void load(identity, true);
  }, [result, error, load]);

  return { result, error, busy, reload: () => setRefresh(v => v + 1), more: () => { if (owner.current) void load(owner.current, true); } };
}
