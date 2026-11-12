// Behavioral coverage of clock labels and the shared visible-page lifecycle.
import { act, cleanup, render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SidebarClockBar } from "./SidebarClockBar";

let hidden = false;

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-11-12T08:04:59.500Z"));
  hidden = false;
  vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
  const original = new Intl.DateTimeFormat().resolvedOptions();
  vi.spyOn(Intl.DateTimeFormat.prototype, "resolvedOptions").mockReturnValue({
    ...original,
    timeZone: "America/Los_Angeles",
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

function setHidden(value: boolean) {
  hidden = value;
  act(() => {
    document.dispatchEvent(new Event("visibilitychange"));
  });
}

function advance(milliseconds: number) {
  act(() => {
    vi.advanceTimersByTime(milliseconds);
  });
}

describe("SidebarClockBar", () => {
  it("renders UTC and browser-local time with accessible ISO time metadata", () => {
    render(<SidebarClockBar />);
    const time = screen.getByLabelText("UTC time 08:04; local time 00:04 PST");
    expect(time).toHaveTextContent("UTC 08:04");
    expect(time).toHaveTextContent("Local 00:04 PST");
    expect(time.tagName).toBe("TIME");
    expect(time).toHaveAttribute("datetime", "2026-11-12T08:04:59.500Z");
  });

  it("rolls over at the next minute boundary with one timer", () => {
    render(<SidebarClockBar />);
    expect(vi.getTimerCount()).toBe(1);
    advance(499);
    expect(screen.getByText("UTC 08:04")).toBeInTheDocument();
    advance(1);
    expect(screen.getByText("UTC 08:05")).toBeInTheDocument();
    expect(screen.getByText("Local 00:05 PST")).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(1);
    advance(60_000);
    expect(screen.getByText("UTC 08:06")).toBeInTheDocument();
  });

  it("suspends hidden updates and refreshes immediately on visibility", () => {
    render(<SidebarClockBar />);
    setHidden(true);
    expect(vi.getTimerCount()).toBe(0);
    advance(120_500);
    expect(screen.getByText("UTC 08:04")).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(0);
    setHidden(false);
    expect(screen.getByText("UTC 08:07")).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(1);
    advance(60_000);
    expect(screen.getByText("UTC 08:08")).toBeInTheDocument();
  });

  it("does not schedule an update when first mounted hidden", () => {
    hidden = true;
    render(<SidebarClockBar />);
    expect(vi.getTimerCount()).toBe(0);
    advance(120_000);
    expect(screen.getByText("UTC 08:04")).toBeInTheDocument();
    setHidden(false);
    expect(screen.getByText("UTC 08:06")).toBeInTheDocument();
  });

  it("shares one timer across panels and releases it after the last unmount", () => {
    const first = render(
      <StrictMode>
        <SidebarClockBar />
      </StrictMode>,
    );
    const second = render(<SidebarClockBar />);
    expect(vi.getTimerCount()).toBe(1);
    first.unmount();
    expect(vi.getTimerCount()).toBe(1);
    second.unmount();
    expect(vi.getTimerCount()).toBe(0);
    setHidden(true);
    setHidden(false);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("uses daylight time and shows UTC midnight as 00:00", () => {
    vi.setSystemTime(new Date("2026-07-12T00:00:00.000Z"));
    render(<SidebarClockBar />);
    expect(
      screen.getByLabelText("UTC time 00:00; local time 17:00 PDT"),
    ).toBeInTheDocument();
  });
});
