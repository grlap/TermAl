// Owns: message-stack scrolling, tail-follow state, and search-match scroll
// convergence for SessionPaneView.
// Does not own: pane tab rendering, active-tab selection, source-file loading,
// or transcript card rendering.
// Split from: ui/src/SessionPaneView.tsx.

import {
  startTransition,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type MutableRefObject,
  type RefObject,
  type TouchEvent as ReactTouchEvent,
  type UIEvent as ReactUIEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import {
  canNestedScrollableConsumeWheel,
  clamp,
  normalizeWheelDelta,
  pruneSessionFlags,
} from "./app-utils";
import {
  MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  notifyMessageStackScrollWrite,
  type MessageStackScrollWriteKind,
} from "./message-stack-scroll-sync";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import {
  requestSessionHistoryStartPage,
  requestSessionHistoryTailPage,
} from "./session-history-demand";
import {
  resolveSettledScrollMinimumAttempts,
  syncMessageStackScrollPosition,
} from "./scroll-position";
import type { SessionSearchMatch } from "./session-find";
import type { Message, Session } from "./types";
import type { PaneViewMode } from "./workspace";

const SESSION_PAGE_SCROLL_VIEWPORT_FACTOR = 0.85;
const SESSION_PAGE_SCROLL_MINIMUM_PX = 160;

export function resolveSessionPageScrollDistance(clientHeight: number) {
  return Math.max(
    Math.round(Math.max(0, clientHeight) * SESSION_PAGE_SCROLL_VIEWPORT_FACTOR),
    SESSION_PAGE_SCROLL_MINIMUM_PX,
  );
}

type NewResponseIndicatorKind = "activity" | "response";

export function resolveNewResponseIndicatorVisibility({
  hasUnloadedNewerHistory,
  indicatorKind,
  liveTailPinned,
}: {
  hasUnloadedNewerHistory: boolean;
  indicatorKind: NewResponseIndicatorKind | null;
  liveTailPinned: boolean;
}) {
  return (
    hasUnloadedNewerHistory ||
    (!liveTailPinned && indicatorKind !== null)
  );
}

type PaneScrollPosition = {
  top: number;
  shouldStick: boolean;
};

type UseSessionPaneScrollStateParams = {
  activeSession: Session | null;
  activeSessionSearchMatch: SessionSearchMatch | null;
  defaultScrollToBottom: boolean;
  deferContentScrollEffects: boolean;
  forceSessionScrollToBottomRef: MutableRefObject<
    Record<string, true | undefined>
  >;
  hasSessionFindQuery: boolean;
  isActive: boolean;
  isSending: boolean;
  isSessionTabActive: boolean;
  onScrollToBottomRequestHandled: (token: number) => void;
  paneContentSignatures: Record<string, string>;
  paneId: string;
  paneMessageContentSignatures: Record<string, string>;
  paneRootRef: RefObject<HTMLElement | null>;
  paneScrollPositions: Record<string, PaneScrollPosition>;
  paneShouldStickToBottomRef: MutableRefObject<
    Record<string, boolean | undefined>
  >;
  paneViewMode: PaneViewMode;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  scrollStateKey: string;
  sessions: Session[];
  showWaitingIndicator: boolean;
  visibleContentSignature: string;
  visibleLastMessageAuthor: Message["author"] | undefined;
  visibleMessageContentSignature: string;
};

export function useSessionPaneScrollState({
  activeSession,
  activeSessionSearchMatch,
  defaultScrollToBottom,
  deferContentScrollEffects,
  forceSessionScrollToBottomRef,
  hasSessionFindQuery,
  isActive,
  isSending,
  isSessionTabActive,
  onScrollToBottomRequestHandled,
  paneContentSignatures,
  paneId,
  paneMessageContentSignatures,
  paneRootRef,
  paneScrollPositions,
  paneShouldStickToBottomRef,
  paneViewMode,
  pendingScrollToBottomRequest,
  scrollStateKey,
  sessions,
  showWaitingIndicator,
  visibleContentSignature,
  visibleLastMessageAuthor,
  visibleMessageContentSignature,
}: UseSessionPaneScrollStateParams) {
  const messageStackRef = useRef<HTMLElement | null>(null);
  const settledScrollToBottomCancelRef = useRef<(() => void) | null>(null);
  const previousShowWaitingIndicatorByKeyRef = useRef<
    Record<string, boolean | undefined>
  >({});
  const paneProgrammaticBottomFollowRef = useRef<{
    key: string | null;
    until: number;
  }>({ key: null, until: Number.NEGATIVE_INFINITY });
  const paneTailFollowUserEscapeByKeyRef = useRef<
    Record<string, true | undefined>
  >({});
  const currentScrollStateKeyRef = useRef(scrollStateKey);
  const pendingStartHistoryDemandRef = useRef<{ key: string } | null>(null);
  const pendingTailHistoryDemandRef = useRef<{ key: string } | null>(null);
  const paneLastTouchClientYRef = useRef<number | null>(null);
  const sessionSearchItemRefsRef = useRef<Record<string, HTMLElement | null>>(
    {},
  );
  const [newResponseIndicatorByKey, setNewResponseIndicatorByKey] = useState<
    Record<string, NewResponseIndicatorKind | undefined>
  >({});
  const [liveTailPinnedByKey, setLiveTailPinnedByKey] = useState<
    Record<string, boolean | undefined>
  >({});
  const [visitedSessionIds, setVisitedSessionIds] = useState<
    Record<string, true | undefined>
  >({});
  currentScrollStateKeyRef.current = scrollStateKey;

  useEffect(() => {
    // Leaving a pane/session must not let its unresolved boundary demand block
    // a fresh request when the reader returns. Each request is also compared by
    // object identity in its completion callback, so a late old completion
    // cannot clear a newer demand for the same key.
    if (pendingStartHistoryDemandRef.current?.key !== scrollStateKey) {
      pendingStartHistoryDemandRef.current = null;
    }
    if (pendingTailHistoryDemandRef.current?.key !== scrollStateKey) {
      pendingTailHistoryDemandRef.current = null;
    }
  }, [scrollStateKey]);

  const savedScrollPosition = paneScrollPositions[scrollStateKey];
  const savedScrollShouldStick = savedScrollPosition?.shouldStick === true;
  const waitingIndicatorShouldStick = savedScrollShouldStick;
  const hasUnloadedNewerHistory = activeSession?.hasNewerHistory === true;
  const newResponseIndicatorKind =
    newResponseIndicatorByKey[scrollStateKey] ?? null;
  const newResponseIndicatorLabel =
    hasUnloadedNewerHistory
      ? "Jump to latest"
      : newResponseIndicatorKind === "activity"
        ? "New activity"
        : "New response";

  function getTailFollowIntent() {
    if (hasUnloadedNewerHistory) {
      return false;
    }
    return paneShouldStickToBottomRef.current[paneId] ?? true;
  }

  function setTailFollowIntent(nextValue: boolean) {
    paneShouldStickToBottomRef.current[paneId] = nextValue;
    if (nextValue) {
      delete paneTailFollowUserEscapeByKeyRef.current[scrollStateKey];
    } else {
      paneTailFollowUserEscapeByKeyRef.current[scrollStateKey] = true;
    }
    setLiveTailPinnedByKey((current) => {
      if (current[scrollStateKey] === nextValue) {
        return current;
      }
      return {
        ...current,
        [scrollStateKey]: nextValue,
      };
    });
  }

  const tailFollowIntent =
    liveTailPinnedByKey[scrollStateKey] ??
    savedScrollPosition?.shouldStick ??
    getTailFollowIntent();
  const liveTailPinned = !hasUnloadedNewerHistory && tailFollowIntent;
  // A delayed indicator-state transition must not override explicit
  // attachment. Geometry can briefly include estimated or stale virtual
  // spacer height while residency changes, but a tail-pinned pane has no
  // unseen response to advertise.
  const showNewResponseIndicator = resolveNewResponseIndicatorVisibility({
    hasUnloadedNewerHistory,
    indicatorKind: newResponseIndicatorKind,
    liveTailPinned,
  });

  function hasTailFollowUserEscape() {
    return Boolean(paneTailFollowUserEscapeByKeyRef.current[scrollStateKey]);
  }

  function markTailFollowUserEscape() {
    paneTailFollowUserEscapeByKeyRef.current[scrollStateKey] = true;
    setTailFollowIntent(false);
    cancelSettledScrollToBottom();
  }

  function keepPaneScrollPositionPinned(node: HTMLElement) {
    paneScrollPositions[scrollStateKey] = {
      top: node.scrollTop,
      shouldStick: true,
    };
  }

  function beginPaneProgrammaticBottomFollow() {
    paneProgrammaticBottomFollowRef.current = {
      key: scrollStateKey,
      until: performance.now() + MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
    };
  }

  function cancelPaneProgrammaticBottomFollow() {
    paneProgrammaticBottomFollowRef.current = {
      key: null,
      until: Number.NEGATIVE_INFINITY,
    };
  }

  function isPaneProgrammaticBottomFollowActive() {
    const bottomFollow = paneProgrammaticBottomFollowRef.current;
    return (
      bottomFollow.key === scrollStateKey &&
      bottomFollow.until >= performance.now()
    );
  }

  useEffect(() => {
    if (paneProgrammaticBottomFollowRef.current.key !== scrollStateKey) {
      paneProgrammaticBottomFollowRef.current = {
        key: null,
        until: Number.NEGATIVE_INFINITY,
      };
    }
  }, [scrollStateKey]);

  function setNewResponseIndicator(
    key: string,
    visible: boolean,
    kind: NewResponseIndicatorKind = "response",
  ) {
    startTransition(() => {
      setNewResponseIndicatorByKey((current) => {
        const currentKind = current[key];
        if (
          (!visible && currentKind === undefined) ||
          (visible &&
            (currentKind === kind ||
              (currentKind === "response" && kind === "activity")))
        ) {
          return current;
        }

        const nextState = { ...current };
        if (visible) {
          nextState[key] = kind;
        } else {
          delete nextState[key];
        }
        return nextState;
      });
    });
  }

  function handleConversationSearchItemMount(
    itemKey: string,
    node: HTMLElement | null,
  ) {
    if (node) {
      sessionSearchItemRefsRef.current[itemKey] = node;
      return;
    }

    delete sessionSearchItemRefsRef.current[itemKey];
  }

  useEffect(() => {
    if (!hasSessionFindQuery) {
      sessionSearchItemRefsRef.current = {};
    }
  }, [activeSession?.id, hasSessionFindQuery]);

  function scrollToLatestMessage(
    behavior: ScrollBehavior,
    force = false,
    scrollKind?: MessageStackScrollWriteKind,
  ) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const nextScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (Math.abs(node.scrollTop - nextScrollTop) > (force ? 0.5 : 1)) {
      node.scrollTo({
        top: nextScrollTop,
        behavior,
      });
      if (scrollKind === "bottom_follow") {
        beginPaneProgrammaticBottomFollow();
      } else if (scrollKind) {
        cancelPaneProgrammaticBottomFollow();
      }
      notifyMessageStackScrollWrite(node, {
        scrollKind,
      });
    }
    setTailFollowIntent(true);
    paneScrollPositions[scrollStateKey] = {
      top: nextScrollTop,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }

  function scrollVirtualizedMessageStackToBottom(
    node: HTMLElement,
    options: {
      scrollKind?: Extract<
        MessageStackScrollWriteKind,
        "bottom_boundary" | "bottom_pin"
      >;
      scrollSource?: "programmatic" | "user";
    } = {},
  ) {
    if (!node.querySelector(".virtualized-message-list")) {
      return false;
    }

    const nextScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (Math.abs(node.scrollTop - nextScrollTop) > 0.5) {
      node.scrollTop = nextScrollTop;
    }
    notifyMessageStackScrollWrite(node, {
      scrollKind: options.scrollKind ?? "bottom_pin",
      scrollSource: options.scrollSource,
    });
    setTailFollowIntent(true);
    paneScrollPositions[scrollStateKey] = {
      top: Number.MAX_SAFE_INTEGER,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
    return true;
  }

  useLayoutEffect(() => {
    const node = messageStackRef.current;
    if (
      !node ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session"
    ) {
      return;
    }

    // Composer measurement may need an immediate, same-task correction before
    // paint. It requests that correction instead of writing scrollTop itself;
    // this handler remains the authority and rejects the request once explicit
    // tail-follow intent has been released by user navigation.
    const handleBottomRepinRequest = () => {
      if (
        currentScrollStateKeyRef.current !== scrollStateKey ||
        !getTailFollowIntent()
      ) {
        return;
      }
      scrollToLatestMessage("auto", true);
    };

    node.addEventListener(
      MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
      handleBottomRepinRequest,
    );
    return () => {
      node.removeEventListener(
        MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
        handleBottomRepinRequest,
      );
    };
  }, [
    activeSession?.id,
    hasUnloadedNewerHistory,
    isActive,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    const node = messageStackRef.current;
    const ResizeObserverCtor = globalThis.ResizeObserver;
    if (
      !node ||
      typeof ResizeObserverCtor !== "function" ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session"
    ) {
      return;
    }

    // The composer is a sibling of the scroll container. Its animated growth
    // and shrink therefore change the message stack's own client height without
    // changing transcript content. ResizeObserver fires for every rendered
    // transition step; when explicit tail-follow is still active, route each
    // correction through the existing single scroll authority so the live tail
    // remains exactly pinned without introducing another scroll writer.
    const resizeObserver = new ResizeObserverCtor(() => {
      if (
        currentScrollStateKeyRef.current !== scrollStateKey ||
        !getTailFollowIntent()
      ) {
        return;
      }

      // A viewport resize does not change transcript residency. Use the pane's
      // ordinary exact-bottom writer without classifying this as `bottom_pin`
      // (which remounts the virtualizer's bottom range) or `bottom_follow`
      // (which starts a smooth-scroll cooldown). Repeating either lifecycle on
      // every textarea transition frame made the rail and transcript oscillate.
      scrollToLatestMessage("auto", true);
    });
    resizeObserver.observe(node);

    return () => {
      resizeObserver.disconnect();
    };
  }, [
    activeSession?.id,
    hasUnloadedNewerHistory,
    isActive,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
  ]);

  function scrollMessageStackByDelta(
    deltaY: number,
    options: {
      scrollKind?: MessageStackScrollWriteKind;
    } = {},
  ) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (maxScrollTop <= 0) {
      return;
    }

    const nextScrollTop = clamp(node.scrollTop + deltaY, 0, maxScrollTop);
    if (Math.abs(nextScrollTop - node.scrollTop) < 0.5) {
      return;
    }

    const isUpwardScroll = deltaY < -0.5;
    if (isUpwardScroll) {
      paneTailFollowUserEscapeByKeyRef.current[scrollStateKey] = true;
    }
    cancelPaneProgrammaticBottomFollow();
    node.scrollTop = nextScrollTop;
    notifyMessageStackScrollWrite(node, {
      scrollKind: options.scrollKind,
      scrollSource: "user",
    });
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
    if (isUpwardScroll) {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else if (shouldStick) {
      setTailFollowIntent(true);
      setNewResponseIndicator(scrollStateKey, false);
    } else {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    }
  }

  function isMessageStackNearBottom() {
    const node = messageStackRef.current;
    if (!node) {
      return true;
    }
    return node.scrollHeight - node.scrollTop - node.clientHeight < 72;
  }

  function followLatestMessageForPromptSend() {
    if (hasUnloadedNewerHistory) {
      scrollMessageStackToBoundary("bottom");
      return undefined;
    }
    if (getTailFollowIntent() || isMessageStackNearBottom()) {
      scrollToLatestMessage("smooth", false, "bottom_follow");
      return undefined;
    }

    return scheduleSettledScrollToBottom("auto", {
      maxAttempts: 24,
      minAttempts: 4,
    });
  }

  function scrollMessageStackByPage(direction: -1 | 1) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    scrollMessageStackByDelta(resolveSessionPageScrollDistance(node.clientHeight) * direction, {
      scrollKind: "page_jump",
    });
  }

  function scrollSessionMessageStackByPageJump(direction: -1 | 1) {
    scrollMessageStackByPage(direction);
  }

  function scrollMessageStackToBoundary(boundary: "top" | "bottom") {
    if (boundary === "bottom") {
      const applyBottomBoundary = () => {
        cancelSettledScrollToBottom();
        cancelPaneProgrammaticBottomFollow();
        const node = messageStackRef.current;
        if (node) {
          if (
            !scrollVirtualizedMessageStackToBottom(node, {
              scrollKind: "bottom_boundary",
              scrollSource: "user",
            })
          ) {
            scrollToLatestMessage("auto", true, "seek");
          }
        }
      };
      if (hasUnloadedNewerHistory && activeSession) {
        if (pendingTailHistoryDemandRef.current?.key === scrollStateKey) {
          return;
        }
        setTailFollowIntent(false);
        setNewResponseIndicator(scrollStateKey, true);
        const requestedScrollStateKey = scrollStateKey;
        const demand = { key: requestedScrollStateKey };
        pendingTailHistoryDemandRef.current = demand;
        void requestSessionHistoryTailPage(activeSession.id).then((applied) => {
          if (pendingTailHistoryDemandRef.current === demand) {
            pendingTailHistoryDemandRef.current = null;
          }
          if (
            !applied ||
            currentScrollStateKeyRef.current !== requestedScrollStateKey
          ) {
            return;
          }
          requestAnimationFrame(() => {
            if (
              currentScrollStateKeyRef.current === requestedScrollStateKey
            ) {
              applyBottomBoundary();
            }
          });
        });
        return;
      }
      applyBottomBoundary();
      return;
    }

    const applyTopBoundary = () => {
      const node = messageStackRef.current;
      if (!node) {
        return;
      }
      cancelSettledScrollToBottom();
      cancelPaneProgrammaticBottomFollow();
      node.scrollTo({
        top: 0,
        behavior: "auto",
      });
      notifyMessageStackScrollWrite(node, {
        scrollKind: "seek",
        scrollSource: "user",
      });
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    };
    const needsTrueStartPage =
      activeSession &&
      (activeSession.hasOlderHistory ??
        (activeSession.messagesLoaded === false &&
          activeSession.hasNewerHistory !== true));
    if (!needsTrueStartPage) {
      applyTopBoundary();
      return;
    }
    if (pendingStartHistoryDemandRef.current?.key === scrollStateKey) {
      return;
    }
    const requestedScrollStateKey = scrollStateKey;
    const demand = { key: requestedScrollStateKey };
    pendingStartHistoryDemandRef.current = demand;
    void requestSessionHistoryStartPage(activeSession.id).then(() => {
      if (pendingStartHistoryDemandRef.current === demand) {
        pendingStartHistoryDemandRef.current = null;
      }
      if (currentScrollStateKeyRef.current !== requestedScrollStateKey) {
        return;
      }
      // A failed/superseded page request still honors the user's navigation
      // against the resident window. Silently swallowing the keypress makes a
      // transient history race indistinguishable from broken input.
      requestAnimationFrame(() => {
        if (currentScrollStateKeyRef.current === requestedScrollStateKey) {
          applyTopBoundary();
        }
      });
    });
  }

  const handleMessageStackWheelRef = useRef<
    ((event: WheelEvent) => void) | null
  >(null);
  handleMessageStackWheelRef.current = function handleMessageStackWheel(
    event: WheelEvent,
  ) {
    if (event.defaultPrevented || event.ctrlKey) {
      return;
    }

    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const deltaY = normalizeWheelDelta(event, node);
    if (Math.abs(deltaY) < 0.5) {
      return;
    }

    if (canNestedScrollableConsumeWheel(event.target, node, deltaY)) {
      return;
    }

    event.preventDefault();
    scrollMessageStackByDelta(deltaY, {
      scrollKind: "incremental",
    });
  };

  useEffect(() => {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }
    const listener = (event: WheelEvent) => {
      handleMessageStackWheelRef.current?.(event);
    };
    node.addEventListener("wheel", listener, { passive: false });
    return () => {
      node.removeEventListener("wheel", listener);
    };
  }, []);

  const handleNestedTargetPageKeyRef = useRef<
    ((event: KeyboardEvent) => void) | null
  >(null);
  handleNestedTargetPageKeyRef.current = function handleNestedTargetPageKey(
    event: KeyboardEvent,
  ) {
    if (
      event.defaultPrevented ||
      (event.key !== "PageUp" && event.key !== "PageDown") ||
      !isNestedEditablePageKeyTarget(event.target)
    ) {
      return;
    }
    if (
      !(event.target instanceof Node) ||
      !paneRootRef.current?.contains(event.target)
    ) {
      return;
    }

    const command = resolvePaneScrollCommand(
      {
        altKey: event.altKey,
        ctrlKey: event.ctrlKey,
        key: event.key,
        metaKey: event.metaKey,
        shiftKey: event.shiftKey,
      },
      event.target,
    );
    if (!command) {
      return;
    }

    event.preventDefault();
    if (command.kind === "boundary") {
      scrollMessageStackToBoundary(
        command.direction === "up" ? "top" : "bottom",
      );
      return;
    }

    scrollSessionMessageStackByPageJump(
      command.direction === "up" ? -1 : 1,
    );
  };

  useEffect(() => {
    if (!isActive || paneViewMode !== "session") {
      return;
    }

    const listener = (event: KeyboardEvent) => {
      handleNestedTargetPageKeyRef.current?.(event);
    };
    window.addEventListener("keydown", listener, true);
    return () => {
      window.removeEventListener("keydown", listener, true);
    };
  }, [isActive, paneViewMode]);

  function scheduleSettledScrollToBottom(
    behavior: ScrollBehavior,
    options: {
      maxAttempts?: number;
      minAttempts?: number;
      onComplete?: () => void;
      preferVirtualizedBoundary?: boolean;
      scrollKind?: MessageStackScrollWriteKind;
    } = {},
  ) {
    cancelSettledScrollToBottom();

    let frameId = 0;
    let cancelled = false;
    let completed = false;
    const maxAttempts = options.maxAttempts ?? 12;
    let remainingAttempts = maxAttempts;
    const minimumAttempts = resolveSettledScrollMinimumAttempts(
      maxAttempts,
      options.minAttempts,
    );
    let attemptCount = 0;
    let previousScrollHeight = -1;
    let stableFrameCount = 0;

    function complete() {
      if (cancelled || completed) {
        return;
      }

      completed = true;
      if (settledScrollToBottomCancelRef.current === cancel) {
        settledScrollToBottomCancelRef.current = null;
      }
      options.onComplete?.();
    }

    const tick = () => {
      frameId = 0;
      attemptCount += 1;
      const node = messageStackRef.current;
      if (!node) {
        remainingAttempts -= 1;
        if (remainingAttempts > 0) {
          frameId = window.requestAnimationFrame(tick);
        } else {
          complete();
        }
        return;
      }

      if (
        options.preferVirtualizedBoundary &&
        scrollVirtualizedMessageStackToBottom(node)
      ) {
        complete();
        return;
      }

      scrollToLatestMessage(
        behavior,
        attemptCount <= minimumAttempts,
        options.scrollKind,
      );

      const bottomGap = Math.max(
        node.scrollHeight - node.clientHeight - node.scrollTop,
        0,
      );
      const heightStable =
        previousScrollHeight >= 0 &&
        Math.abs(node.scrollHeight - previousScrollHeight) <= 16;
      if (bottomGap <= 4 && heightStable) {
        stableFrameCount += 1;
      } else {
        stableFrameCount = 0;
      }

      previousScrollHeight = node.scrollHeight;
      remainingAttempts -= 1;
      if (
        remainingAttempts > 0 &&
        (attemptCount < minimumAttempts || stableFrameCount < 2)
      ) {
        frameId = window.requestAnimationFrame(tick);
      } else {
        complete();
      }
    };

    const cancel = () => {
      cancelled = true;
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
      if (settledScrollToBottomCancelRef.current === cancel) {
        settledScrollToBottomCancelRef.current = null;
      }
    };

    settledScrollToBottomCancelRef.current = cancel;
    frameId = window.requestAnimationFrame(tick);
    return cancel;
  }

  function cancelSettledScrollToBottom() {
    const cancel = settledScrollToBottomCancelRef.current;
    settledScrollToBottomCancelRef.current = null;
    cancel?.();
  }

  function handleMessageStackTouchStart(event: ReactTouchEvent<HTMLElement>) {
    paneLastTouchClientYRef.current = event.touches[0]?.clientY ?? null;
  }

  function isTailFollowEscapeInput(
    event:
      | ReactWheelEvent<HTMLElement>
      | ReactTouchEvent<HTMLElement>
      | ReactKeyboardEvent<HTMLElement>
      | ReactMouseEvent<HTMLElement>,
  ) {
    if (event.type === "wheel" && "deltaY" in event) {
      return event.deltaY < -0.5;
    }

    if (event.type === "touchmove" && "touches" in event) {
      const currentTouchClientY = event.touches[0]?.clientY ?? null;
      const previousTouchClientY = paneLastTouchClientYRef.current;
      paneLastTouchClientYRef.current = currentTouchClientY;
      return (
        currentTouchClientY !== null &&
        previousTouchClientY !== null &&
        currentTouchClientY > previousTouchClientY + 0.5
      );
    }

    if (event.type === "keydown" && "key" in event) {
      return (
        event.key === "PageUp" ||
        event.key === "ArrowUp" ||
        event.key === "Home" ||
        (event.key === " " && event.shiftKey)
      );
    }

    return event.type === "mousedown" && event.target === event.currentTarget;
  }

  function handleMessageStackUserScrollIntent(
    event:
      | ReactWheelEvent<HTMLElement>
      | ReactTouchEvent<HTMLElement>
      | ReactKeyboardEvent<HTMLElement>
      | ReactMouseEvent<HTMLElement>,
  ) {
    cancelPaneProgrammaticBottomFollow();
    if (isTailFollowEscapeInput(event)) {
      markTailFollowUserEscape();
    }
  }

  function handleMessageStackScroll(event: ReactUIEvent<HTMLElement>) {
    const node = event.currentTarget;
    const previousScrollPosition = paneScrollPositions[scrollStateKey];
    const previousTop = previousScrollPosition?.top;
    const movedUpFromRecordedPosition =
      typeof previousTop === "number" &&
      previousTop < Number.MAX_SAFE_INTEGER / 2 &&
      node.scrollTop < previousTop - 1;
    const movedUpAfterUserEscape =
      hasTailFollowUserEscape() && movedUpFromRecordedPosition;
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
    if (hasUnloadedNewerHistory) {
      cancelPaneProgrammaticBottomFollow();
      cancelSettledScrollToBottom();
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: node.scrollTop,
        shouldStick: false,
      };
      setNewResponseIndicator(scrollStateKey, true);
      return;
    }
    if (isPaneProgrammaticBottomFollowActive()) {
      const targetTop = Math.max(node.scrollHeight - node.clientHeight, 0);
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: targetTop,
        shouldStick: true,
      };
      setNewResponseIndicator(scrollStateKey, false);
      if (targetTop - node.scrollTop <= 4) {
        cancelPaneProgrammaticBottomFollow();
      }
      return;
    }
    if (movedUpAfterUserEscape) {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else if (shouldStick) {
      setTailFollowIntent(true);
      setNewResponseIndicator(scrollStateKey, false);
    } else if (
      hasTailFollowUserEscape() ||
      movedUpFromRecordedPosition ||
      !getTailFollowIntent()
    ) {
      setTailFollowIntent(false);
      cancelSettledScrollToBottom();
    } else {
      keepPaneScrollPositionPinned(node);
      setNewResponseIndicator(scrollStateKey, false);
    }
  }

  function restoreMessageStackScrollTop(targetTop: number) {
    const node = messageStackRef.current;
    if (!node) {
      return false;
    }
    // A saved detached position can be recorded during the brief gap between
    // DOM growth and the next bottom-follow write. Never replay it after the
    // pane has re-entered explicit tail-follow.
    if (getTailFollowIntent()) {
      return false;
    }

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (targetTop > maxScrollTop + 1) {
      return false;
    }

    const nextTop = clamp(targetTop, 0, maxScrollTop);
    node.scrollTop = nextTop;
    notifyMessageStackScrollWrite(node);
    paneScrollPositions[scrollStateKey] = {
      top: targetTop,
      shouldStick: false,
    };
    return true;
  }

  useLayoutEffect(() => {
    let restoreCleanup: (() => void) | undefined;
    const node = messageStackRef.current;
    if (!node) {
      return undefined;
    }

    const shouldForceBottomAfterWorkspaceRebuild =
      defaultScrollToBottom &&
      activeSession &&
      forceSessionScrollToBottomRef.current[activeSession.id];
    if (shouldForceBottomAfterWorkspaceRebuild) {
      delete forceSessionScrollToBottomRef.current[activeSession.id];
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
      node.scrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
      scrollMessageStackToBoundary("bottom");
      if (!node.querySelector(".virtualized-message-list")) {
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
        });
      }
    } else if (paneScrollPositions[scrollStateKey]) {
      const saved = paneScrollPositions[scrollStateKey];
      setTailFollowIntent(saved.shouldStick);
      if (saved.shouldStick) {
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      } else if (!restoreMessageStackScrollTop(saved.top)) {
        setTailFollowIntent(true);
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      }
    } else if (defaultScrollToBottom) {
      restoreCleanup = scheduleSettledScrollToBottom("auto", {
        maxAttempts: 60,
        preferVirtualizedBoundary: true,
      });
      setTailFollowIntent(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
    } else {
      node.scrollTop = 0;
      notifyMessageStackScrollWrite(node);
      setTailFollowIntent(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    }

    return () => {
      restoreCleanup?.();
    };
  }, [activeSession?.id, defaultScrollToBottom, scrollStateKey]);

  useLayoutEffect(() => {
    const previousByKey = previousShowWaitingIndicatorByKeyRef.current;
    const wasShowing = previousByKey[scrollStateKey] ?? false;

    if (!showWaitingIndicator) {
      previousByKey[scrollStateKey] = false;
      return;
    }

    if (
      deferContentScrollEffects ||
      !activeSession ||
      activeSession.hasNewerHistory === true ||
      !isActive ||
      !isSessionTabActive ||
      paneViewMode !== "session"
    ) {
      return;
    }

    if (wasShowing) {
      return;
    }

    previousByKey[scrollStateKey] = true;

    if (
      !getTailFollowIntent() &&
      !waitingIndicatorShouldStick &&
      !isMessageStackNearBottom()
    ) {
      return;
    }

    return scheduleSettledScrollToBottom("auto", {
      maxAttempts: 24,
      minAttempts: 4,
      preferVirtualizedBoundary: true,
      scrollKind: "bottom_follow",
    });
  }, [
    activeSession?.id,
    activeSession?.hasNewerHistory,
    deferContentScrollEffects,
    isActive,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
    showWaitingIndicator,
    waitingIndicatorShouldStick,
  ]);

  useLayoutEffect(() => {
    if (!hasSessionFindQuery || !activeSessionSearchMatch) {
      return;
    }

    const node =
      sessionSearchItemRefsRef.current[activeSessionSearchMatch.itemKey];
    if (!node) {
      return;
    }

    setTailFollowIntent(false);
    node.scrollIntoView({
      block: "center",
      behavior: "auto",
    });

    const container = messageStackRef.current;
    if (!container) {
      return;
    }
    notifyMessageStackScrollWrite(container);

    paneScrollPositions[scrollStateKey] = {
      top: container.scrollTop,
      shouldStick: false,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }, [
    activeSessionSearchMatch,
    hasSessionFindQuery,
    paneScrollPositions,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    if (
      deferContentScrollEffects ||
      !activeSession ||
      !isSessionTabActive ||
      paneViewMode !== "session" ||
      visitedSessionIds[activeSession.id]
    ) {
      return;
    }

    if (savedScrollShouldStick) {
      return;
    }

    return scheduleSettledScrollToBottom("auto", {
      preferVirtualizedBoundary: true,
    });
  }, [
    activeSession?.id,
    deferContentScrollEffects,
    isSessionTabActive,
    paneViewMode,
    savedScrollShouldStick,
    scrollStateKey,
    visitedSessionIds,
  ]);

  useEffect(() => {
    if (!activeSession?.id) {
      return;
    }

    setVisitedSessionIds((current) =>
      current[activeSession.id]
        ? current
        : {
            ...current,
            [activeSession.id]: true,
          },
    );
  }, [activeSession?.id]);

  useEffect(() => {
    const availableSessionIds = new Set(sessions.map((session) => session.id));
    setVisitedSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
  }, [sessions]);

  useEffect(() => {
    if (!activeSession || !isSessionTabActive) {
      return;
    }
    if (deferContentScrollEffects) {
      return;
    }

    const previousSignature = paneContentSignatures[scrollStateKey];
    const previousMessageContentSignature =
      paneMessageContentSignatures[scrollStateKey];
    if (previousSignature === visibleContentSignature) {
      return;
    }
    paneContentSignatures[scrollStateKey] = visibleContentSignature;
    paneMessageContentSignatures[scrollStateKey] =
      visibleMessageContentSignature;
    if (previousSignature === undefined) {
      const saved = paneScrollPositions[scrollStateKey];
      if (saved && !saved.shouldStick) {
        if (!restoreMessageStackScrollTop(saved.top)) {
          setTailFollowIntent(true);
          return scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
        }
        return;
      }
      if (getTailFollowIntent() || saved?.shouldStick) {
        return scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
          preferVirtualizedBoundary: true,
        });
      }
      return;
    }

    const onlyPendingPromptsChanged =
      paneViewMode === "session" &&
      showWaitingIndicator &&
      previousMessageContentSignature === visibleMessageContentSignature;
    if (onlyPendingPromptsChanged) {
      if (
        getTailFollowIntent() ||
        paneScrollPositions[scrollStateKey]?.shouldStick === true
      ) {
        setNewResponseIndicator(scrollStateKey, false);
        return scheduleSettledScrollToBottom("smooth", {
          maxAttempts: 24,
          minAttempts: 4,
          scrollKind: "bottom_follow",
        });
      }
      setNewResponseIndicator(scrollStateKey, true, "activity");
      return;
    }

    if (hasSessionFindQuery) {
      setTailFollowIntent(false);
      if (paneViewMode === "session") {
        setNewResponseIndicator(
          scrollStateKey,
          true,
          visibleLastMessageAuthor === "assistant" ? "response" : "activity",
        );
      }
      return;
    }

    const shouldScroll =
      getTailFollowIntent() ||
      paneScrollPositions[scrollStateKey]?.shouldStick === true;
    if (!shouldScroll) {
      if (paneViewMode === "session") {
        setNewResponseIndicator(
          scrollStateKey,
          true,
          visibleLastMessageAuthor === "assistant" ? "response" : "activity",
        );
      }
      return;
    }

    setNewResponseIndicator(scrollStateKey, false);
    return scheduleSettledScrollToBottom("smooth", {
      maxAttempts: 24,
      minAttempts: 4,
      scrollKind: "bottom_follow",
    });
  }, [
    activeSession?.id,
    deferContentScrollEffects,
    hasSessionFindQuery,
    isSessionTabActive,
    paneViewMode,
    scrollStateKey,
    showWaitingIndicator,
    visibleContentSignature,
    visibleLastMessageAuthor,
    visibleMessageContentSignature,
  ]);

  useEffect(() => {
    if (
      !pendingScrollToBottomRequest ||
      !isActive ||
      paneViewMode !== "session" ||
      activeSession?.id !== pendingScrollToBottomRequest.sessionId
    ) {
      return;
    }

    const requestToken = pendingScrollToBottomRequest.token;
    const node = messageStackRef.current;
    if (node?.querySelector(".virtualized-message-list")) {
      scrollMessageStackToBoundary("bottom");
      onScrollToBottomRequestHandled(requestToken);
      return undefined;
    }

    return scheduleSettledScrollToBottom("auto", {
      onComplete: () => {
        onScrollToBottomRequestHandled(requestToken);
      },
    });
  }, [
    activeSession?.id,
    isActive,
    onScrollToBottomRequestHandled,
    paneViewMode,
    pendingScrollToBottomRequest,
    scrollStateKey,
  ]);

  useEffect(() => {
    if (!isSending || paneViewMode !== "session") {
      return;
    }

    // A resident history window can end with a user-authored message, so its
    // last author is not evidence that a prompt was just submitted. Only the
    // explicit send transition may reattach a detached pane to the live tail.
    setNewResponseIndicator(scrollStateKey, false);
    let cleanup: (() => void) | undefined;
    const frameId = window.requestAnimationFrame(() => {
      cleanup = followLatestMessageForPromptSend();
    });

    return () => {
      window.cancelAnimationFrame(frameId);
      cleanup?.();
    };
  }, [isSending, paneViewMode, scrollStateKey]);

  return {
    handleConversationSearchItemMount,
    handleMessageStackScroll,
    handleMessageStackTouchStart,
    handleMessageStackUserScrollIntent,
    liveTailPinned,
    messageStackRef,
    newResponseIndicatorLabel,
    scrollMessageStackByPage,
    scrollMessageStackToBoundary,
    scrollSessionMessageStackByPageJump,
    showNewResponseIndicator,
  };
}

function isNestedEditablePageKeyTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  if (
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLInputElement ||
    target instanceof HTMLSelectElement
  ) {
    return true;
  }

  return (
    target.isContentEditable ||
    target.contentEditable === "true" ||
    target.getAttribute("contenteditable") === "" ||
    target.getAttribute("contenteditable") === "true"
  );
}
