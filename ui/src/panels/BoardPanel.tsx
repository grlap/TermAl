/*
 * BoardPanel — read-only renderer for the per-project coordination board
 * (tm-uwx.7.3; docs/features/agent-boards.md).
 *
 * Owns: presentation of board facts (key, pretty-printed JSON value, revision,
 * author, relative timestamp, stateStamp), freshness status, change badges,
 * and manual Refresh controls. The polling/claim lifecycle lives in
 * board-panel-polling.ts. Deliberately does NOT own: any write path (Greg's
 * v1 ruling — agents write via MCP/HTTP, humans observe), scope/authorization
 * logic (backend-owned), or the mount point (the host passes a sessionId and
 * decides where the panel lives). New file for the board feature; follows the
 * panels/ single-purpose module convention.
 */

import { type BoardPollingState, useBoardPolling } from "./board-panel-polling";

type BoardPanelProps = {
  /** A local root session in the project whose board should be shown. */
  sessionId: string;
};

function formatBoardValue(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? "null";
  } catch {
    return String(value);
  }
}

/**
 * Compact relative age for board facts ("just now", "3m ago", "2h ago",
 * else a local date). Recomputed on every render. The polling hook guarantees
 * a render at poll cadence, including completed, skipped, and coalesced ticks.
 */
export function formatBoardRelativeTime(
  updatedAt: string,
  now: number = Date.now(),
): string {
  const timestamp = Date.parse(updatedAt);
  if (Number.isNaN(timestamp)) {
    return updatedAt;
  }
  const elapsedMs = now - timestamp;
  if (elapsedMs < 60_000) {
    return "just now";
  }
  if (elapsedMs < 3_600_000) {
    return `${Math.floor(elapsedMs / 60_000)}m ago`;
  }
  if (elapsedMs < 86_400_000) {
    return `${Math.floor(elapsedMs / 3_600_000)}h ago`;
  }
  return new Date(timestamp).toLocaleDateString();
}

function newestVisibleWrite({
  visibleEntries,
}: Pick<BoardPollingState, "visibleEntries">): string | null {
  // String-max is safe ONLY because the backend emits fixed-width RFC 3339
  // UTC millis (coordination_board.rs, to_rfc3339_opts(Millis, true)); under
  // offsets or variable precision this comparison would silently break.
  return visibleEntries.reduce<string | null>(
    (newest, entry) =>
      newest === null || entry.updatedAt > newest ? entry.updatedAt : newest,
    null,
  );
}

export function BoardPanel({ sessionId }: BoardPanelProps): JSX.Element {
  const polling = useBoardPolling(sessionId);
  const {
    visibleEntries,
    visibleGeneration,
    visibleError,
    visibleBackgroundError,
    visibleLoading,
    requestActive,
    highlightAboveGeneration,
    visibleDeletionObservedAt,
    refresh,
  } = polling;
  const changedEntryCount =
    highlightAboveGeneration === null
      ? 0
      : visibleEntries.filter(
          (entry) => entry.updatedAtGeneration > highlightAboveGeneration,
        ).length;
  const newestUpdatedAt = newestVisibleWrite(polling);
  const latestActivityAt = visibleDeletionObservedAt ?? newestUpdatedAt;
  const latestActivityIsDeletionObservation =
    visibleDeletionObservedAt !== null;
  const liveAnnouncement =
    changedEntryCount > 0
      ? `${changedEntryCount} board ${
          changedEntryCount === 1 ? "entry" : "entries"
        } updated at scope generation ${visibleGeneration ?? "unknown"}`
      : visibleDeletionObservedAt !== null
        ? `Board entries removed at scope generation ${visibleGeneration ?? "unknown"}`
        : "";

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
        {latestActivityAt !== null ? (
          <span
            className="board-panel-last-write"
            title={
              latestActivityIsDeletionObservation
                ? `Deletion-only board generation observed at ${latestActivityAt}; deleted rows do not expose a write timestamp`
                : `Newest visible fact written at ${latestActivityAt}`
            }
          >
            {latestActivityIsDeletionObservation ? "last change" : "last write"}{" "}
            {formatBoardRelativeTime(latestActivityAt)}
          </span>
        ) : null}
        <span
          className={`board-panel-refresh-mode${
            visibleBackgroundError === null
              ? ""
              : " board-panel-refresh-mode-stale"
          }`}
          role="status"
          aria-label={
            visibleBackgroundError === null
              ? "Live; the board re-checks automatically every few seconds"
              : "Stale; live board polling failed and will retry automatically"
          }
          title={
            visibleBackgroundError === null
              ? "While this panel is visible it re-checks the board on a cheap unchanged probe every few seconds"
              : "Live polling is temporarily stale. Retrying automatically; use Refresh to retry now."
          }
        >
          {visibleBackgroundError === null ? "live" : "stale"}
        </span>
        <button
          type="button"
          className="board-panel-refresh"
          onClick={() => void refresh()}
          disabled={visibleLoading || requestActive}
        >
          {visibleLoading || requestActive ? "Refreshing…" : "Refresh"}
        </button>
      </div>
      {/* Polite announcement for assistive tech when facts move — visual
          badges and disappearing rows alone carry no AT signal (tm-kfy). */}
      <span className="visually-hidden" role="status" aria-live="polite">
        {liveAnnouncement}
      </span>
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
          {visibleGeneration !== null && visibleGeneration > 0 ? (
            <>
              No live entries right now — this project&apos;s board has seen{" "}
              {visibleGeneration} write{visibleGeneration === 1 ? "" : "s"}{" "}
              (deleted keys stay hidden). New facts agents publish appear here
              automatically.
            </>
          ) : (
            <>
              Nothing published yet. Agents publish durable facts here with
              termal_board_set; entries never trigger wake-ups, and this panel
              picks them up live.
            </>
          )}
        </div>
      ) : null}
      {visibleEntries.length > 0 ? (
        <ul className="board-panel-entries">
          {visibleEntries.map((entry) => {
            const changed =
              highlightAboveGeneration !== null &&
              entry.updatedAtGeneration > highlightAboveGeneration;
            return (
              <li
                key={entry.key}
                className={`board-panel-entry${changed ? " board-panel-entry-changed" : ""}`}
              >
                <div className="board-panel-entry-head">
                  <code className="board-panel-entry-key">{entry.key}</code>
                  {changed ? (
                    <span className="board-panel-entry-changed-badge">
                      updated
                    </span>
                  ) : null}
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
                  <span
                    className="board-panel-entry-updated"
                    title={entry.updatedAt}
                  >
                    {formatBoardRelativeTime(entry.updatedAt)}
                  </span>
                  {entry.stateStamp ? (
                    <span className="board-panel-entry-stamp">
                      {entry.stateStamp}
                    </span>
                  ) : null}
                </div>
              </li>
            );
          })}
        </ul>
      ) : null}
    </div>
  );
}
