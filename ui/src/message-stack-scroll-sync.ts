export const MESSAGE_STACK_SCROLL_WRITE_EVENT =
  "termal:message-stack-scroll-write";

export function notifyMessageStackScrollWrite(node: HTMLElement) {
  node.dispatchEvent(new Event(MESSAGE_STACK_SCROLL_WRITE_EVENT));
}
