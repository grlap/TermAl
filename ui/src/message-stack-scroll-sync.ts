export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";

export const MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS = 1200;

export type MessageStackScrollWriteKind =
  | "incremental"
  | "page_jump"
  | "seek"
  | "bottom_pin"
  | "bottom_boundary"
  | "bottom_follow";

export type MessageStackScrollWriteSource = "programmatic" | "user";

export type MessageStackScrollWriteDetail = {
  scrollKind?: MessageStackScrollWriteKind;
  scrollSource?: MessageStackScrollWriteSource;
};

// Shared seam between pane-owned transcript scroll intent and the virtualizer's
// reconciliation path. Producers normally dispatch this immediately after any
// direct message-stack `scrollTop` / `scrollTo` write. `bottom_pin` tells the
// virtualizer an already-sticky programmatic restore should mount the bottom
// range without the boundary reveal loop. `bottom_boundary` asks the virtualizer
// to mount the bottom range first, then perform the scroll after the target
// pages exist. `bottom_follow` marks a smooth programmatic follow; the pane and
// virtualizer keep bottom-stick state while native smooth scroll ticks pass
// through intermediate positions. `scrollSource: "user"` is reserved for direct
// pane writes that are synchronously caused by an input event; layout-effect
// restores and other programmatic writes should omit it so the virtualizer never
// calls `flushSync` from a React lifecycle.
export function notifyMessageStackScrollWrite(
  node: HTMLElement,
  detail?: MessageStackScrollWriteDetail,
) {
  node.dispatchEvent(
    new CustomEvent<MessageStackScrollWriteDetail>(
      MESSAGE_STACK_SCROLL_WRITE_EVENT,
      {
        detail,
      },
    ),
  );
}
