// Compact page-chain status, split from WorkPanel's results. Does not fetch,
// infer completion from counts, or restart a failed cursor.
import type { WorkPage } from "../work-visualizer-api";

export function WorkPaging({ page, busy, error }: { page: WorkPage; busy: boolean; error: string | null }) {
  return <div className="work-paging" aria-live="polite">
    {busy ? "Loading more work…" : error ? "Page loading paused." : !page.more ? "End of work list." : !page.after
      ? "More work remains, but this byte-limited page has no continuation. Refresh to try a new list."
      : "Loading work…"}
    {page.hint && <p>{page.hint}</p>}
  </div>;
}
