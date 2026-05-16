import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ConnectionRetryCard } from "./connection-retry-card";
import type {
  ConnectionRetryDisplayState,
  ConnectionRetryNotice,
} from "./connection-retry";

function renderConnectionRetryCard({
  notice = {
    attemptLabel: "Attempt 2 of 5",
    detail:
      "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
  },
  displayState,
}: {
  notice?: ConnectionRetryNotice;
  displayState: ConnectionRetryDisplayState;
}) {
  return render(
    <ConnectionRetryCard
      meta={<div className="message-meta">assistant 10:00</div>}
      notice={notice}
      searchQuery=""
      searchHighlightTone="match"
      displayState={displayState}
    />,
  );
}

describe("ConnectionRetryCard", () => {
  it("renders live retry notices as polite status updates with a spinner", () => {
    const { container } = renderConnectionRetryCard({
      displayState: "live",
    });

    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent("Reconnecting to continue this turn");
    expect(status).toHaveTextContent("Attempt 2 of 5");
    expect(
      container.querySelector(".connection-notice-spinner"),
    ).toBeInTheDocument();
  });

  it("renders resolved retry notices without the live spinner", () => {
    const { container } = renderConnectionRetryCard({
      displayState: "resolved",
    });

    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "off");
    expect(status).toHaveTextContent("Connection recovered");
    expect(status).toHaveTextContent(
      "Connection dropped briefly; the turn continued after attempt 2 of 5.",
    );
    expect(
      container.querySelector(".connection-notice-spinner"),
    ).not.toBeInTheDocument();
  });

  it("omits the attempt chip when a retry notice has no attempt label", () => {
    renderConnectionRetryCard({
      displayState: "live",
      notice: {
        attemptLabel: null,
        detail:
          "Connection dropped before the response finished. Retrying automatically.",
      },
    });

    expect(screen.queryByText(/Attempt/u)).not.toBeInTheDocument();
  });
});
