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
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  notifyMessageStackScrollWrite,
  type MessageStackScrollWriteKind,
} from "./message-stack-scroll-sync";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import {
  resolveSettledScrollMinimumAttempts,
  syncMessageStackScrollPosition,
} from "./scroll-position";
import type { SessionSearchMatch } from "./session-find";
import type { Message, Session } from "./types";
import type { PaneViewMode } from "./workspace";

const SESSION_PAGE_JUMP_VIEWPORT_FACTOR = 0.45;

type NewResponseIndicatorKind = "activity" | "response";

type PaneScrollPosition = {
  top: number;
  shouldStick: boolean;
};

type UseSessionPaneScrollStateParams = {
  activeSession: Session | null;
  activeSessionSearchMatch: SessionSearchMatch | null;
  defaultScrollToBottom: boolean;
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

  const savedScrollPosition = paneScrollPositions[scrollStateKey];
  const savedScrollShouldStick = savedScrollPosition?.shouldStick === true;
  const waitingIndicatorShouldStick = savedScrollShouldStick;
  const newResponseIndicatorKind =
    newResponseIndicatorByKey[scrollStateKey] ?? null;
  const showNewResponseIndicator = newResponseIndicatorKind !== null;
  const newResponseIndicatorLabel =
    newResponseIndicatorKind === "activity" ? "New activity" : "New response";

  function getTailFollowIntent() {
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
  const liveTailPinned = tailFollowIntent;

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
    if (shouldStick) {
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

    if (direction < 0) {
      markTailFollowUserEscape();
    }
    const distance = Math.max(Math.round(node.clientHeight * 0.85), 160);
    node.scrollBy({
      top: distance * direction,
      behavior: "smooth",
    });
    notifyMessageStackScrollWrite(node, {
      scrollSource: "user",
    });
  }

  function scrollSessionMessageStackByPageJump(direction: -1 | 1) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const distance = Math.max(
      Math.round(node.clientHeight * SESSION_PAGE_JUMP_VIEWPORT_FACTOR),
      1,
    );
    scrollMessageStackByDelta(distance * direction, {
      scrollKind: "page_jump",
    });
  }

  function scrollMessageStackToBoundary(boundary: "top" | "bottom") {
    if (boundary === "bottom") {
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
      return;
    }

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
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
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
    if (shouldStick) {
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
      !activeSession ||
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
    activeSession,
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
    activeSession,
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
      if (paneViewMode === "session" && visibleLastMessageAuthor === "assistant") {
        setNewResponseIndicator(scrollStateKey, true);
      }
      return;
    }

    const shouldScroll =
      getTailFollowIntent() ||
      paneScrollPositions[scrollStateKey]?.shouldStick === true ||
      visibleLastMessageAuthor === "you";
    if (!shouldScroll) {
      if (paneViewMode === "session" && visibleLastMessageAuthor === "assistant") {
        setNewResponseIndicator(scrollStateKey, true);
      }
      return;
    }

    if (visibleLastMessageAuthor === "you") {
      setNewResponseIndicator(scrollStateKey, false);
      let cleanup: (() => void) | undefined;
      const frameId = window.requestAnimationFrame(() => {
        cleanup = followLatestMessageForPromptSend();
      });
      return () => {
        window.cancelAnimationFrame(frameId);
        cleanup?.();
      };
    }

    setNewResponseIndicator(scrollStateKey, false);
    return scheduleSettledScrollToBottom("smooth", {
      maxAttempts: 24,
      minAttempts: 4,
      scrollKind: "bottom_follow",
    });
  }, [
    activeSession,
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

    if (isMessageStackNearBottom()) {
      return;
    }

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
