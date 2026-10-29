// Owns the full-content provenance tests of the virtualized page-height owner:
// that every measurement recomputes provenance, the single-query scan, its
// equivalence with the per-slot algorithm it replaced, and the reuse of an
// unchanged provenance array and measured identity.
// Does not own the commit-order and scroll tests, which stay in
// virtualized-conversation-page-heights.test.tsx. New file beside that one;
// not split out of it.
import { render } from "@testing-library/react";
import { useLayoutEffect, useRef, type MutableRefObject } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  collectFullyRenderedMessageIds,
  useVirtualizedConversationPageHeightChange,
} from "./virtualized-conversation-page-heights";
import type { PageMeasurementIdentity } from "./virtualized-conversation-measurement";
import type { Message } from "../types";

afterEach(() => {
  vi.restoreAllMocks();
});

type Measure = (
  pageKey: string,
  pageIndex: number,
  nextHeight: number,
  pageNode?: HTMLElement | null,
) => void;

type ProvenanceHandles = {
  currentIdentities: MutableRefObject<Record<string, PageMeasurementIdentity>>;
  measure: Measure;
  measuredIdentities: Record<string, PageMeasurementIdentity>;
};

const PAGE_MESSAGES: readonly Message[] = [];

function ProvenanceHarness({
  onReady,
}: {
  onReady: (handles: ProvenanceHandles) => void;
}) {
  const measuredIdentities = useRef<Record<string, PageMeasurementIdentity>>({});
  const currentIdentities = useRef<Record<string, PageMeasurementIdentity>>({
    "page-0": { hasTrailingGap: false, messages: PAGE_MESSAGES },
  });
  const measure = useVirtualizedConversationPageHeightChange({
    bumpLayoutVersion: vi.fn(),
    clearPendingDeferredBottomRestore: vi.fn(),
    clearPendingDeferredLayoutTimer: vi.fn(),
    hasUserScrollInteractionRef: useRef(false),
    isActive: true,
    isDetachedFromBottomRef: useRef(false),
    isMessagePrependCommitRef: useRef(false),
    lastNativeScrollTopRef: useRef(0),
    lastUserScrollInputTimeRef: useRef(0),
    latestVisibleMessageAnchorRef: useRef(null),
    layoutPageHeightsRef: useRef({}),
    measuredPageIdentityRef: measuredIdentities,
    pageHeightsRef: useRef({}),
    currentPageIdentityRef: currentIdentities,
    renderedListRef: useRef(null),
    scheduleDeferredBottomRestoreLayoutVersion: vi.fn(),
    scheduleDeferredLayoutVersion: vi.fn(),
    scrollContainerRef: useRef(null),
    shouldKeepBottomAfterLayoutRef: useRef(false),
    userScrollAdjustmentCooldownMs: 0,
    visiblePageRangeRef: useRef({ startIndex: 0, endIndex: 1 }),
    writeScrollTopAndSyncViewport: vi.fn(),
  });
  useLayoutEffect(
    () =>
      onReady({
        currentIdentities,
        measure,
        measuredIdentities: measuredIdentities.current,
      }),
    [measure, onReady],
  );
  return null;
}

function renderProvenanceHarness(): ProvenanceHandles {
  let handles: ProvenanceHandles | null = null;
  render(
    <ProvenanceHarness
      onReady={(ready) => {
        handles = ready;
      }}
    />,
  );
  if (!handles) {
    throw new Error("the provenance harness did not mount");
  }
  return handles;
}

function pageFromHtml(html: string) {
  const page = document.createElement("div");
  page.className = "virtualized-message-page";
  page.innerHTML = html;
  return page;
}

const slot = (id: string, inner = "") =>
  `<div class="virtualized-message-slot" data-message-id="${id}">${inner}</div>`;
const pending = (inner = "") =>
  `<div data-deferred-content-pending="true">${inner}</div>`;

/** The per-slot algorithm this owner used before the single-query scan. */
function perSlotReference(pageNode: Element) {
  return Array.from(
    pageNode.querySelectorAll<HTMLElement>(
      ".virtualized-message-slot[data-message-id]",
    ),
  )
    .filter((slotNode) => !slotNode.querySelector('[data-deferred-content-pending="true"]'))
    .map((slotNode) => slotNode.dataset.messageId!);
}

const EQUIVALENCE_FIXTURES: Record<string, { html: string; expected: string[] }> = {
  "an empty page": { html: "", expected: [] },
  "fully rendered and deeply pending slots": {
    html:
      slot("a") +
      slot("b", `<section><p>${pending("<span></span>")}</p></section>`) +
      slot("c", "<p>done</p>"),
    expected: ["a", "c"],
  },
  "several markers in one slot": {
    html: slot("a", pending() + pending()) + slot("b"),
    expected: ["b"],
  },
  "a marker outside every slot": {
    html: pending() + slot("a") + `<div>${pending()}</div>` + slot("b"),
    expected: ["a", "b"],
  },
  "a marker on the slot element itself": {
    html:
      '<div class="virtualized-message-slot" data-message-id="a" ' +
      'data-deferred-content-pending="true"></div>' +
      slot("b"),
    expected: ["a", "b"],
  },
  "a marker with another value": {
    html: slot("a", '<div data-deferred-content-pending="false"></div>') + slot("b"),
    expected: ["a", "b"],
  },
  "a marker under a nested slot excludes both slots": {
    html: slot("outer", slot("inner", pending())) + slot("c"),
    expected: ["c"],
  },
  "a marker in the outer slot only keeps the nested slot": {
    html: slot("outer", pending() + slot("inner")) + slot("c"),
    expected: ["inner", "c"],
  },
  "slots without a message id are not listed": {
    html:
      `<div class="virtualized-message-slot">${pending()}</div>` +
      '<div class="virtualized-message-slot"></div>' +
      slot("a"),
    expected: ["a"],
  },
};

