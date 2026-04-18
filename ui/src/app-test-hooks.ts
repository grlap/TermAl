// Observer-only hooks the test suite installs into the running app
// to peek at rare code paths without changing production behaviour.
//
// What this file owns:
//   - `AppTestHooks` — the hook shape. New hook fields must use
//     non-sensitive label arguments (string literal unions, etc.)
//     so a production build that accidentally imported this
//     module could not exfiltrate user content.
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
//   - The call sites that fire hooks — those live in `App.tsx`
//     (currently: persisted-file refresh success / error,
//     delete-project post-await resolve / reject). Those sites
//     read `appTestHooks` directly and no-op when it's `null`.
//
// Split out of `ui/src/App.tsx`. Same types, same runtime, same
// behaviour; the module-scoped `appTestHooks` binding moved from
// App.tsx's top scope to this file and is now exported as a live
// binding.

export type AppTestHooks = {
  onDeleteProjectPostAwaitPath?: (path: "resolve" | "reject") => void;
  onRestoredGitDiffDocumentContentUpdate?: (
    status: "success" | "error",
  ) => void;
};

// eslint-disable-next-line import/no-mutable-exports
export let appTestHooks: AppTestHooks | null = null;

// Keep App test hooks observer-only. Hook fields must use non-sensitive label
// arguments so the production export cannot expose user content if imported.
export function setAppTestHooksForTests(hooks: AppTestHooks | null) {
  appTestHooks = hooks;
}
