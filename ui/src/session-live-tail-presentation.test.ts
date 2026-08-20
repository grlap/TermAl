import { describe, expect, it, vi } from "vitest";

import {
  clearManualLiveTailDetachCompensation,
  detachLiveTailPresentationBeforeManualScroll,
  releaseManualLiveTailDetachCompensationOutsideViewport,
} from "./session-live-tail-presentation";

function rect(top: number, bottom: number): DOMRect {
  return {
    bottom,
    height: bottom - top,
    left: 0,
    right: 600,
    top,
    width: 600,
    x: 0,
    y: top,
    toJSON: () => ({}),
  };
}

function presentationFixture() {
  const scrollNode = document.createElement("section");
  const conversationPage = document.createElement("div");
  conversationPage.className = "session-conversation-page is-active";
  const liveTail = document.createElement("div");
  liveTail.className = "conversation-live-tail";
  liveTail.setAttribute("data-tail-follow", "attached");
  conversationPage.append(liveTail);
  scrollNode.append(conversationPage);
  return { liveTail, scrollNode };
}

describe("live-tail presentation", () => {
  it("preserves sticky displacement while detaching before a manual scroll", () => {
    const { liveTail, scrollNode } = presentationFixture();
    vi.spyOn(liveTail, "getBoundingClientRect").mockImplementation(() =>
      liveTail.getAttribute("data-tail-follow") === "attached"
        ? rect(100, 160)
        : rect(120, 180),
    );

    detachLiveTailPresentationBeforeManualScroll(scrollNode);

    expect(liveTail).toHaveAttribute("data-tail-follow", "detached");
    expect(liveTail).toHaveAttribute("data-manual-detach-compensation");
    expect(
      liveTail.style.getPropertyValue(
        "--conversation-live-tail-detach-offset",
      ),
    ).toBe("-20px");
  });

  it("does not compensate a sub-pixel presentation shift", () => {
    const { liveTail, scrollNode } = presentationFixture();
    vi.spyOn(liveTail, "getBoundingClientRect").mockImplementation(() =>
      liveTail.getAttribute("data-tail-follow") === "attached"
        ? rect(100, 160)
        : rect(100.4, 160.4),
    );

    detachLiveTailPresentationBeforeManualScroll(scrollNode);

    expect(liveTail).toHaveAttribute("data-tail-follow", "detached");
    expect(liveTail).not.toHaveAttribute("data-manual-detach-compensation");
    expect(
      liveTail.style.getPropertyValue(
        "--conversation-live-tail-detach-offset",
      ),
    ).toBe("");
  });

  it("clears compensation when the detached tail leaves the viewport", () => {
    const { liveTail, scrollNode } = presentationFixture();
    liveTail.setAttribute("data-tail-follow", "detached");
    liveTail.setAttribute("data-manual-detach-compensation", "");
    liveTail.style.setProperty(
      "--conversation-live-tail-detach-offset",
      "-20px",
    );
    vi.spyOn(scrollNode, "getBoundingClientRect").mockReturnValue(
      rect(0, 600),
    );
    vi.spyOn(liveTail, "getBoundingClientRect").mockReturnValue(
      rect(-80, -20),
    );

    releaseManualLiveTailDetachCompensationOutsideViewport(scrollNode);

    expect(liveTail).not.toHaveAttribute("data-manual-detach-compensation");
    expect(
      liveTail.style.getPropertyValue(
        "--conversation-live-tail-detach-offset",
      ),
    ).toBe("");
  });

  it("clears compensation when the detached tail is below the viewport", () => {
    const { liveTail, scrollNode } = presentationFixture();
    liveTail.setAttribute("data-tail-follow", "detached");
    liveTail.setAttribute("data-manual-detach-compensation", "");
    liveTail.style.setProperty(
      "--conversation-live-tail-detach-offset",
      "-20px",
    );
    vi.spyOn(scrollNode, "getBoundingClientRect").mockReturnValue(
      rect(0, 600),
    );
    vi.spyOn(liveTail, "getBoundingClientRect").mockReturnValue(
      rect(620, 680),
    );

    releaseManualLiveTailDetachCompensationOutsideViewport(scrollNode);

    expect(liveTail).not.toHaveAttribute("data-manual-detach-compensation");
    expect(
      liveTail.style.getPropertyValue(
        "--conversation-live-tail-detach-offset",
      ),
    ).toBe("");
  });

  it("retains compensation while the detached tail intersects the viewport", () => {
    const { liveTail, scrollNode } = presentationFixture();
    liveTail.setAttribute("data-tail-follow", "detached");
    liveTail.setAttribute("data-manual-detach-compensation", "");
    liveTail.style.setProperty(
      "--conversation-live-tail-detach-offset",
      "-20px",
    );
    vi.spyOn(scrollNode, "getBoundingClientRect").mockReturnValue(
      rect(0, 600),
    );
    vi.spyOn(liveTail, "getBoundingClientRect").mockReturnValue(
      rect(540, 620),
    );

    releaseManualLiveTailDetachCompensationOutsideViewport(scrollNode);

    expect(liveTail).toHaveAttribute("data-manual-detach-compensation");
    expect(
      liveTail.style.getPropertyValue(
        "--conversation-live-tail-detach-offset",
      ),
    ).toBe("-20px");
  });

  it("clears compensation from a page after it loses the active marker", () => {
    const { liveTail, scrollNode } = presentationFixture();
    liveTail.setAttribute("data-tail-follow", "detached");
    liveTail.setAttribute("data-manual-detach-compensation", "");
    liveTail.style.setProperty(
      "--conversation-live-tail-detach-offset",
      "-20px",
    );
    liveTail.parentElement?.classList.remove("is-active");

    clearManualLiveTailDetachCompensation(scrollNode);

    expect(liveTail).not.toHaveAttribute("data-manual-detach-compensation");
    expect(
      liveTail.style.getPropertyValue(
        "--conversation-live-tail-detach-offset",
      ),
    ).toBe("");
  });
});
