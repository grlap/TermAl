// Owns status text only. Callers supply per-operation state;
// this component does not infer activity from session state or touch scrolling.
export function MessageActivityStatus({
  state,
  label,
}: {
  state: "running" | "waiting" | "inactive";
  label: string;
}) {
  return (
    <span className="message-activity-status" data-activity={state}>
      <span className="message-activity-label">{label}</span>
    </span>
  );
}

export function RequestActivityStatus({
  pending,
  enabled = true,
  submitting = false,
  resolvedLabel,
}: {
  pending: boolean;
  enabled?: boolean;
  submitting?: boolean;
  resolvedLabel: string;
}) {
  return (
    <MessageActivityStatus
      state={
        !pending ? "inactive" : submitting ? "running" : enabled ? "waiting" : "inactive"
      }
      label={
        !pending ? resolvedLabel : submitting ? "Sending" : enabled ? "Waiting for you" : "Queued"
      }
    />
  );
}
