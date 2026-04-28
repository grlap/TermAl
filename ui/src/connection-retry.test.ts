import { describe, expect, it } from "vitest";

import {
  connectionRetryPresentationFor,
  parseConnectionRetryNotice,
  type ConnectionRetryDisplayState,
} from "./connection-retry";

describe("connection retry notice parsing", () => {
  it("parses retry notices with and without an attempt label", () => {
    expect(
      parseConnectionRetryNotice(
        " Connection dropped before the response finished. Retrying automatically (attempt 2 of 5). ",
      ),
    ).toEqual({
      attemptLabel: "Attempt 2 of 5",
      detail:
        "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
    });

    expect(
      parseConnectionRetryNotice(
        "Connection dropped before the response finished. Retrying automatically.",
      ),
    ).toEqual({
      attemptLabel: null,
      detail:
        "Connection dropped before the response finished. Retrying automatically.",
    });
  });

  it("ignores ordinary assistant text", () => {
    expect(parseConnectionRetryNotice("The response finished.")).toBeNull();
  });
});

describe("connection retry presentation", () => {
  const notice = {
    attemptLabel: "Attempt 2 of 5",
    detail:
      "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
  };

  it.each<{
    displayState: ConnectionRetryDisplayState;
    ariaLive: "polite" | "off";
    classNamePart: string;
    heading: string;
    showSpinner: boolean;
  }>([
    {
      displayState: "live",
      ariaLive: "polite",
      classNamePart: "connection-notice-card",
      heading: "Reconnecting to continue this turn",
      showSpinner: true,
    },
    {
      displayState: "resolved",
      ariaLive: "off",
      classNamePart: "connection-notice-card-resolved",
      heading: "Connection recovered",
      showSpinner: false,
    },
    {
      displayState: "superseded",
      ariaLive: "off",
      classNamePart: "connection-notice-card-settled",
      heading: "Retry superseded",
      showSpinner: false,
    },
    {
      displayState: "inactive",
      ariaLive: "off",
      classNamePart: "connection-notice-card-settled",
      heading: "Connection retry ended",
      showSpinner: false,
    },
  ])(
    "maps $displayState retry state to stable presentation metadata",
    ({ displayState, ariaLive, classNamePart, heading, showSpinner }) => {
      const presentation = connectionRetryPresentationFor(
        notice,
        displayState,
      );

      expect(presentation.ariaLive).toBe(ariaLive);
      expect(presentation.cardClassName).toContain(classNamePart);
      expect(presentation.chipClassName).toContain("chip-status");
      expect(presentation.heading).toBe(heading);
      expect(presentation.showSpinner).toBe(showSpinner);
    },
  );

  it("uses the explicit attempt label for resolved retry detail", () => {
    expect(connectionRetryPresentationFor(notice, "resolved").detail).toBe(
      "Connection dropped briefly; the turn continued after attempt 2 of 5.",
    );
  });

  it("falls back to automatic retry wording when no attempt label is present", () => {
    expect(
      connectionRetryPresentationFor(
        {
          ...notice,
          attemptLabel: null,
        },
        "resolved",
      ).detail,
    ).toBe(
      "Connection dropped briefly; the turn continued after an automatic retry.",
    );
  });
});
