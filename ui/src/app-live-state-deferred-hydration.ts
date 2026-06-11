// app-live-state-deferred-hydration.ts
//
// Owns: deferred full-session hydration scheduling after a tail-first
// hydration, including idle-callback handoff and the composer-busy retry
// window.
//
// Does not own: fetch/adoption state, retrying failed hydration requests, or
// deciding whether a session is eligible for tail-first hydration. Those stay
// in app-live-state.ts because they depend on live hook refs and state.
//
// Split out of: ui/src/app-live-state.ts to keep DOM probing and timer
// orchestration out of the live-state hook body.

import type { MutableRefObject } from "react";

import {
  SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_HARD_TIMEOUT_MS,
  SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_RETRY_MS,
  SESSION_TAIL_FULL_HYDRATION_DEFER_MS,
  SESSION_TAIL_FULL_HYDRATION_IDLE_TIMEOUT_MS,
} from "./app-live-state-hydration";
import { activeConversationComposerHasDraftText } from "./panels/conversation-composer-focus";

export type SessionHydrationOptions = {
  allowDivergentTextRepairAfterNewerRevision?: boolean;
  fromDeferredFullHydration?: boolean;
  queueAfterCurrent?: boolean;
};

export type DeferredFullHydrationHandle = {
  firstScheduledAtMs: number;
  idleId: number | null;
  timeoutId: number | null;
};

type HydrationIdleWindow = Window &
  typeof globalThis & {
    requestIdleCallback?: (
      callback: IdleRequestCallback,
      options?: IdleRequestOptions,
    ) => number;
    cancelIdleCallback?: (handle: number) => void;
  };

type DeferredFullHydrationTimersRef = MutableRefObject<
  Map<string, DeferredFullHydrationHandle>
>;

type ScheduleDeferredFullHydrationParams = {
  timersRef: DeferredFullHydrationTimersRef;
  isMountedRef: MutableRefObject<boolean>;
  sessionId: string;
  sessionStillNeedsHydration: (sessionId: string) => boolean;
  startSessionHydration: (
    sessionId: string,
    options?: SessionHydrationOptions,
  ) => void;
  options?: {
    autoStart?: boolean;
    delayMs?: number;
    firstScheduledAtMs?: number;
  };
};

type ShouldDelayFullHydrationStartParams = {
  sessionId: string;
  options?: SessionHydrationOptions;
  sessionStillNeedsHydration: (sessionId: string) => boolean;
  shouldStartTailFirstHydration: (
    sessionId: string,
    options?: { allowDivergentTextRepairAfterNewerRevision?: boolean },
  ) => boolean;
};

function currentHydrationSchedulerTimeMs() {
  const now = globalThis.performance?.now;
  if (typeof now === "function") {
    return now.call(globalThis.performance);
  }
  return Date.now();
}

function shouldDelayDeferredFullHydrationForComposer(firstScheduledAtMs: number) {
  if (!activeConversationComposerHasDraftText()) {
    return false;
  }
  return (
    currentHydrationSchedulerTimeMs() - firstScheduledAtMs <
    SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_HARD_TIMEOUT_MS
  );
}

export function shouldDelayFullHydrationStartForComposer({
  sessionId,
  options,
  sessionStillNeedsHydration,
  shouldStartTailFirstHydration,
}: ShouldDelayFullHydrationStartParams) {
  if (
    options?.allowDivergentTextRepairAfterNewerRevision === true ||
    options?.fromDeferredFullHydration === true ||
    options?.queueAfterCurrent === true ||
    !activeConversationComposerHasDraftText()
  ) {
    return false;
  }
  if (shouldStartTailFirstHydration(sessionId, options)) {
    return false;
  }
  return sessionStillNeedsHydration(sessionId);
}

export function shouldPromoteDeferredFullHydration(
  options?: SessionHydrationOptions,
) {
  return (
    options?.allowDivergentTextRepairAfterNewerRevision === true ||
    options?.queueAfterCurrent === true
  );
}

export function clearDeferredFullHydrationTimer(
  timersRef: DeferredFullHydrationTimersRef,
  sessionId: string,
) {
  const handle = timersRef.current.get(sessionId);
  if (!handle) {
    return;
  }
  if (handle.timeoutId !== null) {
    window.clearTimeout(handle.timeoutId);
  }
  if (handle.idleId !== null) {
    const idleWindow = window as HydrationIdleWindow;
    idleWindow.cancelIdleCallback?.(handle.idleId);
  }
  timersRef.current.delete(sessionId);
}

export function cancelDeferredFullHydrationTimers(
  timersRef: DeferredFullHydrationTimersRef,
) {
  for (const sessionId of Array.from(timersRef.current.keys())) {
    clearDeferredFullHydrationTimer(timersRef, sessionId);
  }
}

export function scheduleDeferredFullHydration({
  timersRef,
  isMountedRef,
  sessionId,
  sessionStillNeedsHydration,
  startSessionHydration,
  options = {},
}: ScheduleDeferredFullHydrationParams) {
  if (
    !isMountedRef.current ||
    timersRef.current.has(sessionId) ||
    !sessionStillNeedsHydration(sessionId)
  ) {
    return;
  }

  const runHydration = () => {
    if (!isMountedRef.current || !sessionStillNeedsHydration(sessionId)) {
      timersRef.current.delete(sessionId);
      return;
    }
    if (shouldDelayDeferredFullHydrationForComposer(handle.firstScheduledAtMs)) {
      timersRef.current.delete(sessionId);
      scheduleDeferredFullHydration({
        timersRef,
        isMountedRef,
        sessionId,
        sessionStillNeedsHydration,
        startSessionHydration,
        options: {
          delayMs: SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_RETRY_MS,
          firstScheduledAtMs: handle.firstScheduledAtMs,
        },
      });
      return;
    }
    timersRef.current.delete(sessionId);
    startSessionHydration(sessionId, {
      fromDeferredFullHydration: true,
    });
  };
  const handle: DeferredFullHydrationHandle = {
    firstScheduledAtMs:
      options.firstScheduledAtMs ?? currentHydrationSchedulerTimeMs(),
    idleId: null,
    timeoutId: null,
  };
  timersRef.current.set(sessionId, handle);
  if (options.autoStart === false) {
    return;
  }
  handle.timeoutId = window.setTimeout(() => {
    handle.timeoutId = null;
    if (!isMountedRef.current || !sessionStillNeedsHydration(sessionId)) {
      timersRef.current.delete(sessionId);
      return;
    }
    const idleWindow = window as HydrationIdleWindow;
    if (
      typeof idleWindow.requestIdleCallback === "function" &&
      typeof idleWindow.cancelIdleCallback === "function"
    ) {
      handle.idleId = idleWindow.requestIdleCallback(
        () => {
          handle.idleId = null;
          runHydration();
        },
        { timeout: SESSION_TAIL_FULL_HYDRATION_IDLE_TIMEOUT_MS },
      );
      return;
    }
    runHydration();
  }, options.delayMs ?? SESSION_TAIL_FULL_HYDRATION_DEFER_MS);
}
