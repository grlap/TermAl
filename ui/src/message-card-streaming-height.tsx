// Layout guard for the actively streaming assistant message body.
//
// What this file owns:
//   - preserving small, transient Markdown reparse shrinks so a bottom-pinned
//     transcript never paints an up/down oscillation;
//   - retaining that floor through stable post-stream measurements so final
//     rendering cannot reverse the viewport twice across adjacent frames;
//   - releasing genuinely large layout changes immediately and asking the
//     pane-owned scroll authority to repin before paint;
//   - resetting the floor when the active message identity changes.
//
// What this file does not own:
//   - Markdown parsing/rendering;
//   - direct message-stack lookup or scroll writes. Repin requests bubble from
//     this card-local layout owner to SessionPaneView's single scroll writer.

import { useLayoutEffect, useRef, type ReactNode } from "react";
import { requestMessageStackBottomRepin } from "./message-stack-scroll-sync";

const MAX_TRANSIENT_STREAMING_SHRINK_PX = 96;
const SETTLED_HEIGHT_STABLE_FRAME_COUNT = 2;
const SETTLED_HEIGHT_MAX_FRAME_COUNT = 12;
const HELD_STREAMING_HEIGHT_CLASS = "is-holding-streaming-height";

export function StreamingMarkdownHeightGuard({
  active,
  children,
  streamKey,
}: {
  active: boolean;
  children: ReactNode;
  streamKey: string;
}) {
  const floorRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const maximumHeightRef = useRef(0);
  const wasActiveRef = useRef(false);

  useLayoutEffect(() => {
    const floor = floorRef.current;
    const content = contentRef.current;
    if (!floor || !content) {
      return;
    }

    const clearHeldHeight = () => {
      maximumHeightRef.current = 0;
      floor.style.minHeight = "";
      floor.classList.remove(HELD_STREAMING_HEIGHT_CLASS);
    };
    const holdHeight = (height: number) => {
      maximumHeightRef.current = height;
      floor.style.minHeight = `${height}px`;
      floor.classList.add(HELD_STREAMING_HEIGHT_CLASS);
    };

    if (!active) {
      const shouldRepin = wasActiveRef.current && maximumHeightRef.current > 0;
      wasActiveRef.current = false;
      if (!shouldRepin) {
        clearHeldHeight();
        return;
      }

      // The settled Markdown tree can finish its final reparse one or two
      // frames after the streaming flag clears. Releasing the floor in the
      // transition commit lets the browser clamp the pinned viewport upward;
      // a later final measurement then makes bottom-follow move it down again.
      // Keep the last streaming floor until the inner content reports the same
      // final height in two consecutive frames. ResizeObserver invalidates the
      // streak when syntax highlighting or another late renderer changes it.
      let cancelled = false;
      let frameId: number | null = null;
      let previousSettledHeight: number | null = null;
      let stableFrameCount = 0;
      let totalFrameCount = 0;

      const releaseSettledFloor = () => {
        if (cancelled) {
          return;
        }
        clearHeldHeight();
        requestMessageStackBottomRepin(floor, { beforePaint: true });
      };

      const measureSettledHeight = () => {
        frameId = null;
        if (cancelled) {
          return;
        }
        totalFrameCount += 1;

        const measuredHeight = Math.ceil(
          content.getBoundingClientRect().height,
        );
        if (!Number.isFinite(measuredHeight) || measuredHeight <= 0) {
          releaseSettledFloor();
          return;
        }

        if (measuredHeight > maximumHeightRef.current) {
          holdHeight(measuredHeight);
        }
        if (measuredHeight === previousSettledHeight) {
          stableFrameCount += 1;
        } else {
          previousSettledHeight = measuredHeight;
          stableFrameCount = 1;
        }

        if (
          stableFrameCount >= SETTLED_HEIGHT_STABLE_FRAME_COUNT ||
          totalFrameCount >= SETTLED_HEIGHT_MAX_FRAME_COUNT
        ) {
          releaseSettledFloor();
          return;
        }
        frameId = window.requestAnimationFrame(measureSettledHeight);
      };

      const ResizeObserverCtor = globalThis.ResizeObserver;
      const observer =
        typeof ResizeObserverCtor === "function"
          ? new ResizeObserverCtor(() => {
              previousSettledHeight = null;
              stableFrameCount = 0;
            })
          : null;
      observer?.observe(content);
      frameId = window.requestAnimationFrame(measureSettledHeight);
      return () => {
        cancelled = true;
        if (frameId !== null) {
          window.cancelAnimationFrame(frameId);
        }
        observer?.disconnect();
      };
    }

    // The effect re-runs only when the message identity changes or streaming
    // settles. Token-by-token Markdown updates keep the same guard and floor.
    wasActiveRef.current = true;
    clearHeldHeight();

    const preserveTransientShrink = () => {
      const measuredHeight = Math.ceil(content.getBoundingClientRect().height);
      if (!Number.isFinite(measuredHeight) || measuredHeight <= 0) {
        return;
      }

      const previousMaximum = maximumHeightRef.current;
      if (measuredHeight >= previousMaximum) {
        holdHeight(measuredHeight);
        return;
      }

      if (
        previousMaximum - measuredHeight <=
        MAX_TRANSIENT_STREAMING_SHRINK_PX
      ) {
        return;
      }

      // A large collapse is real layout (for example a completed Mermaid or
      // table), not the short parser wobble this guard exists to mask. Adopt
      // it immediately and let the pane authority preserve the bottom pin.
      holdHeight(measuredHeight);
      requestMessageStackBottomRepin(floor, { beforePaint: true });
    };

    preserveTransientShrink();
    const ResizeObserverCtor = globalThis.ResizeObserver;
    const observer =
      typeof ResizeObserverCtor === "function"
        ? new ResizeObserverCtor(preserveTransientShrink)
        : null;
    observer?.observe(content);
    return () => observer?.disconnect();
  }, [active, streamKey]);

  return (
    <div ref={floorRef} className="streaming-markdown-height-floor">
      <div ref={contentRef} className="streaming-markdown-height-content">
        {children}
      </div>
    </div>
  );
}
