// Controller lifecycle/phase behavior; the browser guard checks real CSS motion.
import { afterEach, expect, it, vi } from "vitest";
import { startActivityAnimationVisibility } from "./activity-animation-visibility";

class AnimationHarness {
  startTime: number | null = 123;
  constructor(readonly animationName: string) {}
}

const cleanups: Array<() => void> = [];
afterEach(() => {
  cleanups.splice(0).forEach(cleanup => cleanup());
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function fixture() {
  let intersect!: IntersectionObserverCallback;
  const observed = new Set<Element>();
  const disconnect = vi.fn(() => observed.clear());
  class IntersectionHarness {
    constructor(callback: IntersectionObserverCallback) { intersect = callback; }
    observe(element: Element) { observed.add(element); }
    unobserve(element: Element) { observed.delete(element); }
    disconnect = disconnect;
  }
  vi.stubGlobal("IntersectionObserver", IntersectionHarness);
  vi.stubGlobal("CSSAnimation", AnimationHarness);
  const motion = new EventTarget();
  vi.stubGlobal("matchMedia", () => motion);
  let visibility: DocumentVisibilityState = "visible";
  vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibility);
  const root = document.createElement("main");
  document.body.append(root);
  const indicator = (className: string, ...animations: AnimationHarness[]) => {
    const element = document.createElement("span");
    element.className = className;
    const getAnimations = vi.fn(() => animations);
    Object.defineProperty(element, "getAnimations", { value: getAnimations });
    root.append(element);
    return { element, animations, getAnimations };
  };
  const visible = (element: Element, isVisible: boolean) => intersect([
    { target: element, isIntersecting: isVisible, intersectionRatio: isVisible ? 1 : 0 } as IntersectionObserverEntry,
  ], {} as IntersectionObserver);
  return {
    root, indicator, visible, observed, disconnect, motion,
    start() {
      const stop = startActivityAnimationVisibility(root);
      cleanups.push(stop);
      return stop;
    },
    visibility(value: DocumentVisibilityState) {
      visibility = value;
      document.dispatchEvent(new Event("visibilitychange"));
    },
  };
}

it("admits visible spinner and dot motion together and leaves offscreen motion paused", () => {
  const f = fixture();
  const spinner = new AnimationHarness("activity-step-spin");
  const unrelated = new AnimationHarness("other-decoration");
  const badge = f.indicator("status-agent-badge", spinner, unrelated);
  const dots = f.indicator("message-activity-squares", ...Array.from({ length: 3 }, () => new AnimationHarness("transcript-activity-lift")));
  f.start();
  expect(f.observed).toEqual(new Set([badge.element, dots.element]));
  expect(badge.element.dataset.activityVisible).toBe("false");
  expect(dots.element.dataset.activityVisible).toBe("false");
  expect(spinner.startTime).toBe(123);
  f.visible(badge.element, true);
  f.visible(dots.element, true);
  expect(badge.element.dataset.activityVisible).toBe("true");
  expect(dots.element.dataset.activityVisible).toBe("true");
  expect([spinner, ...dots.animations].map(animation => animation.startTime)).toEqual([0, 0, 0, 0]);
  expect(unrelated.startTime).toBe(123);
  f.visible(dots.element, false);
  expect(dots.element.dataset.activityVisible).toBe("false");
});

it("rejoins the common phase on document/motion changes and removes all controls at teardown", () => {
  const f = fixture();
  const dot = new AnimationHarness("transcript-activity-lift");
  const dots = f.indicator("message-activity-squares", dot);
  const stop = f.start();
  f.visible(dots.element, true);
  f.visibility("hidden");
  expect(f.root.dataset.activityDocumentVisible).toBe("false");
  dot.startTime = 456;
  f.motion.dispatchEvent(new Event("change"));
  expect(dot.startTime).toBe(456);
  f.visibility("visible");
  expect(f.root.dataset.activityDocumentVisible).toBe("true");
  expect(dot.startTime).toBe(0);
  dot.startTime = 789;
  f.motion.dispatchEvent(new Event("change"));
  expect(dot.startTime).toBe(0);
  stop();
  expect(f.disconnect).toHaveBeenCalled();
  expect(f.observed.size).toBe(0);
  expect(dots.element).not.toHaveAttribute("data-activity-visible");
  expect(f.root).not.toHaveAttribute("data-activity-document-visible");
  dot.startTime = 111;
  f.visibility("hidden");
  f.visibility("visible");
  f.motion.dispatchEvent(new Event("change"));
  expect(f.root).not.toHaveAttribute("data-activity-document-visible");
  expect(dot.startTime).toBe(111);
});

it("tracks mounted/reclassified dots and resynchronizes recreated animations after a DOM move", async () => {
  const f = fixture();
  f.start();
  const dots = f.indicator("message-activity-squares", new AnimationHarness("transcript-activity-lift"));
  await Promise.resolve();
  expect(f.observed.has(dots.element)).toBe(true);
  f.visible(dots.element, true);
  const recreated = new AnimationHarness("transcript-activity-lift");
  dots.getAnimations.mockReturnValue([recreated]);
  f.root.prepend(dots.element);
  await Promise.resolve();
  expect(recreated.startTime).toBe(0);
  expect(dots.element.dataset.activityVisible).toBe("true");
  dots.element.className = "plain-text";
  await Promise.resolve();
  expect(f.observed.has(dots.element)).toBe(false);
  expect(dots.element).not.toHaveAttribute("data-activity-visible");
  dots.element.className = "message-activity-squares";
  await Promise.resolve();
  expect(f.observed.has(dots.element)).toBe(true);
  f.visible(dots.element, true);
  dots.element.remove();
  await Promise.resolve();
  expect(f.observed.has(dots.element)).toBe(false);
  expect(dots.element).not.toHaveAttribute("data-activity-visible");
});
