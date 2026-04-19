// Tiny React hook that keeps an event-handler identity stable
// across renders while still calling the latest closure. Some
// callers dispatch the stable wrapper from `flushSync`-driven
// event paths, which means the ref publishing must run before the
// layout phase — `useLayoutEffect` satisfies that ordering.
//
// What this file owns:
//   - `useStableEvent<TArgs, TResult>(callback)` — returns a
//     `useCallback`-backed wrapper whose identity never changes
//     but which always dispatches to the freshest `callback`
//     value (stored in a ref that `useLayoutEffect` refreshes on
//     each render).
//
// What this file does NOT own:
//   - Any domain-specific handler. Callers pass their own
//     callback and receive back a memoised wrapper.
//   - React 19 `useEvent` — still unshipped; this hook exists to
//     fill that gap with the `useLayoutEffect` ordering the diff
//     panel relies on.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same shape, same
// publish-before-layout guarantee, same generic signature.

import { useCallback, useLayoutEffect, useRef } from "react";

export function useStableEvent<TArgs extends unknown[], TResult>(
  callback: (...args: TArgs) => TResult,
) {
  const callbackRef = useRef(callback);
  // Some callers invoke the stable wrapper from flushSync-driven event paths,
  // so publish the latest callback before layout-phase work can run.
  useLayoutEffect(() => {
    callbackRef.current = callback;
  });

  return useCallback((...args: TArgs) => callbackRef.current(...args), []);
}
