// Shared sentinel identifier used by the project-filter UI.
//
// What this file owns:
//   - `ALL_PROJECTS_FILTER_ID` — the `"__all__"` sentinel that the
//     project-list / session-list filter UI uses to mean "show
//     every project / session, not just those scoped to a single
//     project". Lives in its own module so the top-level App
//     shell (which reads it when filtering the session list) and
//     the `ProjectListSection` component (which writes it when the
//     user clicks the "All projects" row) can agree on the value
//     without creating a circular import between `App.tsx` and
//     `./ProjectListSection.tsx`.
//
// What this file does NOT own:
//   - The project-filter state itself — that's a `useState` in
//     `App.tsx`.
//   - The filter UI — `ProjectListSection.tsx` renders it.
//   - Any other sentinel ids, e.g., `CREATE_SESSION_WORKSPACE_ID`
//     which stays in `App.tsx` because only App uses it.
//
// Split out of `ui/src/App.tsx` to break the circular dependency
// that would otherwise exist between App.tsx and a future
// `ProjectListSection.tsx` module.

export const ALL_PROJECTS_FILTER_ID = "__all__";
