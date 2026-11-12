// Owns cancellable global-position seeks through the existing bounded history API.
// Does not fetch overview data, render a scrollbar, or choose FOLLOW/STAY.
// Split from conversation-overview-controller.ts, retaining its admission fences.
import { requestSessionHistoryAroundPage } from "../session-history-demand";
import type { VirtualizedConversationMessageListHandleRef } from "./virtualized-conversation-types";

export type PositionNavigationContext = {
  sessionId: string;
  scrollStateKey: string;
  isActive: boolean;
  messageCount: number;
  messageStartIndex: number;
  renderedMessageCount: number;
  virtualizerHandleRef: VirtualizedConversationMessageListHandleRef;
  onNavigateStart?: () => void;
};

export function createConversationPositionNavigation(current: () => PositionNavigationContext) {
  let abort: AbortController | null = null;
  let retry: (() => void) | null = null;
  const frames = new Set<number>();
  const cancel = () => {
    abort?.abort();
    abort = null;
    retry = null;
    frames.forEach(frame => window.cancelAnimationFrame(frame));
    frames.clear();
  };
  const schedule = (callback: () => void) => {
    let synchronous = true;
    let fired = false;
    const frame = window.requestAnimationFrame(() => {
      fired = true;
      if (!synchronous) frames.delete(frame);
      callback();
    });
    synchronous = false;
    if (!fired) frames.add(frame);
  };
  const navigate = (position: number): Promise<boolean> => {
    cancel();
    const context = current();
    if (!context.isActive || context.messageCount <= 0) return Promise.resolve(false);
    context.onNavigateStart?.();
    const generation = context.virtualizerHandleRef.current?.beginUserScrollNavigation();
    const request = new AbortController();
    abort = request;
    const target = Math.max(0, Math.min(Math.floor(position), context.messageCount - 1));
    const isCurrent = () => {
      const next = current();
      return !request.signal.aborted && next.isActive && next.sessionId === context.sessionId && next.scrollStateKey === context.scrollStateKey &&
        (generation === undefined || next.virtualizerHandleRef.current?.getUserScrollGeneration() === generation);
    };
    const jump = () => {
      const next = current();
      const local = target - next.messageStartIndex;
      return local >= 0 && local < next.renderedMessageCount &&
        (next.virtualizerHandleRef.current?.jumpToMessageIndex(local, { align: "center", flush: true, preserveDetachedAuthority: true }) ?? false);
    };
    return new Promise(resolve => {
      let phase: "waiting" | "positioning" | "done" = "waiting";
      let loading = false;
      let unsubscribe: (() => void) | undefined;
      const finish = (applied: boolean) => {
        if (phase === "done") return;
        phase = "done";
        unsubscribe?.();
        request.signal.removeEventListener("abort", cancelled);
        if (abort === request) abort = null;
        if (retry === retryAfterCommit) retry = null;
        resolve(applied);
      };
      const cancelled = () => finish(false);
      const attempt = () => {
        if (phase !== "waiting" || loading) return;
        if (!isCurrent()) { finish(false); return; }
        phase = "positioning";
        if (!jump()) { phase = "waiting"; return; }
        schedule(() => {
          if (!isCurrent()) { finish(false); return; }
          if (jump()) finish(true);
          else phase = "waiting";
        });
      };
      const retryAfterCommit = () => {
        if (phase === "waiting" && !loading) schedule(attempt);
      };
      retry = retryAfterCommit;
      request.signal.addEventListener("abort", cancelled, { once: true });
      unsubscribe = context.virtualizerHandleRef.current?.subscribeToCommittedLayout?.(retryAfterCommit);
      attempt();
      if (phase !== "waiting") return;
      loading = true;
      void requestSessionHistoryAroundPage(context.sessionId, target, { signal: request.signal }).then(applied => {
        loading = false;
        if (!applied || !isCurrent()) { finish(false); return; }
        retryAfterCommit();
      }, () => finish(false));
    });
  };
  return { cancel, navigate, committed: () => retry?.(), isPending: () => abort !== null };
}
