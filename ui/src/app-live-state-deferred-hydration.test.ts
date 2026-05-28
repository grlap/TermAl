import { afterEach, describe, expect, it } from "vitest";

import { shouldDelayFullHydrationStartForComposer } from "./app-live-state-deferred-hydration";
import { CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES } from "./panels/conversation-composer-focus";

const appendedComposerDrafts: HTMLTextAreaElement[] = [];

function appendFocusedComposerDraft(value: string) {
  const composer = document.createElement("textarea");
  for (const [attribute, attributeValue] of Object.entries(
    CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES,
  )) {
    composer.setAttribute(attribute, attributeValue);
  }
  composer.value = value;
  document.body.appendChild(composer);
  appendedComposerDrafts.push(composer);
  composer.focus();
  return composer;
}

afterEach(() => {
  for (const composer of appendedComposerDrafts.splice(0)) {
    composer.remove();
  }
});

describe("shouldDelayFullHydrationStartForComposer", () => {
  it("delays ordinary full hydration while a composer draft is focused", () => {
    appendFocusedComposerDraft("still typing");

    expect(
      shouldDelayFullHydrationStartForComposer({
        sessionId: "session-1",
        sessionStillNeedsHydration: () => true,
        shouldStartTailFirstHydration: () => false,
      }),
    ).toBe(true);
  });

  it("does not delay queued recovery hydration behind the composer guard", () => {
    appendFocusedComposerDraft("still typing");

    expect(
      shouldDelayFullHydrationStartForComposer({
        sessionId: "session-1",
        options: { queueAfterCurrent: true },
        sessionStillNeedsHydration: () => true,
        shouldStartTailFirstHydration: () => false,
      }),
    ).toBe(false);
  });
});
