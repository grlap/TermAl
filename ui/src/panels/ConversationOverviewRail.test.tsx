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
  it("waits for complete pane-local geometry before mounting a loading rail", () => {
    const { rerender } = render(
      <ConversationOverviewRail
        overview={null}
        portalTarget={document.body}
        viewport={{ startPosition: 0, endPosition: 1 }}
        onNavigate={() => {}}
      />,
    );

    expect(screen.queryByTestId("conversation-overview-rail")).toBeNull();

    rerender(
      <ConversationOverviewRail
        heightPx={480}
        rightPx={28}
        topPx={96}
        overview={null}
        portalTarget={document.body}
        viewport={{ startPosition: 0, endPosition: 1 }}
        onNavigate={() => {}}
      />,
    );
    const rail = screen.getByTestId("conversation-overview-rail");
    expect(rail).toHaveClass("is-pending");
    expect(rail).toHaveAttribute("aria-label", "Loading conversation overview");
    expect(rail).toHaveStyle({ position: "absolute" });
  });

  it("anchors the rail inside its owning pane instead of the global body", () => {
    const portalTarget = document.createElement("section");
    portalTarget.className = "workspace-pane";
    document.body.append(portalTarget);
    const { unmount } = render(
      <ConversationOverviewRail
        heightPx={480}
        rightPx={28}
        topPx={96}
        overview={overview()}
        portalTarget={portalTarget}
        viewport={{ startPosition: 40, endPosition: 60 }}
        onNavigate={() => {}}
      />,
    );

    const rail = screen.getByTestId("conversation-overview-rail");
    expect(portalTarget.contains(rail)).toBe(true);
    expect(rail).toHaveStyle({
      position: "absolute",
      height: "480px",
      right: "28px",
      top: "96px",
    });
    unmount();
    portalTarget.remove();
  });

  it("renders server buckets, markers, and viewport in one position scale", () => {
    render(
      <ConversationOverviewRail
        heightPx={480}
        rightPx={28}
        topPx={96}
        overview={overview()}
        portalTarget={document.body}
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
        heightPx={480}
        rightPx={28}
        topPx={96}
        overview={overview()}
        portalTarget={document.body}
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
        heightPx={480}
        rightPx={28}
        topPx={96}
        overview={overview()}
        portalTarget={document.body}
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
