import { fireEvent, render, waitFor } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import type { Session } from "../types";
import type { WorkspaceCanvasTab } from "../workspace";
import { SessionCanvasPanel } from "./SessionCanvasPanel";

describe("SessionCanvasPanel", () => {
  it("zooms around the pointer when ctrl + wheel is used", async () => {
    const onSetZoomSpy = vi.fn();
    const { container } = render(
      <ZoomHarness
        onSetZoomSpy={onSetZoomSpy}
        session={makeSession("session-a")}
        tab={{
          id: "canvas-a",
          kind: "canvas",
          cards: [{ sessionId: "session-a", x: 160, y: 220 }],
          originSessionId: null,
        }}
      />,
    );

    const scrollContainer = container.querySelector(".message-stack") as HTMLDivElement;
    let scrollLeft = 120;
    let scrollTop = 180;
    Object.defineProperty(scrollContainer, "scrollLeft", {
      configurable: true,
      get: () => scrollLeft,
      set: (value: number) => {
        scrollLeft = value;
      },
    });
    Object.defineProperty(scrollContainer, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });

    const frame = container.querySelector(".session-canvas-scale-frame") as HTMLDivElement;
    vi.spyOn(frame, "getBoundingClientRect").mockReturnValue({
      x: 100,
      y: 80,
      left: 100,
      top: 80,
      right: 3700,
      bottom: 2480,
      width: 3600,
      height: 2400,
      toJSON() {
        return {};
      },
    } as DOMRect);

    fireEvent.wheel(frame, {
      ctrlKey: true,
      deltaY: -100,
      clientX: 340,
      clientY: 380,
    });

    await waitFor(() => expect(onSetZoomSpy).toHaveBeenCalledTimes(1));
    const nextZoom = onSetZoomSpy.mock.calls[0][0] as number;

    expect(nextZoom).toBeGreaterThan(1);
    await waitFor(() => {
      expect(scrollLeft).toBeCloseTo(120 + 240 * nextZoom - 240, 4);
      expect(scrollTop).toBeCloseTo(180 + 300 * nextZoom - 300, 4);
    });
  });

  it("pans the canvas viewport when right dragging", () => {
    const { container } = render(
      <ZoomHarness
        onSetZoomSpy={vi.fn()}
        session={makeSession("session-a")}
        tab={{
          id: "canvas-a",
          kind: "canvas",
          cards: [{ sessionId: "session-a", x: 160, y: 220 }],
          originSessionId: null,
        }}
      />,
    );

    const scrollContainer = container.querySelector(".message-stack") as HTMLDivElement;
    let scrollLeft = 120;
    let scrollTop = 180;
    Object.defineProperty(scrollContainer, "scrollLeft", {
      configurable: true,
      get: () => scrollLeft,
      set: (value: number) => {
        scrollLeft = value;
      },
    });
    Object.defineProperty(scrollContainer, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });

    const frame = container.querySelector(".session-canvas-scale-frame") as HTMLDivElement;
    let capturedPointerId: number | null = null;
    Object.defineProperty(frame, "setPointerCapture", {
      configurable: true,
      value: (pointerId: number) => {
        capturedPointerId = pointerId;
      },
    });
    Object.defineProperty(frame, "releasePointerCapture", {
      configurable: true,
      value: (pointerId: number) => {
        if (capturedPointerId === pointerId) {
          capturedPointerId = null;
        }
      },
    });
    Object.defineProperty(frame, "hasPointerCapture", {
      configurable: true,
      value: (pointerId: number) => capturedPointerId === pointerId,
    });

    fireEvent.pointerDown(frame, {
      button: 2,
      buttons: 2,
      clientX: 300,
      clientY: 260,
      pointerId: 9,
    });
    fireEvent.pointerMove(frame, {
      button: 2,
      buttons: 2,
      clientX: 360,
      clientY: 300,
      pointerId: 9,
    });
    fireEvent.pointerUp(frame, {
      button: 2,
      buttons: 0,
      clientX: 360,
      clientY: 300,
      pointerId: 9,
    });

    expect(scrollLeft).toBe(60);
    expect(scrollTop).toBe(140);

    const contextMenuEvent = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
    });
    frame.dispatchEvent(contextMenuEvent);
    expect(contextMenuEvent.defaultPrevented).toBe(true);
  });

  it("ignores plain wheel input", () => {
    const onSetZoomSpy = vi.fn();
    const { container } = render(
      <ZoomHarness
        onSetZoomSpy={onSetZoomSpy}
        session={makeSession("session-a")}
        tab={{
          id: "canvas-a",
          kind: "canvas",
          cards: [{ sessionId: "session-a", x: 160, y: 220 }],
          originSessionId: null,
        }}
      />,
    );

    const frame = container.querySelector(".session-canvas-scale-frame") as HTMLDivElement;
    fireEvent.wheel(frame, {
      deltaY: -100,
      clientX: 340,
      clientY: 380,
    });

    expect(onSetZoomSpy).not.toHaveBeenCalled();
  });
});

function ZoomHarness({
  onSetZoomSpy,
  session,
  tab,
}: {
  onSetZoomSpy: (zoom: number) => void;
  session: Session;
  tab: WorkspaceCanvasTab;
}) {
  const [zoom, setZoom] = useState(tab.zoom ?? 1);

  return (
    <div className="message-stack">
      <SessionCanvasPanel
        tab={{ ...tab, zoom }}
        sessionLookup={new Map([[session.id, session]])}
        draggedTab={null}
        onOpenSession={() => {}}
        onRemoveCard={() => {}}
        onSetZoom={(nextZoom) => {
          onSetZoomSpy(nextZoom);
          setZoom(nextZoom);
        }}
        onUpsertCard={() => {}}
      />
    </div>
  );
}

function makeSession(id: string): Session {
  return {
    agent: "Codex",
    emoji: "O",
    id,
    messages: [],
    model: "gpt-5.4",
    name: "Session",
    preview: "Ready",
    status: "idle",
    workdir: "C:/repo",
  };
}
