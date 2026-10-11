import { act, fireEvent, render, screen } from "@testing-library/react";
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
  it("keeps a portaled touch drag from cancelling itself through the transcript", async () => {
    const frame = vi.spyOn(window, "requestAnimationFrame").mockReturnValue(1);
    const cancelFrame = vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
    let complete!: (applied: boolean) => void;
    const onNavigate = vi.fn(() => new Promise<boolean>((resolve) => { complete = resolve; }));
    const transcriptTouch = vi.fn(() => complete?.(false));
    const view = render(<section onTouchStart={transcriptTouch} onTouchMove={transcriptTouch}>
      <ConversationOverviewRail heightPx={400} rightPx={0} topPx={0}
        messageCount={100} overview={overview()} portalTarget={document.body}
        onNavigate={onNavigate} viewport={{ startPosition: 0, endPosition: 10 }} />
    </section>);
    try {
      const rail = screen.getByTestId("conversation-overview-rail");
      rail.getBoundingClientRect = () => ({ top: 100, height: 400 }) as DOMRect;
      fireEvent.pointerDown(rail, { button: 0, pointerId: 1, pointerType: "touch", clientY: 420 });
      fireEvent.touchStart(rail, { touches: [{ clientY: 420 }] });
      fireEvent.pointerMove(rail, { pointerId: 1, pointerType: "touch", clientY: 420 });
      fireEvent.touchMove(rail, { touches: [{ clientY: 419 }] });
      // Let a wrongly bubbled touch settle the request before release, while
      // the pending pointer-move frame still has not run.
      await act(async () => Promise.resolve());
      fireEvent.pointerUp(rail, { pointerId: 1, pointerType: "touch", clientY: 420 });
      expect(transcriptTouch).not.toHaveBeenCalled();
      expect(onNavigate).toHaveBeenCalledOnce();
      expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
        "--conversation-overview-viewport-center": "80%",
      });
      await act(async () => { complete(true); });
    } finally {
      view.unmount(); frame.mockRestore(); cancelFrame.mockRestore();
    }
  });

  it("keeps the released target while its history is loading without restarting the request", async () => {
    let complete!: (applied: boolean) => void;
    const onNavigate = vi.fn(() => new Promise<boolean>((resolve) => { complete = resolve; }));
    const props = {
      heightPx: 400, rightPx: 0, topPx: 0, messageCount: 100,
      overview: overview(), portalTarget: document.body, onNavigate,
      viewport: { startPosition: 0, endPosition: 10 },
    };
    const { rerender } = render(<ConversationOverviewRail {...props} />);
    const rail = screen.getByTestId("conversation-overview-rail");
    rail.getBoundingClientRect = () => ({ top: 100, height: 400 }) as DOMRect;
    fireEvent.pointerDown(rail, { button: 0, pointerId: 1, clientY: 420 });
    fireEvent.pointerUp(rail, { pointerId: 1, clientY: 420 });
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "80%",
    });
    expect(onNavigate).toHaveBeenCalledOnce();
    // The resident viewport can still be the old one until React commits the
    // loaded page. Releasing the pointer must not restore that old indicator.
    rerender(<ConversationOverviewRail {...props} viewport={{ startPosition: 10, endPosition: 20 }} />);
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "80%",
    });
    rerender(<ConversationOverviewRail {...props} viewport={{ startPosition: 75, endPosition: 85 }} />);
    await act(async () => { complete(true); });
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "80%",
    });
  });

  it("keeps the drag handle under the pointer while history changes and lands at release", () => {
    const onNavigate = vi.fn();
    const props = {
      heightPx: 400, rightPx: 0, topPx: 0, messageCount: 100,
      overview: overview(), portalTarget: document.body, onNavigate,
      viewport: { startPosition: 0, endPosition: 10 },
    };
    const { rerender } = render(<ConversationOverviewRail {...props} />);
    const rail = screen.getByTestId("conversation-overview-rail");
    rail.getBoundingClientRect = () => ({ top: 100, height: 400 }) as DOMRect;
    fireEvent.pointerDown(rail, { button: 0, pointerId: 1, clientY: 300 });
    expect(onNavigate).toHaveBeenLastCalledWith(50);
    rerender(<ConversationOverviewRail {...props} viewport={{ startPosition: 5, endPosition: 15 }} />);
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "50%",
    });
    fireEvent.pointerMove(rail, { pointerId: 1, clientY: 420 });
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "80%",
    });
    fireEvent.pointerUp(rail, { pointerId: 1, clientY: 460 });
    expect(onNavigate).toHaveBeenLastCalledWith(90);
  });

  it("ignores an older completion and releases a failed current target", async () => {
    const completions: ((applied: boolean) => void)[] = [];
    const onNavigate = vi.fn(() => new Promise<boolean>((resolve) => { completions.push(resolve); }));
    render(<ConversationOverviewRail
      heightPx={400} rightPx={0} topPx={0} messageCount={100}
      overview={overview()} portalTarget={document.body} onNavigate={onNavigate}
      viewport={{ startPosition: 0, endPosition: 10 }}
    />);
    const rail = screen.getByTestId("conversation-overview-rail");
    rail.getBoundingClientRect = () => ({ top: 100, height: 400 }) as DOMRect;
    fireEvent.pointerDown(rail, { button: 0, pointerId: 1, clientY: 420 });
    fireEvent.pointerUp(rail, { pointerId: 1, clientY: 420 });
    fireEvent.pointerDown(rail, { button: 0, pointerId: 2, clientY: 300 });
    fireEvent.pointerUp(rail, { pointerId: 2, clientY: 300 });
    await act(async () => { completions[0]?.(false); });
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "50%",
    });
    await act(async () => { completions[1]?.(false); });
    expect(screen.getByTestId("conversation-overview-viewport-handle")).toHaveStyle({
      "--conversation-overview-viewport-center": "5%",
    });
  });

  it("waits for complete pane-local geometry before mounting a loading rail", () => {
    const { rerender } = render(
      <ConversationOverviewRail
        messageCount={100}
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
        messageCount={100}
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
    expect(rail).toHaveAttribute(
      "aria-label",
      "Conversation overview loading, message 1 of 100",
    );
    expect(rail).toHaveStyle({ position: "absolute" });
    expect(rail).toHaveAttribute("tabindex", "0");
  });

  it("keeps pointer and keyboard scrolling available while overview data loads", () => {
    const onNavigate = vi.fn();
    render(
      <ConversationOverviewRail
        heightPx={480}
        messageCount={100}
        rightPx={28}
        topPx={96}
        overview={null}
        portalTarget={document.body}
        viewport={{ startPosition: 40, endPosition: 60 }}
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
    expect(onNavigate).toHaveBeenLastCalledWith(50);

    fireEvent.keyDown(rail, { key: "PageDown" });
    expect(onNavigate).toHaveBeenLastCalledWith(50);
    fireEvent.keyDown(rail, { key: "End" });
    expect(onNavigate).toHaveBeenLastCalledWith(99);
  });

  it("anchors the rail inside its owning pane instead of the global body", () => {
    const portalTarget = document.createElement("section");
    portalTarget.className = "workspace-pane";
    document.body.append(portalTarget);
    const { unmount } = render(
      <ConversationOverviewRail
        heightPx={480}
        messageCount={100}
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
        messageCount={100}
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
        messageCount={100}
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
        messageCount={100}
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
    fireEvent.keyDown(rail, { key: "PageUp" });
    expect(onNavigate).toHaveBeenLastCalledWith(0);
    fireEvent.keyDown(rail, { key: "Home" });
    expect(onNavigate).toHaveBeenLastCalledWith(0);
    fireEvent.keyDown(rail, { key: "End" });
    expect(onNavigate).toHaveBeenLastCalledWith(99);
  });
});
