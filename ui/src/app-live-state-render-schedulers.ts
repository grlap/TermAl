// app-live-state-render-schedulers.ts
//
// Owns: coalesced session-store/session-state and codex-state render
// scheduling for useAppLiveState.
//
// Does not own: live transport, state adoption decisions, or hydration
// retries. Those remain in app-live-state.ts.
//
// Split out of: ui/src/app-live-state.ts.

import {
  startTransition,
  useEffect,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import {
  removeSessionFromStore,
  syncComposerSessionsStoreIncremental,
} from "./session-store";
import type { DraftImageAttachment } from "./app-utils";
import type { CodexState, Session } from "./types";

type UseAppLiveStateRenderSchedulersParams = {
  codexStateRef: MutableRefObject<CodexState>;
  draftAttachmentsBySessionIdRef: MutableRefObject<
    Record<string, DraftImageAttachment[]>
  >;
  draftsBySessionIdRef: MutableRefObject<Record<string, string>>;
  isMountedRef: MutableRefObject<boolean>;
  sessionsRef: MutableRefObject<Session[]>;
  setCodexState: Dispatch<SetStateAction<CodexState>>;
  setSessions: Dispatch<SetStateAction<Session[]>>;
};

export function useAppLiveStateRenderSchedulers(
  params: UseAppLiveStateRenderSchedulersParams,
) {
  const {
    codexStateRef,
    draftAttachmentsBySessionIdRef,
    draftsBySessionIdRef,
    isMountedRef,
    sessionsRef,
    setCodexState,
    setSessions,
  } = params;
  const pendingSessionRenderFrameRef = useRef<number | null>(null);
  const hasPendingSessionRenderRef = useRef(false);
  const pendingSessionStoreSyncIdsRef = useRef<Set<string>>(new Set());
  // Session panes subscribe through session-store. Session deltas publish their
  // changed slices immediately, while the pending-id queue stays armed so the
  // coalesced frame can still prune ids that disappear before the broad
  // `sessions` render flushes.
  const eagerlyPublishedSessionStoreIdsRef = useRef<Set<string>>(new Set());
  const pendingCodexStateRenderFrameRef = useRef<number | null>(null);
  const hasPendingCodexStateRenderRef = useRef(false);

  function queueSessionSliceForRender(sessionId: string) {
    pendingSessionStoreSyncIdsRef.current.add(sessionId);
    eagerlyPublishedSessionStoreIdsRef.current.delete(sessionId);
  }

  function publishQueuedSessionSlices(sessionSnapshot = sessionsRef.current) {
    const pendingSessionIds = pendingSessionStoreSyncIdsRef.current;
    if (pendingSessionIds.size === 0) {
      return;
    }

    const sessionsById = new Map(
      sessionSnapshot.map((session) => [session.id, session]),
    );
    const changedSessions = [...pendingSessionIds].flatMap((sessionId) => {
      const session = sessionsById.get(sessionId);
      return session ? [session] : [];
    });
    if (changedSessions.length === 0) {
      return;
    }
    changedSessions.forEach((session) => {
      eagerlyPublishedSessionStoreIdsRef.current.add(session.id);
    });

    syncComposerSessionsStoreIncremental({
      changedSessions,
      draftsBySessionId: draftsBySessionIdRef.current,
      draftAttachmentsBySessionId: draftAttachmentsBySessionIdRef.current,
      removedSessionIds: [],
    });
  }

  function flushPendingSessionStoreSync(sessionSnapshot = sessionsRef.current) {
    const pendingSessionIds = pendingSessionStoreSyncIdsRef.current;
    if (pendingSessionIds.size === 0) {
      return;
    }

    const sessionsById = new Map(
      sessionSnapshot.map((session) => [session.id, session]),
    );
    const eagerlyPublishedSessionIds = eagerlyPublishedSessionStoreIdsRef.current;
    const changedSessions = [...pendingSessionIds].flatMap((sessionId) => {
      const session = sessionsById.get(sessionId);
      if (!session || eagerlyPublishedSessionIds.has(sessionId)) {
        return [];
      }
      return [session];
    });
    const removedSessionIds = [...pendingSessionIds].filter(
      (sessionId) => !sessionsById.has(sessionId),
    );
    pendingSessionIds.clear();
    eagerlyPublishedSessionIds.clear();
    if (changedSessions.length === 0 && removedSessionIds.length === 0) {
      return;
    }
    if (changedSessions.length === 0) {
      removedSessionIds.forEach((sessionId) => {
        removeSessionFromStore({ sessionId });
      });
      return;
    }

    syncComposerSessionsStoreIncremental({
      changedSessions,
      draftsBySessionId: draftsBySessionIdRef.current,
      draftAttachmentsBySessionId: draftAttachmentsBySessionIdRef.current,
      removedSessionIds,
    });
  }

  function cancelPendingSessionRender() {
    if (pendingSessionRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingSessionRenderFrameRef.current);
      pendingSessionRenderFrameRef.current = null;
    }
    hasPendingSessionRenderRef.current = false;
    pendingSessionStoreSyncIdsRef.current.clear();
    eagerlyPublishedSessionStoreIdsRef.current.clear();
  }

  function cancelPendingCodexStateRender() {
    if (pendingCodexStateRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingCodexStateRenderFrameRef.current);
      pendingCodexStateRenderFrameRef.current = null;
    }
    hasPendingCodexStateRenderRef.current = false;
  }

  function flushPendingCodexStateRender() {
    pendingCodexStateRenderFrameRef.current = null;
    if (!hasPendingCodexStateRenderRef.current || !isMountedRef.current) {
      return;
    }

    hasPendingCodexStateRenderRef.current = false;
    const nextCodexState = codexStateRef.current;
    startTransition(() => {
      setCodexState(nextCodexState);
    });
  }

  function scheduleCodexStateRender() {
    hasPendingCodexStateRenderRef.current = true;
    if (pendingCodexStateRenderFrameRef.current !== null) {
      return;
    }
    if (
      typeof window === "undefined" ||
      typeof window.requestAnimationFrame !== "function"
    ) {
      flushPendingCodexStateRender();
      return;
    }

    pendingCodexStateRenderFrameRef.current = window.requestAnimationFrame(
      flushPendingCodexStateRender,
    );
  }

  function flushAndCancelPendingSessionRender(
    sessionSnapshot = sessionsRef.current,
  ) {
    if (pendingSessionRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingSessionRenderFrameRef.current);
      pendingSessionRenderFrameRef.current = null;
    }
    flushPendingSessionStoreSync(sessionSnapshot);
    hasPendingSessionRenderRef.current = false;
  }

  function flushPendingSessionRender() {
    pendingSessionRenderFrameRef.current = null;
    if (!hasPendingSessionRenderRef.current || !isMountedRef.current) {
      return;
    }

    hasPendingSessionRenderRef.current = false;
    const nextSessions = sessionsRef.current;
    flushPendingSessionStoreSync(nextSessions);
    startTransition(() => {
      setSessions(nextSessions);
    });
  }

  function scheduleSessionRender() {
    hasPendingSessionRenderRef.current = true;
    if (pendingSessionRenderFrameRef.current !== null) {
      return;
    }
    if (
      typeof window === "undefined" ||
      typeof window.requestAnimationFrame !== "function"
    ) {
      flushPendingSessionRender();
      return;
    }

    pendingSessionRenderFrameRef.current = window.requestAnimationFrame(
      flushPendingSessionRender,
    );
  }

  useEffect(() => {
    return () => {
      cancelPendingSessionRender();
      cancelPendingCodexStateRender();
    };
    // These helpers only close over stable refs and React setters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return {
    cancelPendingCodexStateRender,
    flushAndCancelPendingSessionRender,
    publishQueuedSessionSlices,
    queueSessionSliceForRender,
    scheduleCodexStateRender,
    scheduleSessionRender,
  };
}
