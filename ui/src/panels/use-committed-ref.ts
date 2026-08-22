// Owns refs that publish render-derived values only after React commits them.
// Does not own domain state or event-handler identity; use useStableEvent for callbacks.
// Extracted from transcript viewport coordination paths that formerly wrote refs in render.
import { useInsertionEffect, useRef } from "react";

export function useCommittedRef<T>(value: T) {
  const valueRef = useRef(value);
  // Some consumers are read by descendant layout measurement. Publish in the
  // earliest commit-safe phase so those measurements see this commit without
  // exposing values from a render React later abandons.
  useInsertionEffect(() => {
    valueRef.current = value;
  }, [value]);
  return valueRef;
}
