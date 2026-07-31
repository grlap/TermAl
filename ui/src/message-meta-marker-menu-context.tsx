// Owns the opt-in context that lets message metadata expose marker-menu triggers.
// Does not own message rendering, marker grouping, or marker menu positioning.
// Split from ui/src/message-cards.tsx.

import {
  createContext,
  useContext,
  type DragEvent as ReactDragEvent,
  type ReactNode,
} from "react";

const MessageMetaMarkerMenuContext = createContext(false);
const MessageMetaResponseBoardDragContext = createContext<
  ((event: ReactDragEvent<HTMLDivElement>) => void) | null
>(null);

export function useIsMessageMetaMarkerMenuTriggerEnabled() {
  return useContext(MessageMetaMarkerMenuContext);
}

export function useMessageMetaResponseBoardDragStart() {
  return useContext(MessageMetaResponseBoardDragContext);
}

export function MessageMetaMarkerMenuProvider({
  children,
  onResponseBoardDragStart,
}: {
  children: ReactNode;
  onResponseBoardDragStart?: (
    event: ReactDragEvent<HTMLDivElement>,
  ) => void;
}) {
  // Only conversation messages opt into metadata-owned actions. Keeping the
  // native drag source on the metadata row leaves the message body selectable.
  return (
    <MessageMetaMarkerMenuContext.Provider value={true}>
      <MessageMetaResponseBoardDragContext.Provider
        value={onResponseBoardDragStart ?? null}
      >
        {children}
      </MessageMetaResponseBoardDragContext.Provider>
    </MessageMetaMarkerMenuContext.Provider>
  );
}
