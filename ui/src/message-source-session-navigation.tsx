// Owns the message-header adapter to the pane's existing session navigation.
// Does not own session state, discovery, or workspace/tab selection.
// MessageMeta uses this for mailbox source names; the pane supplies its lookup.

import { createContext, useContext, type ReactNode } from "react";
import type { Session } from "./types";
import "./message-source-session-navigation.css";

export const MessageSourceSessionNavigationContext = createContext<
  {
    onOpenSession: (sessionId: string) => void;
    sessionLookup: ReadonlyMap<string, Session>;
  } | null
>(null);

export function MessageSourceSessionName({
  sessionId,
  name,
  className,
  markerMenuTrigger,
  fallback,
}: {
  sessionId: string;
  name: string;
  className: string;
  markerMenuTrigger: boolean;
  fallback: ReactNode;
}) {
  const navigation = useContext(MessageSourceSessionNavigationContext);
  if (!navigation?.sessionLookup.has(sessionId)) {
    return fallback;
  }

  return (
    <button
      className={`${className} message-meta-session-link`}
      type="button"
      aria-label={`Open session ${name}`}
      aria-haspopup={markerMenuTrigger ? "menu" : undefined}
      aria-description={markerMenuTrigger
        ? "Right-click or press Shift+F10 for conversation marker actions."
        : undefined}
      title={`Open session ${name}${markerMenuTrigger ? "; right-click for marker actions" : ""}`}
      data-conversation-marker-menu-trigger={markerMenuTrigger || undefined}
      onClick={(event) => {
        event.stopPropagation();
        navigation.onOpenSession(sessionId);
      }}
      onKeyDown={(event) => {
        // Enter/Space activate navigation, while ContextMenu remains available
        // to the existing transcript marker-menu handler.
        if (event.key === "Enter" || event.key === " ") {
          event.stopPropagation();
        }
      }}
    >
      {name}
    </button>
  );
}
