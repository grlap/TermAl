// Owns the opt-in context that lets message metadata expose marker-menu triggers.
// Does not own message rendering, marker grouping, or marker menu positioning.
// Split from ui/src/message-cards.tsx.

import { createContext, useContext, type ReactNode } from "react";

const MessageMetaMarkerMenuContext = createContext(false);

export function useIsMessageMetaMarkerMenuTriggerEnabled() {
  return useContext(MessageMetaMarkerMenuContext);
}

export function MessageMetaMarkerMenuProvider({
  children,
}: {
  children: ReactNode;
}) {
  // Binary opt-in: only the conversation panel wraps message cards that should
  // expose the marker-menu affordance on their metadata author label.
  return (
    <MessageMetaMarkerMenuContext.Provider value={true}>
      {children}
    </MessageMetaMarkerMenuContext.Provider>
  );
}
