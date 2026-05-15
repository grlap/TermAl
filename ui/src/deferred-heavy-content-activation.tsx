// Owns the virtualization-controlled activation gate for deferred heavy content.
// Does not own Markdown/code rendering or viewport observation of individual cards.
// Split from ui/src/message-cards.tsx.

import { createContext, useContext, type ReactNode } from "react";

const DeferredHeavyContentActivationContext = createContext(true);

export function DeferredHeavyContentActivationProvider({
  allowActivation,
  children,
}: {
  allowActivation: boolean;
  children: ReactNode;
}) {
  return (
    <DeferredHeavyContentActivationContext.Provider value={allowActivation}>
      {children}
    </DeferredHeavyContentActivationContext.Provider>
  );
}

export function useDeferredHeavyContentActivation() {
  return useContext(DeferredHeavyContentActivationContext);
}
