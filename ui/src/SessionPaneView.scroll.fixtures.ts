// Owns shared hook inputs and animation-frame fixtures for scroll tests.
// Does not own assertions or production scroll policy.
// Split from SessionPaneView.scroll.test.ts.
import { act } from "@testing-library/react";
import { vi } from "vitest";
import type { Session } from "./types";

export function session(hasNewerHistory: boolean): Session {
  return {
    id: "session-history",
    name: "History",
    emoji: "H",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt",
    status: "active",
    preview: "",
    messages: [
      {
        id: hasNewerHistory ? "message-64" : "message-1000",
        type: "text",
        timestamp: "12:00",
        author: "assistant",
        text: hasNewerHistory ? "Historical message" : "Live tail message",
      },
    ],
    messagesLoaded: false,
    hasOlderHistory: !hasNewerHistory,
    hasNewerHistory,
    messageCount: 1_000,
  };
}

export function params(activeSession: Session) {
  return {
    activeSession,
    activeSessionSearchMatch: null,
    defaultScrollToBottom: false,
    deferContentScrollEffects: false,
    hasSessionFindQuery: false,
    isActive: false,
    isSending: false,
    isSessionTabActive: false,
    onScrollToBottomRequestHandled: vi.fn(),
    paneContentSignatures: {},
    paneMessageContentSignatures: {},
    paneRootRef: { current: null },
    paneScrollPositions: {},
    paneShouldStickToBottomRef: { current: {} },
    paneViewMode: "session" as const,
    pendingScrollToBottomRequest: null,
    scrollStateKey: "pane-1:session-history",
    showWaitingIndicator: false,
    visibleContentSignature: "history",
    visibleLastMessageAuthor: "assistant" as const,
    visibleMessageContentSignature: "history-message",
  };
}

export function installAnimationFrameHarness(frameDurationMs?: number) {
  let nextAnimationFrameId = 1;
  const animationFrames = new Map<number, FrameRequestCallback>();
  const requestAnimationFrame = vi.fn((callback: FrameRequestCallback) => {
    const frameId = nextAnimationFrameId;
    nextAnimationFrameId += 1;
    animationFrames.set(frameId, callback);
    return frameId;
  });
  vi.stubGlobal("requestAnimationFrame", requestAnimationFrame);
  vi.stubGlobal(
    "cancelAnimationFrame",
    vi.fn((frameId: number) => animationFrames.delete(frameId)),
  );
  const drainAnimationFrames = () => {
    let drainCount = 0;
    let frameTimestamp = performance.now();
    while (animationFrames.size > 0) {
      drainCount += 1;
      if (drainCount > 50) {
        throw new Error(
          `animation frame drain exceeded 50 rounds with ${animationFrames.size} callbacks pending`,
        );
      }
      const callbacks = Array.from(animationFrames.values());
      animationFrames.clear();
      // Time-bounded follow needs advancing frame timestamps, not a tight loop
      // of almost identical performance.now() readings.
      frameTimestamp = frameDurationMs === undefined
        ? performance.now()
        : frameTimestamp + frameDurationMs;
      act(() => {
        callbacks.forEach((callback) => callback(frameTimestamp));
      });
    }
  };
  return { animationFrames, drainAnimationFrames, requestAnimationFrame };
}
