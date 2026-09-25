// App-scoped projection of revision-gated live state; no independent SSE or
// inference that a notification recipient is idle or registered as waiting.
import { createContext, useContext, useMemo, type ReactNode } from "react";
import { useStableEvent } from "./panels/use-stable-event";
import { testRunSessionMarker, type TestRunSummary } from "./test-runs";

export const TestRunsOpenContext = createContext<((sessionId?: string | null) => void) | null>(null);

export const TestRunsContext = createContext<{
  runs: readonly TestRunSummary[];
  open: ((sessionId?: string | null) => void) | null;
}>({ runs: [], open: null });

export function TestRunsProvider({ runs, open, children }: {
  runs: readonly TestRunSummary[]; open: (sessionId?: string | null) => void; children: ReactNode;
}) {
  const stableOpen = useStableEvent(open);
  const value = useMemo(() => ({ runs, open: stableOpen }), [runs, stableOpen]);
  return <TestRunsOpenContext.Provider value={stableOpen}>
    <TestRunsContext.Provider value={value}>{children}</TestRunsContext.Provider>
  </TestRunsOpenContext.Provider>;
}

export function TestRunSessionMarker({ sessionId }: { sessionId: string }) {
  const { runs, open } = useContext(TestRunsContext);
  const label = testRunSessionMarker(runs, sessionId);
  if (!label) return null;
  return <button type="button" className="test-run-session-marker" onClick={event => {
    event.stopPropagation();
    open?.(sessionId);
  }} title={`${label} — Open test runs for this session`}>{label}</button>;
}
