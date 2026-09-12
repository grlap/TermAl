import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AssistantResponseProgress } from "./assistant-response-progress";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

// Geometry is supplied explicitly: these are coordinate/observer tests, not
// browser proof that the conversation retains its height or scroll position.
function setup(tailX: number) {
  const originalRange = document.createRange.bind(document);
  vi.spyOn(document, "createRange").mockImplementation(() => {
    const range = originalRange();
    Object.defineProperty(range, "getClientRects", {
      value: () => [new DOMRect(tailX, 240, 8, 20)],
    });
    return range;
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("bubble-assistant")
      ? new DOMRect(100, 200, 400, 80)
      : new DOMRect(121, 230, 358, 38);
  });
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("assistant-response-progress") ? 20 : 400;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("assistant-response-progress") ? 4 : 80;
  });
  vi.spyOn(HTMLElement.prototype, "clientLeft", "get").mockReturnValue(1);
  vi.spyOn(HTMLElement.prototype, "clientTop", "get").mockReturnValue(1);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(398);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(78);
  return render(
    <article className="bubble-assistant">
      <div className="streaming-markdown-height-content">
        <p>A response <strong>ending here</strong></p>
        <button>Copy</button>
      </div>
      <AssistantResponseProgress />
    </article>,
  );
}

it("places squares after the last text glyph at ellipsis height, not mid-line", () => {
  const view = setup(300);
  const marker = view.container.querySelector<HTMLElement>(".assistant-response-progress")!;
  expect(marker).toHaveAttribute("aria-hidden", "true");
  expect(screen.queryByRole("status")).toBeNull();
  expect(marker.style.left).toBe("213px");
  // Bottom = 55px: 80% down the 20px text rect, relative to the card padding box.
  expect(marker.style.top).toBe("51px");
  const range = vi.mocked(document.createRange).mock.results[0].value as Range;
  expect(range.endContainer.textContent).toBe("ending here");
});

it("falls back below the text if squares would overflow a full line", () => {
  const view = setup(472);
  const marker = view.container.querySelector<HTMLElement>(".assistant-response-progress")!;
  expect(marker.style.left).toBe("20px");
  expect(marker.style.top).toBe("69px");
});

it("coalesces movement notifications and cancels pending work on unmount", () => {
  const schedule = vi.spyOn(window, "requestAnimationFrame").mockReturnValue(42);
  const cancel = vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  const view = setup(300);
  const body = view.container.querySelector(".streaming-markdown-height-content")!;
  fireEvent.scroll(body);
  fireEvent.scroll(body);
  expect(schedule).toHaveBeenCalledTimes(1);
  view.unmount();
  expect(cancel).toHaveBeenCalledWith(42);
});