describe("full-content provenance of page measurements", () => {
  it.each(Object.entries(EQUIVALENCE_FIXTURES))(
    "matches the per-slot algorithm for %s",
    (_name, { html, expected }) => {
      const page = pageFromHtml(html);
      expect(perSlotReference(page)).toEqual(expected);
      expect(collectFullyRenderedMessageIds(page)).toEqual(expected);
    },
  );

  it("reuses an equal previous result and replaces a different one", () => {
    const page = pageFromHtml(slot("a") + slot("b", pending()) + slot("c"));
    const equal = ["a", "c"];
    expect(collectFullyRenderedMessageIds(page, equal)).toBe(equal);

    const longer = ["a", "c", "d"];
    const fromLonger = collectFullyRenderedMessageIds(page, longer);
    expect(fromLonger).toEqual(["a", "c"]);
    expect(fromLonger).not.toBe(longer);

    const different = ["a", "b"];
    const fromDifferent = collectFullyRenderedMessageIds(page, different);
    expect(fromDifferent).toEqual(["a", "c"]);
    expect(different).toEqual(["a", "b"]);

    const shorter = ["a"];
    expect(collectFullyRenderedMessageIds(page, shorter)).toEqual(["a", "c"]);
    expect(shorter).toEqual(["a"]);
  });

  it("removes a slot's id at the same height once it becomes pending again", () => {
    const { measure, measuredIdentities } = renderProvenanceHarness();
    const page = pageFromHtml(slot("a") + slot("b"));

    measure("page-0", 0, 100, page);
    expect(measuredIdentities["page-0"]!.fullyRenderedMessageIds).toEqual(["a", "b"]);

    page
      .querySelector('[data-message-id="b"]')!
      .insertAdjacentHTML("beforeend", pending());
    measure("page-0", 0, 100, page);
    expect(measuredIdentities["page-0"]!.fullyRenderedMessageIds).toEqual(["a"]);
  });

  it("scans the page with one combined query and no per-slot query", () => {
    const { measure } = renderProvenanceHarness();
    const page = pageFromHtml(
      slot("a", "<p>full</p>") +
        slot("b", `<section>${pending()}</section>`) +
        slot("c", "<p>full</p>") +
        slot("d", pending()),
    );
    const insidePage = (node: unknown) =>
      node instanceof Node && (node === page || page.contains(node));
    const querySelectorAll = vi.spyOn(Element.prototype, "querySelectorAll");
    const querySelector = vi.spyOn(Element.prototype, "querySelector");

    measure("page-0", 0, 100, page);

    const pageQueries = querySelectorAll.mock.contexts.filter(insidePage);
    expect(pageQueries).toEqual([page]);
    expect(querySelectorAll.mock.calls[
      querySelectorAll.mock.contexts.indexOf(page)
    ]?.[0]).toBe(
      '.virtualized-message-slot[data-message-id], [data-deferred-content-pending="true"]',
    );
    expect(querySelector.mock.contexts.filter(insidePage)).toEqual([]);
  });

  it("reuses the provenance array and measured identity of an unchanged repeat", () => {
    const { measure, measuredIdentities } = renderProvenanceHarness();
    const page = pageFromHtml(slot("a") + slot("b", pending()) + slot("c"));

    measure("page-0", 0, 100, page);
    const first = measuredIdentities["page-0"]!;
    expect(first.fullyRenderedMessageIds).toEqual(["a", "c"]);

    measure("page-0", 0, 100, page);
    expect(measuredIdentities["page-0"]).toBe(first);

    measure("page-0", 0, 140, page);
    expect(measuredIdentities["page-0"]).toBe(first);
  });

  it("stores a new identity when provenance or the page identity changes", () => {
    const { currentIdentities, measure, measuredIdentities } =
      renderProvenanceHarness();
    const page = pageFromHtml(slot("a") + slot("b", pending()));

    measure("page-0", 0, 100, page);
    const first = measuredIdentities["page-0"]!;

    page.querySelector('[data-deferred-content-pending="true"]')!.remove();
    measure("page-0", 0, 100, page);
    const afterProvenance = measuredIdentities["page-0"]!;
    expect(afterProvenance).not.toBe(first);
    expect(afterProvenance.fullyRenderedMessageIds).toEqual(["a", "b"]);
    expect(first.fullyRenderedMessageIds).toEqual(["a"]);

    const replacedMessages: readonly Message[] = [];
    currentIdentities.current["page-0"] = {
      hasTrailingGap: false,
      messages: replacedMessages,
    };
    measure("page-0", 0, 100, page);
    const afterMessages = measuredIdentities["page-0"]!;
    expect(afterMessages).not.toBe(afterProvenance);
    expect(afterMessages.messages).toBe(replacedMessages);
    expect(afterMessages.fullyRenderedMessageIds).toBe(
      afterProvenance.fullyRenderedMessageIds,
    );

    currentIdentities.current["page-0"] = {
      hasTrailingGap: true,
      messages: replacedMessages,
    };
    measure("page-0", 0, 100, page);
    const afterGap = measuredIdentities["page-0"]!;
    expect(afterGap).not.toBe(afterMessages);
    expect(afterGap.hasTrailingGap).toBe(true);
  });
});
