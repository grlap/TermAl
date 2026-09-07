// Owns the browser-tab title derived from the workspace label and last active session tab.
// Deliberately does not own workspace routing or session selection.

import { useEffect, useRef } from "react";

export const DEFAULT_BROWSER_TITLE = "TermAl";

export function formatSessionBrowserTitle(sessionName: string): string {
  return `${sessionName} · ${DEFAULT_BROWSER_TITLE}`;
}

export function useLastActiveSessionDocumentTitle(
  activeSessionName: string | null | undefined,
  workspaceLabel?: string | null,
) {
  const lastActiveSessionNameRef = useRef<string | null>(null);

  useEffect(() => {
    const normalizedSessionName = activeSessionName?.trim() ?? "";
    if (normalizedSessionName) {
      lastActiveSessionNameRef.current = normalizedSessionName;
    }

    const sessionTitle = lastActiveSessionNameRef.current
      ? formatSessionBrowserTitle(lastActiveSessionNameRef.current)
      : DEFAULT_BROWSER_TITLE;
    document.title = workspaceLabel?.trim()
      ? `${workspaceLabel.trim()} · ${sessionTitle}`
      : sessionTitle;
  }, [activeSessionName, workspaceLabel]);

  useEffect(
    () => () => {
      document.title = DEFAULT_BROWSER_TITLE;
    },
    [],
  );
}
