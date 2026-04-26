export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";

export type MessageStackScrollWriteKind =
  | "incremental"
  | "page_jump"
  | "seek"
  | "bottom_boundary"
  | "bottom_follow";

export type MessageStackScrollWriteDetail = {
  scrollKind?: MessageStackScrollWriteKind;
};

// Shared seam between pane-owned transcript scroll intent and the virtualizer's
// reconciliation path. Producers normally dispatch this immediately after any
// direct message-stack `scrollTop` / `scrollTo` write. `bottom_boundary` is the
// exception: it asks the virtualizer to mount the bottom range first, then
// perform the scroll after the target pages exist.
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
