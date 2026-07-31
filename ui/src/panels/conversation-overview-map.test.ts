import { describe, expect, it } from "vitest";

import type { SessionOverviewResponse } from "../api";
import {
  conversationOverviewBucketIndexForPosition,
  conversationOverviewPositionAtFraction,
  projectConversationOverviewBuckets,
  projectConversationOverviewMarkers,
  projectConversationOverviewViewport,
} from "./conversation-overview-map";

function overview(messageCount = 100): SessionOverviewResponse {
  return {
    sessionId: "session-overview",
    messageCount,
    sessionMutationStamp: 7,
    buckets: [
      { c: 25, k: "text", u: 3, m: false },
      { c: 25, k: "command", u: 0, m: true },
      { c: 25, k: "diff", u: 0, m: false },
      { c: 25, k: "error", u: 0, m: false },
    ],
    markers: [{ position: 40, kind: "review", label: "Review" }],
    latestPosition: Math.max(0, messageCount - 1),
  };
}

describe("position-linear conversation overview", () => {
  it("projects every bucket in whole-conversation position space", () => {
    const buckets = projectConversationOverviewBuckets(overview());

    expect(buckets.map((bucket) => bucket.startPosition)).toEqual([
      0, 25, 50, 75,
    ]);
    expect(buckets.map((bucket) => bucket.topPercent)).toEqual([
      0, 25, 50, 75,
    ]);
    expect(buckets.every((bucket) => bucket.heightPercent === 25)).toBe(true);
  });

  it("projects markers and the viewport on the same scale", () => {
    const response = overview();
    const marker = projectConversationOverviewMarkers(response)[0]!;
    const viewport = projectConversationOverviewViewport(response, {
      startPosition: 40,
      endPosition: 60,
    });

    expect(marker.topPercent).toBe(40);
    expect(viewport).toEqual({ topPercent: 40, heightPercent: 20 });
  });

  it("maps pointer fractions directly to global positions", () => {
    const response = overview();
    expect(conversationOverviewPositionAtFraction(response, 0)).toBe(0);
    expect(conversationOverviewPositionAtFraction(response, 0.405)).toBe(40);
    expect(conversationOverviewPositionAtFraction(response, 1)).toBe(99);
  });

  it("maps global positions to stable bucket indexes", () => {
    const response = overview();
    expect(conversationOverviewBucketIndexForPosition(response, 0)).toBe(0);
    expect(conversationOverviewBucketIndexForPosition(response, 49)).toBe(1);
    expect(conversationOverviewBucketIndexForPosition(response, 99)).toBe(3);
  });

  it("does not depend on resident-window or layout inputs", () => {
    expect(projectConversationOverviewBuckets(overview(100))).toHaveLength(4);
    // The public projector accepts only the server response; resident messages,
    // pixel heights, focus, and virtualizer snapshots cannot affect its output.
    expect(Object.keys(projectConversationOverviewBuckets(overview())[0]!)).toEqual(
      expect.arrayContaining([
        "c",
        "k",
        "u",
        "m",
        "startPosition",
        "endPosition",
        "topPercent",
        "heightPercent",
      ]),
    );
  });
});
