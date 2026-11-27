// Owns: the App-level regression for an ArrowUp after the reader clicks
// non-focusable transcript content while a virtualized transcript is attached
// at its physical bottom, with focus moving from the composer to the message
// stack or to document.body. The key must detach live follow at once, and no
// later measured layout change, repin or native scroll frame may return the
// view to the bottom.
// Does not own: hook-level keyboard ownership (SessionPaneView.scroll.keyboard
// .test.ts), focused-stack keyboard and live-turn follow scenarios
// (App.scroll-behavior.test.tsx), or prompt-send scrolling
// (App.send-scroll.test.tsx).
// New file; it uses the shared App and scroll fixtures that
// App.scroll-behavior.test.tsx uses.
import { act, fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { installAppScrollTestHarness } from "./App.scroll-behavior.fixtures";
import {
  dispatchStateEvent,
  flushUiWork,
  latestEventSource,
  makeSession,
  mockScrollToAndApplyTop,
  renderAppWithProjectAndSession,
  settleAsyncUi,
  stubElementScrollGeometry,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";
import { requestMessageStackBottomRepin } from "./message-stack-scroll-sync";
import { VIRTUALIZED_MESSAGE_GAP_PX } from "./panels/conversation-virtualization";
import { VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS } from "./panels/virtualized-conversation-controller";
import { SESSION_STICKY_BOTTOM_BAND_PX } from "./scroll-position";
import type { Session } from "./types";

const CLIENT_HEIGHT_PX = 200;
const SLOT_HEIGHT_PX = 80;
const ARROW_STEP_PX = 40;
const MEASURED_GROWTH_PX = 16;

// Records every observer so the test can deliver the measured page, slot and
// pane resizes that the pane's repin path and the virtualizer listen to.
const resizeCallbacks = new Set<ResizeObserverCallback>();
class RecordingResizeObserver {
  private readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
  }

  observe() {
    resizeCallbacks.add(this.callback);
  }

  unobserve() {}

  disconnect() {
    resizeCallbacks.delete(this.callback);
  }
}

function domRect(top: number, height: number): DOMRect {
  return {
    bottom: top + height,
    height,
    left: 0,
    right: 800,
    top,
    width: 800,
    x: 0,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

function assistantMessages(count: number): Session["messages"] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-assistant-${index + 1}`,
    type: "text",
    timestamp: "10:01",
    author: "assistant",
    text: `Assistant response ${index + 1}.`,
  }));
}

const outputBeforeEscape = {
  id: "message-appended-91",
  type: "text",
  timestamp: "10:02",
  author: "assistant",
  text: "Streamed output before the reader escapes.",
} as const;

const outputAfterEscape = {
  id: "message-appended-92",
  type: "text",
  timestamp: "10:03",
  author: "assistant",
  text: "Output that arrives after the reader escaped.",
} as const;

function stateWith(
  revision: number,
  messages: Session["messages"],
  preview: string,
) {
  return {
    revision,
    projects: [
      {
        id: "project-termal",
        name: "TermAl",
        rootPath: "/projects/termal",
      },
    ],
    sessions: [
      makeSession("session-1", {
        name: "Session 1",
        projectId: "project-termal",
        workdir: "/projects/termal",
        status: "active",
        preview,
        messages,
      }),
    ],
  };
}

// Where focus lands after a press on non-focusable message content. The
// message stack is focusable, so a browser normally focuses it; the pane also
// owns the keys when focus is left on document.body after a transcript press.
type FocusLanding = "message stack" | "document body";

describe("App ArrowUp detach from a virtualized live bottom", () => {
  installAppScrollTestHarness();

  it.each<FocusLanding>(["message stack", "document body"])("detaches on the first ArrowUp after a transcript click with focus on the %s and never snaps back to the bottom", async (focusLanding) => {
    await withVerifiedNoReactActWarnings(async () => {
      resizeCallbacks.clear();
      // One measured layout drives every reader: the stack's scrollHeight,
      // the active conversation page's height (the pane's resize baseline),
      // and the slot rectangles (the virtualizer's page measurement). Mounted
      // slots are laid out upward from the end of the content.
      let contentHeight = 1_000;
      let grownMessageId: string | null = null;
      let grownSlotHeight = SLOT_HEIGHT_PX;
      const slotHeight = (slot: HTMLElement) =>
        slot.dataset.messageId === grownMessageId
          ? grownSlotHeight
          : SLOT_HEIGHT_PX;
      const originalGetBoundingClientRect =
        Element.prototype.getBoundingClientRect;
      let restoreScrollGeometry: (() => void) | null = null;
      let restoreRects: (() => void) | null = null;
      // The pane's programmatic follow window and the virtualizer's
      // user-scroll cooldown read performance.now. A monotonic controlled
      // clock keeps the key inside the follow window and puts the measured
      // growth after the cooldown on every run, whatever the real timing.
      const realNow = performance.now.bind(performance);
      let clockOffset = 0;
      let frozenAt: number | null = null;
      const clockNow = () => frozenAt ?? realNow() + clockOffset;
      const freezeClock = () => {
        frozenAt = clockNow();
      };
      const resumeClock = () => {
        if (frozenAt !== null) {
          clockOffset = frozenAt - realNow();
          frozenAt = null;
        }
      };
      const advanceClock = (durationMs: number) => {
        clockOffset += durationMs;
      };
      let restoreClock: (() => void) | null = null;
      let context: Awaited<
        ReturnType<typeof renderAppWithProjectAndSession>
      > | null = null;

      try {
        const clockSpy = vi
          .spyOn(performance, "now")
          .mockImplementation(clockNow);
        restoreClock = () => clockSpy.mockRestore();
        restoreScrollGeometry = stubElementScrollGeometry({
          clientHeight: CLIENT_HEIGHT_PX,
          scrollHeight: () => contentHeight,
        });
        const rectSpy = vi.spyOn(Element.prototype, "getBoundingClientRect");
        restoreRects = () => rectSpy.mockRestore();
        rectSpy.mockImplementation(
          function (this: Element) {
            if (!(this instanceof HTMLElement)) {
              return originalGetBoundingClientRect.call(this);
            }
            const stack = this.closest(".message-stack");
            if (!(stack instanceof HTMLElement)) {
              return originalGetBoundingClientRect.call(this);
            }
            if (this === stack) {
              return domRect(0, CLIENT_HEIGHT_PX);
            }
            if (this.classList.contains("session-conversation-page")) {
              return domRect(-stack.scrollTop, contentHeight);
            }
            if (this.classList.contains("virtualized-message-slot")) {
              const slots = Array.from(
                stack.querySelectorAll<HTMLElement>(".virtualized-message-slot"),
              );
              let bottom = contentHeight;
              for (let index = slots.length - 1; index >= 0; index -= 1) {
                const slot = slots[index]!;
                const height = slotHeight(slot);
                const top = bottom - height;
                if (slot === this) {
                  return domRect(top - stack.scrollTop, height);
                }
                bottom = top - VIRTUALIZED_MESSAGE_GAP_PX;
              }
            }
            return originalGetBoundingClientRect.call(this);
          },
        );
        mockScrollToAndApplyTop();
        context = await renderAppWithProjectAndSession({
          resizeObserver:
            RecordingResizeObserver as unknown as typeof ResizeObserver,
        });
        const messages = assistantMessages(90);

        await dispatchStateEvent(
          latestEventSource(),
          stateWith(2, messages, "Assistant response 90."),
        );
        for (let iteration = 0; iteration < 10; iteration += 1) {
          await settleAsyncUi();
        }

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }
        expect(
          messageStack.querySelector(".virtualized-message-list"),
        ).not.toBeNull();
        const bottomTop = () => contentHeight - CLIENT_HEIGHT_PX;

        // The reader is attached at the physical bottom.
        messageStack.scrollTop = bottomTop();
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        expect(messageStack).toHaveClass("is-tail-following");

        // New output arrives and the pane follows it with a programmatic
        // bottom write. The clock stays still from here until the key's
        // native frame, so the key lands inside that follow window.
        freezeClock();
        contentHeight = 1_120;
        await dispatchStateEvent(
          latestEventSource(),
          stateWith(
            3,
            [...messages, outputBeforeEscape],
            outputBeforeEscape.text,
          ),
        );
        await settleAsyncUi();
        expect(messageStack.scrollTop).toBe(bottomTop());
        expect(messageStack).toHaveClass("is-tail-following");

        // The reader is typing in the composer before turning to the
        // transcript.
        const composer = await screen.findByLabelText("Message Session 1");
        act(() => {
          composer.focus();
        });
        expect(document.activeElement).toBe(composer);

        // From here on, record every scrollTop the stack takes.
        const scrollTopHistory: number[] = [];
        const nativeScrollTop = Object.getOwnPropertyDescriptor(
          Element.prototype,
          "scrollTop",
        );
        if (!nativeScrollTop?.get || !nativeScrollTop.set) {
          throw new Error("Element scrollTop accessor not found");
        }
        Object.defineProperty(messageStack, "scrollTop", {
          configurable: true,
          get() {
            return nativeScrollTop.get!.call(this);
          },
          set(value: number) {
            nativeScrollTop.set!.call(this, value);
            scrollTopHistory.push(nativeScrollTop.get!.call(this));
          },
        });

        // A press on non-focusable transcript content takes focus off the
        // composer. jsdom dispatches the mousedown but does not run the
        // browser's default focus change, so the test applies that change
        // after the event, as the browser does, and keys then go to whatever
        // holds focus.
        const messageSlot = messageStack.querySelector(
          ".virtualized-message-slot",
        );
        if (!(messageSlot instanceof HTMLElement)) {
          throw new Error("Virtualized message slot not found");
        }
        const keyTarget =
          focusLanding === "message stack" ? messageStack : document.body;
        act(() => {
          fireEvent.mouseDown(messageSlot);
          if (focusLanding === "message stack") {
            messageStack.focus();
          } else {
            composer.blur();
          }
          fireEvent.mouseUp(messageSlot);
        });
        expect(document.activeElement).toBe(keyTarget);

        const firstArrowUp = new KeyboardEvent("keydown", {
          bubbles: true,
          cancelable: true,
          key: "ArrowUp",
        });
        act(() => {
          keyTarget.dispatchEvent(firstArrowUp);
        });
        const afterFirstKey = bottomTop() - ARROW_STEP_PX;
        expect(firstArrowUp.defaultPrevented).toBe(true);
        expect(messageStack.scrollTop).toBe(afterFirstKey);
        expect(messageStack).not.toHaveClass("is-tail-following");

        // The native frame for that write. Then time moves on past the
        // virtualizer's user-scroll cooldown, so a measured layout change is
        // applied at once rather than deferred.
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        resumeClock();
        advanceClock(VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS + 1);

        // The newest message card finishes measuring slightly taller below the
        // reader's position: the conversation page and the newest slot both
        // grow, and the reader stays inside the sticky bottom band, where a
        // stale bottom owner would snap back. Every observer is delivered and
        // the virtualizer's queued measurement frame is flushed before the
        // next native scroll frame.
        grownMessageId = outputBeforeEscape.id;
        grownSlotHeight = SLOT_HEIGHT_PX + MEASURED_GROWTH_PX;
        contentHeight += MEASURED_GROWTH_PX;
        expect(
          contentHeight - CLIENT_HEIGHT_PX - messageStack.scrollTop,
        ).toBeLessThan(SESSION_STICKY_BOTTOM_BAND_PX);
        await act(async () => {
          resizeCallbacks.forEach((callback) =>
            callback([], {} as ResizeObserver),
          );
          await flushUiWork();
        });
        await act(async () => {
          requestMessageStackBottomRepin(messageStack);
          await flushUiWork();
        });
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(messageStack.scrollTop).toBe(afterFirstKey);
        expect(messageStack).not.toHaveClass("is-tail-following");

        const secondArrowUp = new KeyboardEvent("keydown", {
          bubbles: true,
          cancelable: true,
          key: "ArrowUp",
        });
        act(() => {
          keyTarget.dispatchEvent(secondArrowUp);
        });
        const afterSecondKey = afterFirstKey - ARROW_STEP_PX;
        expect(secondArrowUp.defaultPrevented).toBe(true);
        expect(messageStack.scrollTop).toBe(afterSecondKey);

        // More output arrives while the reader is detached.
        contentHeight += SLOT_HEIGHT_PX + VIRTUALIZED_MESSAGE_GAP_PX;
        await dispatchStateEvent(
          latestEventSource(),
          stateWith(
            4,
            [...messages, outputBeforeEscape, outputAfterEscape],
            outputAfterEscape.text,
          ),
        );
        await act(async () => {
          resizeCallbacks.forEach((callback) =>
            callback([], {} as ResizeObserver),
          );
          await flushUiWork();
        });
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        await settleAsyncUi();
        expect(messageStack.scrollTop).toBe(afterSecondKey);
        expect(messageStack).not.toHaveClass("is-tail-following");

        // No write after the first key moved the view back toward the bottom
        // past the position that key reached.
        expect(
          scrollTopHistory.filter((top) => top > afterFirstKey),
        ).toEqual([]);
      } finally {
        context?.cleanup();
        restoreScrollGeometry?.();
        restoreRects?.();
        restoreClock?.();
      }
    });
  });
});
