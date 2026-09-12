// Owns the current response's decorative cursor and text-end coordinates.
// Mounted outside StreamingMarkdownHeightGuard's measured body; never changes
// Markdown layout, message heights, scroll position, or bottom-follow state.
import { useLayoutEffect, useRef } from "react";

// Walk backwards so appends only measure the tail, not every previous line.
function lastTextRect(node: Node): DOMRect | null {
  if (
    node instanceof Element &&
    node.matches('button, [role="status"], [aria-hidden="true"], [hidden], script, style')
  ) return null;
  if (node.nodeType === Node.TEXT_NODE) {
    const end = node.textContent?.trimEnd().length ?? 0;
    if (!end) return null;
    const range = node.ownerDocument!.createRange();
    range.setStart(node, end - 1);
    range.setEnd(node, end);
    // Non-layout environments (including JSDOM) use the CSS fallback.
    const rect = range.getClientRects?.()[0];
    return rect && rect.height > 0 ? rect : null;
  }
  for (let child = node.lastChild; child; child = child.previousSibling) {
    const rect = lastTextRect(child);
    if (rect) return rect;
  }
  return null;
}

export function AssistantResponseProgress() {
  const indicatorRef = useRef<HTMLSpanElement>(null);

  useLayoutEffect(() => {
    const indicator = indicatorRef.current;
    const card = indicator?.closest<HTMLElement>(".bubble-assistant");
    const body = card?.querySelector<HTMLElement>(".streaming-markdown-height-content");
    if (!indicator || !card || !body) return;

    const position = () => {
      const tail = lastTextRect(body);
      if (!tail) {
        indicator.style.removeProperty("left");
        indicator.style.removeProperty("top");
        indicator.style.removeProperty("bottom");
        return;
      }
      const cardRect = card.getBoundingClientRect();
      const bodyRect = body.getBoundingClientRect();
      // DOM ranges use viewport coordinates; absolute offsets use the card's
      // padding box. Account for borders and a scaled/zoomed conversation.
      const scaleX = card.offsetWidth ? cardRect.width / card.offsetWidth : 1;
      const scaleY = card.offsetHeight ? cardRect.height / card.offsetHeight : 1;
      if (scaleX <= 0 || scaleY <= 0) return;
      const originX = cardRect.left + card.clientLeft * scaleX;
      const originY = cardRect.top + card.clientTop * scaleY;
      const start = (bodyRect.left - originX) / scaleX;
      const limit = Math.min(
        (bodyRect.right - originX) / scaleX,
        card.clientWidth,
      ) - indicator.offsetWidth;
      let left = (tail.right - originX) / scaleX + 6;
      // Range includes the font's descender area. Approximate the baseline at
      // 80% of its height and align the squares' bottom there, like an ellipsis.
      let top =
        (tail.top + tail.height * 0.8 - originY) / scaleY - indicator.offsetHeight;
      if (left > limit || left < start) {
        // A full line or horizontally scrolled code must not grow scrollWidth.
        // Use the existing bottom padding, aligned with the text, instead.
        left = start;
        top = (bodyRect.bottom - originY) / scaleY + 2;
      }
      left = Math.max(0, Math.min(left, limit));
      top = Math.max(0, Math.min(top, card.clientHeight - indicator.offsetHeight - 2));
      indicator.style.left = `${left}px`;
      indicator.style.top = `${top}px`;
      indicator.style.bottom = "auto";
    };

    position();
    // Coalesce deltas, font loads and resize notifications into one read/write
    // per frame. The indicator never resizes the body or writes scroll state.
    let frame: number | null = null;
    const schedulePosition = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        position();
      });
    };
    const resize = typeof ResizeObserver === "undefined"
      ? null : new ResizeObserver(schedulePosition);
    resize?.observe(body);
    resize?.observe(card);
    // Markdown can update independently (deferred rendering/highlighting).
    // Observe only its subtree, never the indicator's own style writes.
    const mutations = new MutationObserver(schedulePosition);
    mutations.observe(body, { childList: true, characterData: true, subtree: true });
    body.addEventListener("scroll", schedulePosition, true);
    document.fonts?.addEventListener("loadingdone", schedulePosition);
    return () => {
      resize?.disconnect();
      mutations.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
      body.removeEventListener("scroll", schedulePosition, true);
      document.fonts?.removeEventListener("loadingdone", schedulePosition);
    };
  }, []);

  return (
    <span
      ref={indicatorRef}
      className="assistant-response-progress"
      aria-hidden="true"
    >
      <span aria-hidden="true" />
      <span aria-hidden="true" />
      <span aria-hidden="true" />
    </span>
  );
}
