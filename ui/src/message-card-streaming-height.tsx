// Layout guard for the actively streaming assistant message body.
//
// What this file owns:
//   - preserving small, transient Markdown reparse shrinks so a bottom-pinned
//     transcript never paints an up/down oscillation;
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

    if (!active) {
      const shouldRepin = wasActiveRef.current && maximumHeightRef.current > 0;
      wasActiveRef.current = false;
      maximumHeightRef.current = 0;
      floor.style.minHeight = "";
      if (shouldRepin) {
        requestMessageStackBottomRepin(floor, { beforePaint: true });
      }
      return;
    }

    // The effect re-runs only when the message identity changes or streaming
    // settles. Token-by-token Markdown updates keep the same guard and floor.
    wasActiveRef.current = true;
    maximumHeightRef.current = 0;
    floor.style.minHeight = "";

    const preserveTransientShrink = () => {
      const measuredHeight = Math.ceil(content.getBoundingClientRect().height);
      if (!Number.isFinite(measuredHeight) || measuredHeight <= 0) {
        return;
      }

      const previousMaximum = maximumHeightRef.current;
      if (measuredHeight >= previousMaximum) {
        maximumHeightRef.current = measuredHeight;
        floor.style.minHeight = `${measuredHeight}px`;
        return;
      }

      if (previousMaximum - measuredHeight <= MAX_TRANSIENT_STREAMING_SHRINK_PX) {
        return;
      }

      // A large collapse is real layout (for example a completed Mermaid or
      // table), not the short parser wobble this guard exists to mask. Adopt
      // it immediately and let the pane authority preserve the bottom pin.
      maximumHeightRef.current = measuredHeight;
      floor.style.minHeight = `${measuredHeight}px`;
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
