/*
 * BoardPanel — read-only viewer for the per-project coordination board
 * (tm-uwx.7.3; docs/features/agent-boards.md).
 *
 * Owns: fetching the COMPLETE board through one caller session
 * (generation-bound pagination with bounded conflict restarts — no silent
 * truncation above one page), the knownGeneration unchanged short-circuit,
 * stale-response suppression across sessionId changes, and rendering entries
 * (key, pretty-printed JSON value, revision, author, timestamp, stateStamp).
 * Deliberately does NOT own: any write path (Greg's v1 ruling — agents write
 * via MCP/HTTP, humans observe), scope/authorization logic (backend-owned),
 * or the mount point (the host passes a sessionId and decides where the
 * panel lives). New file for the board feature; follows the panels/
 * single-purpose module convention.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiRequestError, fetchCoordinationBoard } from "../api";
import type { BoardEntry } from "../types";

type BoardPanelProps = {
  /** A local root session in the project whose board should be shown. */
  sessionId: string;
};

/** Bounded restarts when a write lands between pages (409 snapshot conflict). */
const BOARD_PAGINATION_RESTART_LIMIT = 5;

function formatBoardValue(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? "null";
  } catch {
    return String(value);
  }
}

function isSnapshotConflict(error: unknown): boolean {
  return error instanceof ApiRequestError && error.status === 409;
}

/**
 * Fetches every page of the board under one generation snapshot. Returns
 * `null` when the board is unchanged versus `knownGeneration` (the caller
 * keeps its rendered entries). Restarts the whole listing on a 409 snapshot
 * conflict, at most `BOARD_PAGINATION_RESTART_LIMIT` times.
 */
async function fetchWholeBoard(
  sessionId: string,
  knownGeneration: number | null,
): Promise<{ entries: BoardEntry[]; generation: number } | null> {
  let restarts = 0;
  for (;;) {
    const first = await fetchCoordinationBoard(sessionId, {
      knownGeneration: knownGeneration ?? undefined,
    });
    if (first.unchanged) {
      return null;
    }
    const entries = [...first.entries];
    let afterKey = first.nextAfterKey ?? null;
    let restarted = false;
    while (afterKey !== null) {
      let page;
      try {
        page = await fetchCoordinationBoard(sessionId, {
          afterKey,
          snapshotGeneration: first.generation,
        });
      } catch (error) {
        if (isSnapshotConflict(error)) {
          if (restarts < BOARD_PAGINATION_RESTART_LIMIT) {
            restarts += 1;
            restarted = true;
            break;
          }
          throw new Error(
            "The board kept changing while it was read. Refresh again when writes settle.",
          );
        }
        throw error;
      }
      entries.push(...page.entries);
      afterKey = page.nextAfterKey ?? null;
    }
    if (restarted) {
      continue;
    }
    return { entries, generation: first.generation };
  }
}

export function BoardPanel({ sessionId }: BoardPanelProps): JSX.Element {
  const [entries, setEntries] = useState<BoardEntry[]>([]);
  const [generation, setGeneration] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // The latest generation we fully rendered; passed back as knownGeneration
  // so an unchanged board costs one cheap short-circuit round trip.
  const renderedGeneration = useRef<number | null>(null);
  // State updates from the session-change effect run after React's first
  // render for the new prop. Keep the owner alongside the rendered payload so
  // that first render cannot display the previous project's facts under the
  // new project heading.
  const renderedSessionId = useRef<string | null>(null);
  // Monotonic token: only the newest in-flight refresh may apply state, so a
  // slow response for a previous sessionId can never overwrite the current
  // project's board.
  const requestToken = useRef(0);

  const refresh = useCallback(async () => {
    const token = requestToken.current + 1;
    requestToken.current = token;
    setLoading(true);
    setError(null);
    try {
      const board = await fetchWholeBoard(sessionId, renderedGeneration.current);
      if (token !== requestToken.current) {
        return;
      }
      if (board !== null) {
        renderedGeneration.current = board.generation;
        renderedSessionId.current = sessionId;
        setEntries(board.entries);
        setGeneration(board.generation);
      } else {
        // Unchanged short-circuit: keep the rendered entries.
        renderedSessionId.current = sessionId;
        setGeneration(renderedGeneration.current);
      }
    } catch (fetchError) {
      if (token !== requestToken.current) {
        return;
      }
      renderedSessionId.current = sessionId;
      setError(
        fetchError instanceof Error ? fetchError.message : String(fetchError),
      );
    } finally {
      if (token === requestToken.current) {
        setLoading(false);
      }
    }
  }, [sessionId]);

  useEffect(() => {
    renderedGeneration.current = null;
    setEntries([]);
    setGeneration(null);
    void refresh();
    return () => {
      // Invalidate any in-flight refresh on prop change or unmount so a
      // promise resolving inside the cleanup window can never apply state
      // for a stale session (review, mailbox #238).
      requestToken.current += 1;
    };
  }, [refresh]);

  const scopeMatches = renderedSessionId.current === sessionId;
  const visibleEntries = scopeMatches ? entries : [];
  const visibleGeneration = scopeMatches ? generation : null;
  const visibleError = scopeMatches ? error : null;
  const visibleLoading = loading || !scopeMatches;

  return (
    <div className="board-panel">
      <div className="board-panel-header">
        <span className="board-panel-title">Coordination board</span>
        {visibleGeneration !== null ? (
          <span
            className="board-panel-generation"
            title="Scope generation: bumps on every successful write"
            aria-label={`Scope generation ${visibleGeneration}; bumps on every successful write`}
          >
            gen {visibleGeneration}
          </span>
        ) : null}
        <span
          className="board-panel-refresh-mode"
          title="The board does not auto-refresh"
          aria-label="Manual refresh; the board does not auto-refresh"
        >
          manual refresh
        </span>
        <button
          type="button"
          className="board-panel-refresh"
          onClick={() => void refresh()}
          disabled={visibleLoading}
        >
          {visibleLoading ? "Refreshing…" : "Refresh"}
        </button>
      </div>
      {visibleError !== null ? (
        <div className="board-panel-error" role="alert">
          {visibleError}
        </div>
      ) : null}
      {visibleLoading &&
      visibleEntries.length === 0 &&
      visibleError === null ? (
        <div className="board-panel-loading">Loading board…</div>
      ) : null}
      {!visibleLoading &&
      visibleEntries.length === 0 &&
      visibleError === null ? (
        <div className="board-panel-empty">
          No board entries. Agents publish durable facts here with
          termal_board_set; entries never trigger wake-ups.
        </div>
      ) : null}
      {visibleEntries.length > 0 ? (
        <ul className="board-panel-entries">
          {visibleEntries.map((entry) => (
            <li key={entry.key} className="board-panel-entry">
              <div className="board-panel-entry-head">
                <code className="board-panel-entry-key">{entry.key}</code>
                <span
                  className="board-panel-entry-revision"
                  title="Per-key revision: the CAS token agents pass as expectedRevision"
                  aria-label={`Per-key revision ${entry.revision}; CAS token for expectedRevision`}
                >
                  r{entry.revision}
                </span>
              </div>
              <pre className="board-panel-entry-value">
                {formatBoardValue(entry.value)}
              </pre>
              <div className="board-panel-entry-meta">
                <span className="board-panel-entry-author">
                  {entry.authorName}
                </span>
                <span className="board-panel-entry-updated">
                  {entry.updatedAt}
                </span>
                {entry.stateStamp ? (
                  <span className="board-panel-entry-stamp">
                    {entry.stateStamp}
                  </span>
                ) : null}
              </div>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
