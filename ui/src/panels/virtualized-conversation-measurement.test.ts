// Owns content-identity regressions for measured virtual transcript pages.
// Does not model browser table layout, width changes, or scroll restoration.
import { describe, expect, it } from "vitest";
import type { Message } from "../types";
import { pageMatchesMeasurement, type MessagePage } from "./virtualized-conversation-measurement";

const message: Message = {
  id: "table-message", type: "text", author: "assistant", timestamp: "10:00",
  text: "| Column | Value |\n| --- | --- |\n| Long table | Wrapped content |",
};
const page: MessagePage = {
  key: "table-page", pageIndex: 0, startIndex: 0, endIndex: 1,
  hasTrailingGap: false, messages: [message],
};
const identity = { hasTrailingGap: false, messages: [message] };

describe("measured page content identity", () => {
  it("retains measurements for value-identical hydration, regardless of field order", () => {
    const copy = Object.fromEntries(Object.entries(message).reverse()) as Message;
    expect(copy).not.toBe(message);
    expect(pageMatchesMeasurement({ ...page, messages: [copy] }, identity)).toBe(true);
  });

  it.each([
    { ...message, text: "Different table content" },
    { ...message, timestamp: "10:01" },
    { ...message, id: "replacement" },
  ])("invalidates changed message fields: %j", (changed) => {
    expect(pageMatchesMeasurement({ ...page, messages: [changed] }, identity)).toBe(false);
  });

  it("compares nested wire fields, not only the text", () => {
    // Use an extra JSON field to pin conservative handling of future wire
    // additions without coupling this test to one attachment schema.
    const before = { ...message, layoutMetadata: { widths: [40, 80], open: true } };
    const same = { ...message, layoutMetadata: { open: true, widths: [40, 80] } };
    const changed = { ...message, layoutMetadata: { widths: [40, 90], open: true } };
    const measured = { ...identity, messages: [before] };
    expect(pageMatchesMeasurement({ ...page, messages: [same] }, measured)).toBe(true);
    expect(pageMatchesMeasurement({ ...page, messages: [changed] }, measured)).toBe(false);
    expect(pageMatchesMeasurement(page, measured)).toBe(false);
  });

  it("invalidates page membership and trailing-gap changes", () => {
    expect(pageMatchesMeasurement({ ...page, messages: [] }, identity)).toBe(false);
    expect(pageMatchesMeasurement({ ...page, hasTrailingGap: true }, identity)).toBe(false);
    expect(pageMatchesMeasurement(page, undefined)).toBe(false);
  });
});
