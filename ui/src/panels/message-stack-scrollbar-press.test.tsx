import { afterEach, describe, expect, it } from "vitest";
import { classifyScrollbarPress, freezeMessageStackView } from "./message-stack-scrollbar-press";

// A 1000 px tall stack at the page top with a 17 px classic scrollbar on its
// right edge (arrow buttons 17 px tall) and an 8,000,000 px scroll extent.
function makeStack(scrollTop: number) {
  const parent = document.createElement("div");
  const node = document.createElement("section");
  node.className = "message-stack";
  parent.append(node);
  document.body.append(parent);
  Object.defineProperties(node, {
    clientHeight: { value: 1000 }, clientWidth: { value: 483 }, offsetWidth: { value: 500 },
    offsetHeight: { value: 1000 }, offsetTop: { value: 40 }, offsetLeft: { value: 10 },
    clientTop: { value: 0 }, clientLeft: { value: 0 },
    scrollHeight: { value: 8_000_000 }, scrollTop: { value: scrollTop, writable: true },
    getBoundingClientRect: { value: () => ({ top: 0, left: 0, right: 500, bottom: 1000, width: 500, height: 1000, x: 0, y: 0, toJSON: () => ({}) }) },
  });
  return { parent, node };
}

afterEach(() => { document.body.innerHTML = ""; });

describe("classifyScrollbarPress", () => {
  it("ignores presses on the content", () => {
    const { node } = makeStack(0);
    expect(classifyScrollbarPress(node, 200, 500)).toBeNull();
  });

  it("tells the arrow buttons and the thumb from the track", () => {
    // Scrolled to the end: the thumb sits at the bottom of the track.
    const { node } = makeStack(8_000_000 - 1000);
    expect(classifyScrollbarPress(node, 490, 5)).toEqual({ kind: "arrow" });
    expect(classifyScrollbarPress(node, 490, 995)).toEqual({ kind: "arrow" });
    expect(classifyScrollbarPress(node, 490, 975)).toEqual({ kind: "thumb" });
  });

  it("sends a track press to the pressed position", () => {
    const { node } = makeStack(8_000_000 - 1000);
    const press = classifyScrollbarPress(node, 490, 500);
    expect(press?.kind).toBe("track");
    // The middle of the track is the middle of the scroll range.
    const middle = (8_000_000 - 1000) / 2;
    expect(press?.kind === "track" && Math.abs(press.scrollTop - middle)).toBeLessThan(8_000_000 * 0.01);
  });
});

describe("freezeMessageStackView", () => {
  it("covers the live view with a still copy until released", () => {
    const { parent, node } = makeStack(1234);
    node.innerHTML = '<div id="page" data-message-id="m-1">first</div><div>second</div>';
    const frozen = freezeMessageStackView(node);

    frozen.take();
    const copy = parent.querySelector<HTMLElement>(".message-stack-frozen-view")!;
    expect(copy).not.toBeNull();
    expect(copy.textContent).toBe("firstsecond");
    // Presentation only: no duplicate ids or message identities.
    expect(copy.querySelector("[id]")).toBeNull();
    expect(copy.querySelector("[data-message-id]")).toBeNull();
    expect(copy.getAttribute("aria-hidden")).toBe("true");
    // It covers the content area, not the scrollbar.
    expect(copy.style.width).toBe("483px");
    // The live content under it is hidden, not moved.
    expect([...node.children].every(child => (child as HTMLElement).style.visibility === "hidden")).toBe(true);

    // A refresh copies the live view as shown, not as hidden.
    frozen.take();
    const refreshed = parent.querySelectorAll<HTMLElement>(".message-stack-frozen-view");
    expect(refreshed).toHaveLength(1);
    expect([...refreshed[0].children].every(child => (child as HTMLElement).style.visibility === "")).toBe(true);

    frozen.release();
    expect(parent.querySelector(".message-stack-frozen-view")).toBeNull();
    expect([...node.children].every(child => (child as HTMLElement).style.visibility === "")).toBe(true);
    expect(node.querySelector("[data-message-id]")).not.toBeNull();
  });
});
