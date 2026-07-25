// Test hooks the suite installs into the running app to observe rare
// code paths and coordinate exact asynchronous commit boundaries.
//
// What this file owns:
//   - `AppTestHooks` — the hook shape. New observer fields must use
//     non-sensitive label arguments (string literal unions, etc.).
//     Coordination fields may only pause a transition; they must not
//     accept or replace user data.
//   - `appTestHooks` — the currently-installed hook object, or
//     `null` when tests aren't in control. Exported as a `let`
//     binding so all production call sites see the latest value
//     through ES module live-binding semantics. Reads stay plain
//     property accesses (`appTestHooks?.onFoo?.(...)`) with no
//     function-call overhead.
//   - `setAppTestHooksForTests` — the only way to mutate the
//     binding. Tests call this in `beforeEach` / `afterEach`; in
//     production it is never called.
//
// What this file does NOT own:
//   - The call sites that fire hooks — those live beside the production
//     transitions they observe or coordinate (currently: persisted-file
//     refresh success / error, delete-project post-await resolve /
//     reject, and the workspace-layout commit boundary). Those sites
//     read `appTestHooks` directly and no-op when it's `null`.
//
// Split out of `ui/src/App.tsx`. Same types, same runtime, same
// behaviour; the module-scoped `appTestHooks` binding moved from
// App.tsx's top scope to this file and is now exported as a live
// binding.

export type AppTestHooks = {
  beforeWorkspaceLayoutLoadCommit?: () => Promise<void>;
  onDeleteProjectPostAwaitPath?: (path: "resolve" | "reject") => void;
  onRestoredGitDiffDocumentContentUpdate?: (
    status: "success" | "error",
  ) => void;
};

// eslint-disable-next-line import/no-mutable-exports
export let appTestHooks: AppTestHooks | null = null;

// Keep observer fields non-sensitive and coordination fields data-free so the
// production export cannot expose or replace user content if imported.
export function setAppTestHooksForTests(hooks: AppTestHooks | null) {
  if (import.meta.env.MODE !== "test") {
    throw new Error("App test hooks can only be installed in test mode.");
  }
  appTestHooks = hooks;
}
