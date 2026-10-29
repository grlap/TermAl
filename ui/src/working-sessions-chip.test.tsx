import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  WorkingSessionsBreakdownDescription,
  WorkingSessionsChipLabel,
} from "./working-sessions-chip";
import type { SessionWorkingBreakdown } from "./session-list-filter";

function renderChip(count: number, breakdown: SessionWorkingBreakdown) {
  render(
    <>
      <button type="button" aria-describedby={count > 0 ? "split" : undefined}>
        <WorkingSessionsChipLabel count={count} breakdown={breakdown} />
      </button>
      <WorkingSessionsBreakdownDescription id="split" count={count} breakdown={breakdown} />
    </>,
  );
  return screen.getByRole("button", { name: `Working sessions (${count})` });
}

const full = () => document.querySelector(".sidebar-status-split-full");
const compact = () => document.querySelector(".sidebar-status-split-compact");

describe("Working sessions chip", () => {
  it("shows the split inside the chip, in full and compact forms, without changing its name", () => {
    const chip = renderChip(3, { foreground: 1, background: 2 });
    expect(full()).toHaveTextContent("· 1 foreground / 2 background");
    expect(compact()).toHaveTextContent("(1/2)");
    // Both forms are visual only; the description spells the split out.
    expect(full()).toHaveAttribute("aria-hidden", "true");
    expect(compact()).toHaveAttribute("aria-hidden", "true");
    expect(chip).toHaveAccessibleDescription(
      "1 foreground / 2 background. A session with a foreground turn and background work counts in both.",
    );
    // Hidden, so it is read once, as the button's description.
    expect(document.getElementById("split")).toHaveAttribute("hidden");
    // The compact form's tooltip spells it out too.
    expect(compact()).toHaveAttribute(
      "title",
      "1 foreground / 2 background. A session with a foreground turn and background work counts in both.",
    );
  });

  it("shows a zero foreground count", () => {
    const chip = renderChip(2, { foreground: 0, background: 2 });
    expect(full()).toHaveTextContent("· 0 foreground / 2 background");
    expect(compact()).toHaveTextContent("(0/2)");
    expect(chip).toHaveAccessibleDescription(/^0 foreground \/ 2 background\./);
  });

  it("shows a zero background count", () => {
    const chip = renderChip(2, { foreground: 2, background: 0 });
    expect(full()).toHaveTextContent("· 2 foreground / 0 background");
    expect(compact()).toHaveTextContent("(2/0)");
    expect(chip).toHaveAccessibleDescription(/^2 foreground \/ 0 background\./);
  });

  it("counts a session with both kinds of work in each", () => {
    renderChip(1, { foreground: 1, background: 1 });
    expect(full()).toHaveTextContent("· 1 foreground / 1 background");
    expect(compact()).toHaveTextContent("(1/1)");
  });

  it("shows no split and no description with no working session", () => {
    const chip = renderChip(0, { foreground: 0, background: 0 });
    expect(chip).toHaveTextContent(/^Working sessions \(0\)$/);
    expect(full()).toBeNull();
    expect(compact()).toBeNull();
    expect(document.getElementById("split")).toBeNull();
    expect(chip).not.toHaveAttribute("aria-describedby");
  });
});
