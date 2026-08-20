// Owns synchronous LIVE TURN manual-detach presentation and FLIP compensation
// around transcript scrolling.
// Does not own tail-follow policy, saved scroll intent, or scheduling.
// Split from ui/src/SessionPaneView.scroll.ts.

const ACTIVE_ATTACHED_LIVE_TAIL_SELECTOR =
  '.session-conversation-page.is-active .conversation-live-tail[data-tail-follow="attached"]';
const ACTIVE_COMPENSATED_DETACHED_LIVE_TAIL_SELECTOR =
  ".session-conversation-page.is-active .conversation-live-tail[data-manual-detach-compensation]";
const COMPENSATED_DETACHED_LIVE_TAIL_SELECTOR =
  ".conversation-live-tail[data-manual-detach-compensation]";
const LIVE_TAIL_DETACH_OFFSET_PROPERTY =
  "--conversation-live-tail-detach-offset";

export function clearManualLiveTailDetachCompensation(
  node: HTMLElement | null,
) {
  const liveTails = node?.querySelectorAll<HTMLElement>(
    COMPENSATED_DETACHED_LIVE_TAIL_SELECTOR,
  );
  if (!liveTails?.length) {
    return;
  }
  for (const liveTail of liveTails) {
    liveTail.removeAttribute("data-manual-detach-compensation");
    liveTail.style.removeProperty(LIVE_TAIL_DETACH_OFFSET_PROPERTY);
  }
}

export function detachLiveTailPresentationBeforeManualScroll(
  node: HTMLElement,
) {
  const liveTail = node.querySelector<HTMLElement>(
    ACTIVE_ATTACHED_LIVE_TAIL_SELECTOR,
  );
  if (!liveTail) {
    return;
  }

  const attachedTop = liveTail.getBoundingClientRect().top;
  liveTail.setAttribute("data-tail-follow", "detached");
  const detachedTop = liveTail.getBoundingClientRect().top;
  const detachOffset = attachedTop - detachedTop;

  if (Math.abs(detachOffset) < 0.5) {
    return;
  }

  // Sticky presentation can be displaced from its in-flow slot while the
  // velocity-bounded bottom follow catches up with streaming growth. Preserve
  // that exact visual position across the detach; from the first native
  // scroll step onward this fixed transform travels with the transcript.
  liveTail.style.setProperty(
    LIVE_TAIL_DETACH_OFFSET_PROPERTY,
    `${detachOffset}px`,
  );
  liveTail.setAttribute("data-manual-detach-compensation", "");
}

export function releaseManualLiveTailDetachCompensationOutsideViewport(
  node: HTMLElement,
) {
  const liveTail = node.querySelector<HTMLElement>(
    ACTIVE_COMPENSATED_DETACHED_LIVE_TAIL_SELECTOR,
  );
  if (!liveTail) {
    return;
  }
  const viewportRect = node.getBoundingClientRect();
  const liveTailRect = liveTail.getBoundingClientRect();
  if (
    liveTailRect.bottom <= viewportRect.top ||
    liveTailRect.top >= viewportRect.bottom
  ) {
    clearManualLiveTailDetachCompensation(node);
  }
}
