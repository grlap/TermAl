// Owns the explicit recovery action for a session retained after Delete.
// Uses the existing Resume callback; recovery authority belongs to the server.
// Split from session-activity-cards.tsx while isolating the recovery control.
export function SourceTrackingRecoveryIndicator({
  explanation,
  onRecover,
}: {
  explanation: string;
  onRecover: () => void;
}) {
  return (
    <article
      className="activity-card activity-card-queue-paused"
      role="status"
      aria-live="polite"
    >
      <div className="activity-pause-glyph" aria-hidden="true" />
      <div className="activity-card-copy">
        <div className="card-label">Source tracking recovery</div>
        <p>{explanation}</p>
        <p>
          Recovery keeps queued prompts paused. When tracking is recovered, retry
          Delete.
        </p>
      </div>
      <button
        className="queue-resume-button"
        type="button"
        onClick={onRecover}
      >
        Recover tracking
      </button>
    </article>
  );
}
