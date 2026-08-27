// Owns: the explicit pending flag and retry timer for a page-measurement
// bottom restoration deferred past direct user scroll input.
// Does not own: deciding whether bottom authority exists or writing scrollTop.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.

import { useCallback, useEffect, useRef } from "react";

export function useVirtualizedConversationDeferredBottomRestore({
  bumpLayoutVersion,
}: {
  bumpLayoutVersion: () => void;
}) {
  const timerRef = useRef<number | null>(null);
  const pendingRef = useRef(false);

  const clear = useCallback(() => {
    pendingRef.current = false;
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const isPending = useCallback(() => pendingRef.current, []);

  const scheduleLayoutVersion = useCallback(
    (delayMs: number) => {
      pendingRef.current = true;
      if (timerRef.current !== null) {
        window.clearTimeout(timerRef.current);
      }
      timerRef.current = window.setTimeout(() => {
        timerRef.current = null;
        bumpLayoutVersion();
      }, Math.max(Math.ceil(delayMs), 0));
    },
    [bumpLayoutVersion],
  );

  useEffect(() => clear, [clear]);

  return {
    clear,
    isPending,
    scheduleLayoutVersion,
  };
}
