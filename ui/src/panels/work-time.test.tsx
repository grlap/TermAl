import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { WorkTime, formatWorkTime } from "./work-time";

describe("work-time", () => {
  it("prints source timestamps to the minute in the requested zone, whatever their precision", () => {
    expect(formatWorkTime("2026-09-05T23:06:38.367821300Z", "UTC")).toBe("2026-09-05 23:06");
    expect(formatWorkTime("2026-09-05T23:06:38.367821300Z", "Europe/Warsaw")).toBe("2026-09-06 01:06");
    expect(formatWorkTime("2026-09-15T16:11:33.827648500+00:00", "UTC")).toBe("2026-09-15 16:11");
    expect(formatWorkTime("2026-09-14T00:00:00Z", "UTC")).toBe("2026-09-14 00:00");
    expect(formatWorkTime("2026-09-14T10:00:00Z", "America/New_York")).toBe("2026-09-14 06:00");
  });

  it("shows a value that is not a timestamp as the source sent it", () => {
    expect(formatWorkTime("today", "UTC")).toBe("today");
    expect(formatWorkTime("", "UTC")).toBe("");
  });

  it("renders a time element that keeps the raw value on hover and a parseable dateTime", () => {
    const { container } = render(<WorkTime value="2026-09-05T23:06:38.367821300Z" />);
    const time = container.querySelector("time")!;
    expect(time.getAttribute("title")).toBe("2026-09-05T23:06:38.367821300Z");
    expect(time.getAttribute("dateTime")).toBe("2026-09-05T23:06:38.367Z");
    expect(time.textContent).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
    const plain = render(<WorkTime value="today" />);
    expect(plain.container.querySelector("time")).toBeNull();
    expect(plain.container.textContent).toBe("today");
  });
});
