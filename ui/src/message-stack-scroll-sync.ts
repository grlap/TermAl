export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";
export const MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT =
  "termal:message-stack-bottom-repin-request";

export const MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS = 1200;

export type MessageStackScrollWriteKind =
  | "incremental"
  | "page_jump"
  | "seek"
  | "position_restore"
  | "bottom_pin"
  | "bottom_boundary"
  | "bottom_follow";

export type MessageStackScrollWriteSource = "programmatic" | "user";

export type MessageStackScrollWriteDetail = {
  scrollKind?: MessageStackScrollWriteKind;
  scrollSource?: MessageStackScrollWriteSource;
};

export type MessageStackBottomRepinRequestDetail = {
  authorityPresent: boolean;
  beforePaint: boolean;
};

export type MessageStackBottomRepinRequestOptions = {
  beforePaint?: boolean;
};

// Shared seam between pane-owned transcript scroll intent and the virtualizer's
// reconciliation path. Producers normally dispatch this immediately after any
// direct message-stack `scrollTop` / `scrollTo` write. `bottom_pin` tells the
// virtualizer an already-sticky programmatic restore should mount the bottom
// range without the boundary reveal loop. `bottom_boundary` asks the virtualizer
// to mount the bottom range first, then perform the scroll after the target
// pages exist. `bottom_follow` marks the pane's frame-by-frame programmatic
// follow; the pane and virtualizer keep bottom-stick state while those bounded
// writes pass through intermediate positions. `position_restore` preserves an
// explicitly detached saved viewport while virtualized geometry converges.
// `scrollSource: "user"` is reserved for direct
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

// Synchronous request seam for layout owners that need to preserve an existing
// bottom pin without becoming another transcript scroll writer. Requests may
// start on the message stack or bubble from a descendant layout owner;
// SessionPaneView handles them through its scroll authority, including explicit
// tail-follow intent, saved pane position, and virtualizer notification.
export function requestMessageStackBottomRepin(
  node: HTMLElement,
  options: MessageStackBottomRepinRequestOptions = {},
) {
  const detail: MessageStackBottomRepinRequestDetail = {
    authorityPresent: false,
    beforePaint: options.beforePaint === true,
  };
  node.dispatchEvent(
    new CustomEvent<MessageStackBottomRepinRequestDetail>(
      MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
      { bubbles: true, detail },
    ),
  );
  return detail.authorityPresent;
}
