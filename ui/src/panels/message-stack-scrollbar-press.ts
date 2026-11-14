// Owns: telling a press on a message stack's native scrollbar apart (thumb,
// track or arrow button), the scrollTop a track press should go to, and a
// frozen copy of the stack's view shown over it while the reader drags the
// scrollbar. The browser moves a dragged scroller on its compositor thread
// before page code runs, so a view kept in place by page code alone shows
// blank frames during a fast drag; a static copy outside the scroller cannot.
// Does not own: history loading or seeks (the conversation history scroll
// decides when the copy is refreshed or removed), wheel or keyboard input.
// New module; not split out of an existing file.

export type ScrollbarPress =
  | { kind: "thumb" }
  | { kind: "arrow" }
  | { kind: "track"; scrollTop: number };

function scrollbarGeometry(node: HTMLElement) {
  const rect = node.getBoundingClientRect();
  const width = node.offsetWidth - node.clientWidth - node.clientLeft * 2;
  const top = rect.top + node.clientTop;
  // Classic scrollbars have an arrow button at each end, as tall as the bar
  // is wide; overlay scrollbars report no width and take no press here.
  const arrow = width;
  const trackTop = top + arrow;
  const trackLength = Math.max(node.clientHeight - arrow * 2, 1);
  const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
  const thumbLength = Math.min(
    Math.max(trackLength * (node.clientHeight / Math.max(node.scrollHeight, 1)), width),
    trackLength,
  );
  const travel = Math.max(trackLength - thumbLength, 1);
  const thumbTop = trackTop + (maxScrollTop > 0 ? (node.scrollTop / maxScrollTop) * travel : 0);
  return {
    left: rect.left + node.clientLeft + node.clientWidth,
    width,
    top,
    trackTop,
    trackLength,
    thumbTop,
    thumbLength,
    travel,
    maxScrollTop,
  };
}

// Classifies a primary-button press at the given client point; null when the
// press is not on the stack's vertical scrollbar.
export function classifyScrollbarPress(
  node: HTMLElement,
  clientX: number,
  clientY: number,
): ScrollbarPress | null {
  const bar = scrollbarGeometry(node);
  if (bar.width <= 0 || clientX < bar.left || clientX > bar.left + bar.width) {
    return null;
  }
  if (clientY < bar.trackTop || clientY > bar.trackTop + bar.trackLength) {
    return { kind: "arrow" };
  }
  if (clientY >= bar.thumbTop && clientY <= bar.thumbTop + bar.thumbLength) {
    return { kind: "thumb" };
  }
  // The thumb's centre goes to the pressed point, so the browser's own track
  // paging, which runs until the thumb reaches the pointer, has nothing left
  // to do.
  const fraction = (clientY - bar.trackTop - bar.thumbLength / 2) / bar.travel;
  return {
    kind: "track",
    scrollTop: Math.round(Math.min(Math.max(fraction, 0), 1) * bar.maxScrollTop),
  };
}

// A static copy of the stack's current view, placed over its content area
// (not its scrollbar, so the thumb stays visible). The pane's translucent
// background shows through the copy, so the live content under it is hidden
// instead (its layout, and so its measurements, are unchanged). `take`
// replaces the copy with the view as it is now; `release` removes it.
export function freezeMessageStackView(node: HTMLElement) {
  let copy: HTMLElement | null = null;
  let hidden: Array<{ element: HTMLElement; visibility: string }> = [];
  const hideLive = () => {
    if (hidden.length > 0) {
      return;
    }
    for (const child of node.children) {
      if (child instanceof HTMLElement) {
        hidden.push({ element: child, visibility: child.style.visibility });
        child.style.visibility = "hidden";
      }
    }
  };
  const showLive = () => {
    for (const { element, visibility } of hidden) {
      element.style.visibility = visibility;
    }
    hidden = [];
  };
  const take = () => {
    const parent = node.parentElement;
    if (!parent) {
      return;
    }
    // Copy the live view as shown (a refresh runs while it is hidden); both
    // style changes land before the next paint.
    showLive();
    const next = node.cloneNode(true) as HTMLElement;
    // The copy is presentation only: no ids, message identities, focus or
    // pointer targets that page code could confuse with the live stack.
    next.removeAttribute("id");
    for (const element of next.querySelectorAll<HTMLElement>("[id]")) {
      element.removeAttribute("id");
    }
    for (const element of next.querySelectorAll<HTMLElement>("[data-message-id]")) {
      element.removeAttribute("data-message-id");
    }
    next.setAttribute("aria-hidden", "true");
    next.inert = true;
    next.classList.add("message-stack-frozen-view");
    Object.assign(next.style, {
      position: "absolute",
      top: `${node.offsetTop}px`,
      left: `${node.offsetLeft}px`,
      width: `${node.clientLeft + node.clientWidth}px`,
      height: `${node.offsetHeight}px`,
      margin: "0",
      boxSizing: "border-box",
      overflow: "hidden",
      pointerEvents: "none",
      zIndex: "1",
    });
    parent.insertBefore(next, node.nextSibling);
    next.scrollTop = node.scrollTop;
    copy?.remove();
    copy = next;
    hideLive();
  };
  const release = () => {
    copy?.remove();
    copy = null;
    showLive();
  };
  return { take, release, isShown: () => copy !== null };
}
