// Owns presentation of canonical control-recovery summaries in settings.
// Does not retain debt, settle operations, or send recovery requests.
// New module shared by the project dialog and global Engram preferences.
import { createContext, useContext } from "react";
import type { EngramControlRecoverySummary } from "./api";
import type { Project, Session } from "./types";

type RecoveryView = {
  entries: readonly EngramControlRecoverySummary[];
  projects: readonly Project[];
  sessions: readonly Session[];
  onInspect: (sessionId: string) => void;
  onStop: (sessionId: string) => void;
  stopping: Readonly<Record<string, boolean | undefined>>;
};

export const EngramControlRecoveryContext = createContext<RecoveryView | null>(null);

const reasons = new Map([
  ["existing_checkpoint_unconfirmed", "The original turn checkpoint has not been confirmed."],
  ["original_authority_unconfirmed", "The original control authority has not been confirmed."],
  ["provider_interruption_unconfirmed", "Provider interruption has not been confirmed."],
  ["control_settlement_unconfirmed", "Settlement of the original control operation has not been confirmed."],
  ["checkpoint_receipt_capacity_unavailable", "The checkpoint receipt could not yet be retained."],
]);

export function EngramControlRecoveryPanel({ projectId, onInspect }: {
  projectId?: string;
  onInspect?: () => void;
}) {
  const view = useContext(EngramControlRecoveryContext);
  const entries = view?.entries.filter(entry =>
    !entry.settlementConfirmed && (projectId === undefined || entry.projectId === projectId));
  if (!view || !entries?.length) return null;

  return (
    <section className="message-card prompt-settings-card" aria-label="Engram control recovery">
      <h3>Engram control recovery</h3>
      <p className="settings-panel-copy">
        These original operations remain unresolved, including after a session is removed.
        Disabling control does not settle them. Only confirmed settlement updates this list.
      </p>
      {entries.map((entry, index) => {
        const session = view.sessions.find(candidate =>
          candidate.id === entry.sessionId && candidate.projectId === entry.projectId);
        const project = view.projects.find(candidate => candidate.id === entry.projectId);
        const canInspect = session && entry.nextActions.includes("inspect_session");
        const canStop = session && !session.remoteId &&
          (session.status === "active" || session.status === "approval") &&
          entry.nextActions.includes("stop_session_if_running");
        return (
          <article key={`${entry.projectId}:${entry.sessionId}:${entry.operation}:${index}`}>
            <h4>{project?.name ?? entry.projectId} · {session?.name ?? "Removed session"}</h4>
            <dl>
              <dt>Original session</dt><dd>{entry.sessionId}</dd>
              <dt>Unresolved operation</dt>
              <dd>{entry.operation === "turn_checkpoint" ? "Turn checkpoint"
                : entry.operation === "session_bind_retirement" ? "Session binding retirement"
                : "Control operation"}</dd>
              <dt>Reason</dt><dd>{reasons.get(entry.code) ?? "The original operation's outcome remains unconfirmed."}</dd>
            </dl>
            <p>{entry.excluded ? "The original control identity remains excluded." : "Settlement remains pending."}</p>
            {!session ? (
              <p>The original session is no longer available. No session action can settle this debt.
                Recovery requires confirmation of the original operation through Engram.</p>
            ) : (
              <p>The existing session retains this unresolved operation. Stopping a running provider
                does not confirm settlement of an unknown operation.</p>
            )}
            {canInspect ? <button type="button" className="ghost-button"
              aria-label={`Inspect session ${entry.sessionId}`}
              onClick={() => { onInspect?.(); view.onInspect(entry.sessionId); }}>Inspect session</button> : null}
            {canStop ? <button type="button" className="danger-button"
              aria-label={`Stop running session ${entry.sessionId}`}
              disabled={view.stopping[entry.sessionId] === true}
              onClick={() => view.onStop(entry.sessionId)}>Stop running session</button> : null}
          </article>
        );
      })}
    </section>
  );
}
