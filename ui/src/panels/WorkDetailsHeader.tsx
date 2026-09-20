// Shared Work drawer heading, split from WorkItemDetails and WorkBeadDetails.
// Owns compact, accessible controls; fetching and close/focus behavior stay in callers.
export function WorkDetailsHeader({ workRef, onReload, onClose }: {
  workRef: string; onReload: () => void; onClose: () => void;
}) {
  return <div className="work-details-header">
    <strong>{workRef}</strong>
    <button type="button" className="work-details-icon" aria-label="Reload details" title="Reload details" onClick={onReload}>
      <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <path d="M12.2 5.9A5 5 0 1 0 13 8M10.1 3.9h2.7v2.7" />
      </svg>
    </button>
    <button type="button" className="work-details-icon" aria-label="Close details" title="Close details" onClick={onClose}>
      <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
        <path d="m4 4 8 8M12 4l-8 8" />
      </svg>
    </button>
  </div>;
}
