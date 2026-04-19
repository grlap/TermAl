// Empty-state placeholder card used by the top-level shell when
// there is nothing to render yet (no project selected, no session
// open, backend still loading, etc.).
//
// What this file owns:
//   - The `<article class="empty-state">` wrapper with its
//     `"Live State"` card label and a `<h3>` title + `<p>` body.
//
// What this file does NOT own:
//   - The different empty-state cards rendered by the individual
//     panels (`FileSystemPanel`, `GitStatusPanel`, `SourcePanel`)
//     which intentionally use a different class
//     (`empty-state-card`) and a `"Workspace"` card label. Those
//     stay co-located with their panels.
//   - Deciding when to show an empty state — the caller renders
//     `<EmptyState>` only in the specific contexts where nothing
//     else applies.
//
// Split out of `ui/src/App.tsx`. Same JSX, same className, same
// copy as the inline definition it replaced.

export function EmptyState({
  title,
  body,
}: {
  title: string;
  body: string;
}) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}
