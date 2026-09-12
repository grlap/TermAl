// Owns the permanently reserved trailing activity line and its status label.
// New feature mounted by SessionPaneView, not a split of an existing module.
// Does not own cards, stream tracking, announcements, or scroll writes.
import { resolveSessionActivity, type SessionActivityOptions } from "./panels/AgentSessionPanel.waiting-indicator";
import { isSessionAtLiveTail } from "./session-live-tail";
import type { Session } from "./types";
import "./transcript-activity-slot.css";

type ActivityInputs = Omit<SessionActivityOptions, "session">;

export function transcriptActivityLabel(session: Session | null, inputs: ActivityInputs = {}): string | null {
  if (!session || !isSessionAtLiveTail(session)) return null;
  switch (resolveSessionActivity({ session, ...inputs }).state) {
    case "working":
    case "sending":
    case "queued":
    case "waiting":
      return "Agent is working";
    case "stopping":
      return "Agent is stopping";
    default:
      return null;
  }
}

export function TranscriptActivitySlot({ session, ...inputs }: { session: Session | null } & ActivityInputs) {
  const label = transcriptActivityLabel(session, inputs);
  return (
    <div
      className="transcript-activity-slot"
      data-waiting={String(label !== null)}
      // The activity strip owns announcements. This duplicate is visual only.
      aria-hidden="true"
    >
      {label ? (
        <span className="message-activity-status" data-activity="running">
          <span className="message-activity-label">{label}</span>
          <span className="message-activity-squares" aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
        </span>
      ) : null}
    </div>
  );
}
