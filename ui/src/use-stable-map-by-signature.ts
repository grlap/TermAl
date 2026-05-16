// Owns: signature-stable Map identity for memo-sensitive render paths.
// Does not own: deciding what entries belong in the map or how callers use it.
// Split from: ui/src/SessionPaneView.tsx connection retry display-state cache.

import { useEffect, useMemo, useRef } from "react";

function encodeSignaturePart(value: string): string {
  // Length-prefixing keeps delimiter-bearing keys/values collision-free.
  return `${value.length}:${value}`;
}

/**
 * Reuses the previous map identity while the ordered string-entry signature is
 * unchanged. This hook is intentionally scoped to string-valued maps so the
 * length-prefixed signature is collision-free without caller-provided encoders.
 * Signature construction is O(entries), so keep it out of per-keystroke paths.
 *
 * Callers must treat returned maps as immutable snapshots; this is a single-slot
 * cache, so A -> B -> A returns the later A map, not the first one.
 */
export function useStableMapBySignature<Value extends string>(
  nextMap: ReadonlyMap<string, Value>,
): ReadonlyMap<string, Value> {
  const signature = useMemo(() => {
    if (nextMap.size === 0) {
      return "";
    }

    return [...nextMap]
      .map(
        ([key, value]) =>
          `${encodeSignaturePart(key)}${encodeSignaturePart(value)}`,
      )
      .join("");
  }, [nextMap]);
  const stableMapRef = useRef<{
    signature: string;
    map: ReadonlyMap<string, Value>;
  } | null>(null);

  // The ref is written only after commit below. During render, a stale ref can
  // only lose identity reuse for one render; it cannot change map contents.
  const stableMap = useMemo(() => {
    const previous = stableMapRef.current;
    if (previous?.signature === signature) {
      return previous.map;
    }

    return nextMap;
  }, [signature, nextMap]);

  useEffect(() => {
    stableMapRef.current = { signature, map: stableMap };
  }, [signature, stableMap]);

  return stableMap;
}
