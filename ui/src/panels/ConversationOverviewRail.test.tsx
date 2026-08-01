import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { SessionOverviewResponse } from "../api";
import { ConversationOverviewRail } from "./ConversationOverviewRail";

function overview(): SessionOverviewResponse {
  return {
    sessionId: "session-overview",
    messageCount: 100,
    sessionMutationStamp: 4,
    buckets: [
      { c: 25, k: "text", u: 4, m: false },
      { c: 25, k: "command", u: 0, m: true },
      { c: 25, k: "diff", u: 0, m: false },
      { c: 25, k: "error", u: 0, m: false },
    ],
    markers: [{ position: 40, kind: "review", label: "Review here" }],
    latestPosition: 99,
  };
}

describe("ConversationOverviewRail", () => {
  it("mounts a stable loading rail before the overview arrives", () => {
    render(
      <ConversationOverviewRail
        overview={null}
        viewport={{ startPosition: 0, endPosition: 1 }}
        onNavigate={() => {}}
      />,
    );

    const rail = screen.getByTestId("conversation-overview-rail");
    expect(rail).toHaveClass("is-pending");
    expect(rail).toHaveAttribute("aria-label", "Loading conversation overview");
    expect(rail).toHaveStyle({ position: "fixed" });
  });

  it("anchors the rail to the transcript viewport instead of scroll content", () => {
    render(
      <ConversationOverviewRail
        heightPx={480}
        rightPx={28}
        topPx={96}
        overview={overview()}
        viewport={{ startPosition: 40, endPosition: 60 }}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByTestId("conversation-overview-rail")).toHaveStyle({
      position: "fixed",
      height: "480px",
      right: "28px",
      top: "96px",
    });
  });

  it("renders server buckets, markers, and viewport in one position scale", () => {
    render(
      <ConversationOverviewRail
        overview={overview()}
        viewport={{ startPosition: 40, endPosition: 60 }}
        onNavigate={() => {}}
      />,
    );

    const segments = document.querySelectorAll(
      ".conversation-overview-visual-segment",
    );
    expect(segments).toHaveLength(4);
    expect(segments[0]).toHaveAttribute("data-kind", "text");
    expect(segments[0]).toHaveClass("has-user-messages");
    expect(screen.getByLabelText("Review here")).toHaveStyle({ top: "40%" });
    expect(screen.getByTestId("conversation-overview-viewport")).toHaveStyle({
      top: "40%",
      height: "20%",
    });
    expect(
      screen.getByTestId("conversation-overview-viewport-handle"),
    ).toHaveStyle({
      "--conversation-overview-viewport-center": "50%",
      height: "24px",
    });
  });

  it("maps pointer navigation directly to a global message position", () => {
    const onNavigate = vi.fn();
    render(
      <ConversationOverviewRail
        overview={overview()}
        viewport={{ startPosition: 0, endPosition: 20 }}
        onNavigate={onNavigate}
      />,
    );
    const rail = screen.getByTestId("conversation-overview-rail");
    rail.getBoundingClientRect = () =>
      ({
        top: 100,
        height: 400,
        bottom: 500,
        left: 0,
        right: 40,
        width: 40,
        x: 0,
        y: 100,
        toJSON: () => ({}),
      }) as DOMRect;

    fireEvent.pointerDown(rail, {
      button: 0,
      clientY: 300,
      pointerId: 1,
    });

    expect(onNavigate).toHaveBeenCalledWith(50);
  });

  it("supports position-linear keyboard navigation", () => {
    const onNavigate = vi.fn();
    render(
      <ConversationOverviewRail
        overview={overview()}
        viewport={{ startPosition: 40, endPosition: 60 }}
        onNavigate={onNavigate}
      />,
    );
    const rail = screen.getByTestId("conversation-overview-rail");

    fireEvent.keyDown(rail, { key: "ArrowDown" });
    expect(onNavigate).toHaveBeenCalledWith(50);
    fireEvent.keyDown(rail, { key: "End" });
    expect(onNavigate).toHaveBeenLastCalledWith(75);
  });
});
