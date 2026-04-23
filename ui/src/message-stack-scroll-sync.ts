export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";

export type MessageStackScrollWriteKind =
  | "incremental"
  | "page_jump"
  | "seek";

export type MessageStackScrollWriteDetail = {
  scrollKind?: MessageStackScrollWriteKind;
};

// Shared seam between pane-owned programmatic transcript scroll writes and the
// virtualizer's reconciliation path. Producers must dispatch this immediately
// after any direct message-stack `scrollTop` / `scrollTo` write so the
// virtualizer can update its bookkeeping without waiting for a native scroll
// event. Provide `detail.scrollKind` for keyboard-owned seek jumps where raw
// delta size alone is not authoritative.
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
