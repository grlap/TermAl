export const CONVERSATION_COMPOSER_INPUT_DATASET_KEY =
  "conversationComposerInput";

export const CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES = {
  "data-conversation-composer-input": "true",
} as const;

export function isConversationComposerInputElement(
  element: Element | null,
): element is HTMLTextAreaElement {
  if (typeof window === "undefined") {
    return false;
  }
  if (!(element instanceof window.HTMLTextAreaElement)) {
    return false;
  }
  return element.dataset[CONVERSATION_COMPOSER_INPUT_DATASET_KEY] === "true";
}

export function activeConversationComposerHasDraftText() {
  if (typeof document === "undefined") {
    return false;
  }
  const activeElement = document.activeElement;
  if (!isConversationComposerInputElement(activeElement)) {
    return false;
  }
  return activeElement.value.length > 0;
}
