// Owns the browser-tab title derived from the last active session tab.
// Deliberately does not own workspace routing or session selection.

import { useEffect, useRef } from "react";

export const DEFAULT_BROWSER_TITLE = "TermAl";

export function formatSessionBrowserTitle(sessionName: string): string {
  return `${sessionName} · ${DEFAULT_BROWSER_TITLE}`;
}

export function useLastActiveSessionDocumentTitle(
  activeSessionName: string | null | undefined,
) {
  const lastActiveSessionNameRef = useRef<string | null>(null);

  useEffect(() => {
    const normalizedSessionName = activeSessionName?.trim() ?? "";
    if (normalizedSessionName) {
      lastActiveSessionNameRef.current = normalizedSessionName;
    }

    document.title = lastActiveSessionNameRef.current
      ? formatSessionBrowserTitle(lastActiveSessionNameRef.current)
      : DEFAULT_BROWSER_TITLE;
  }, [activeSessionName]);

  useEffect(
    () => () => {
      document.title = DEFAULT_BROWSER_TITLE;
    },
    [],
  );
}
