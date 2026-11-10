import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SessionActivityStrip } from "./session-activity-cards";
import type { SessionActivitySource } from "./AgentSessionPanel.waiting-indicator";

describe("SessionActivityStrip", () => {
  it("keeps the same DOM footprint through idle, working, stopping, and completion", () => {
    const session: SessionActivitySource = { agent: "Codex", status: "idle" };
    const { container, rerender } = render(<SessionActivityStrip session={session} />);
    const strip = container.firstElementChild;
    const track = strip?.firstElementChild;
    const status = screen.getByRole("status");
    const snapshots: string[] = [];
    for (const [state, label] of [
      ["active", "working"], ["stopping", "stopping"], ["idle", "idle"],
    ] as const) {
      rerender(<SessionActivityStrip session={{ ...session, status: state }} />);
      expect(container.firstElementChild).toBe(strip);
      expect(strip?.firstElementChild).toBe(track);
      expect(strip?.childElementCount).toBe(3);
      expect(status).toHaveTextContent(`Codex is ${label}`);
      expect(status).toHaveClass("visually-hidden");
      expect(status).toHaveAttribute("aria-live", "polite");
      expect(status).toHaveAttribute("aria-atomic", "true");
      expect(strip).toHaveAttribute("data-state", label === "working" ? "working" : state);
      snapshots.push([
        strip?.tagName,
        strip?.className,
        strip?.getAttribute("data-state"),
        strip?.getAttribute("data-animated"),
        Array.from(strip?.children ?? []).map((child) => child.className).join(" | "),
        status.textContent,
      ].join("; "));
    }
    expect(snapshots).toEqual([
      "DIV; session-activity-strip; working; true; session-activity-strip-track | visually-hidden | activity-tooltip; Codex is working",
      "DIV; session-activity-strip; stopping; true; session-activity-strip-track | visually-hidden | activity-tooltip; Codex is stopping",
      "DIV; session-activity-strip; idle; false; session-activity-strip-track | visually-hidden | activity-tooltip; Codex is idle",
    ]);
  });

  it("provides prompt context to assistive technology and the keyboard tooltip", () => {
    render(<SessionActivityStrip session={{ agent: "Codex", status: "active", liveActivity: { prompt: "/review-code" } }} />);
    expect(screen.getByRole("status")).toHaveTextContent("Codex is working: /review-code");
    const tooltip = screen.getByRole("tooltip");
    expect(tooltip).toHaveTextContent("/review-code");
    expect(tooltip.parentElement).toHaveAttribute("tabindex", "0");
    expect(tooltip.parentElement).toHaveAttribute("aria-describedby", tooltip.id);
  });

  it("shows send and delegation feedback without any resident transcript", () => {
    const session: SessionActivitySource = { agent: "Codex", status: "idle" };
    const { rerender } = render(<SessionActivityStrip session={session} isSending />);
    expect(screen.getByRole("status")).toHaveTextContent("Codex is sending a prompt");
    rerender(<SessionActivityStrip session={session} delegationWaitPrompt="Waiting for two reviewers" />);
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for two reviewers");
    expect(screen.getByRole("status").parentElement).toHaveAttribute("data-animated", "false");
  });

  // Pixel geometry and motion are exercised with production CSS in
  // scripts/check-activity-indicators.mjs, rather than pinning stylesheet text.
});
