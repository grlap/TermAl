// connection-retry.ts
//
// Owns: pure parsing, lifecycle classification, and presentation metadata for
// assistant connection-retry notices.
//
// Does not own: message rendering, transcript search assembly, or session pane
// memoization. Those callers consume this module's display-state contract.
//
// Split out of: ui/src/app-utils.ts, where these helpers had grown into a
// coherent subsystem beside unrelated DOM, clipboard, and attachment utilities.

import type { Session } from "./types";

export type ConnectionRetryNotice = {
  attemptLabel: string | null;
  detail: string;
};

export type ConnectionRetryDisplayState =
  | "live"
  | "resolved"
  | "superseded"
  | "inactive";

export function connectionRetryPresentationFor(
  notice: ConnectionRetryNotice,
  displayState: ConnectionRetryDisplayState,
): {
  ariaLive: "polite" | "off";
  cardClassName: string;
  chipClassName: string;
  detail: string;
  heading: string;
  showSpinner: boolean;
} {
  if (displayState === "live") {
    return {
      ariaLive: "polite",
      cardClassName: "message-card connection-notice-card",
      chipClassName: "chip chip-status chip-status-active",
      heading: "Reconnecting to continue this turn",
      detail: notice.detail,
      showSpinner: true,
    };
  }
  if (displayState === "resolved") {
    return {
      ariaLive: "off",
      cardClassName:
        "message-card connection-notice-card connection-notice-card-resolved",
      chipClassName: "chip chip-status",
      heading: "Connection recovered",
      detail: notice.attemptLabel
        ? `Connection dropped briefly; the turn continued after ${notice.attemptLabel.toLowerCase()}.`
        : "Connection dropped briefly; the turn continued after an automatic retry.",
      showSpinner: false,
    };
  }
  if (displayState === "superseded") {
    return {
      ariaLive: "off",
      cardClassName:
        "message-card connection-notice-card connection-notice-card-settled",
      chipClassName: "chip chip-status",
      heading: "Retry superseded",
      detail: "A newer reconnect attempt continued the turn.",
      showSpinner: false,
    };
  }
  return {
    ariaLive: "off",
    cardClassName:
      "message-card connection-notice-card connection-notice-card-settled",
    chipClassName: "chip chip-status",
    heading: "Connection retry ended",
    detail: "The session is no longer running this turn.",
    showSpinner: false,
  };
}

export function buildConnectionRetryDisplayStateByMessageId(
  session: Pick<Session, "messages" | "status"> | null | undefined,
) {
  const map = new Map<string, ConnectionRetryDisplayState>();
  if (!session) {
    return map;
  }

  const sessionIsBusy =
    session.status === "active" || session.status === "approval";
  let nearestLaterTranscriptItem:
    | "assistant-retry"
    | "assistant-response"
    | "user-prompt"
    | null = null;
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const candidate = session.messages[index];
    if (!candidate) {
      continue;
    }
    if (candidate.author === "you") {
      nearestLaterTranscriptItem = "user-prompt";
      continue;
    }
    if (candidate.author !== "assistant") {
      continue;
    }
    const retryNotice =
      candidate.type === "text"
        ? parseConnectionRetryNotice(candidate.text)
        : null;
    if (!retryNotice) {
      nearestLaterTranscriptItem = "assistant-response";
      continue;
    }

    // Classification is based on the nearest later transcript boundary: an
    // assistant response resolves the retry, another retry supersedes it, a
    // later user prompt ends that old turn, and only the newest retry in the
    // current busy turn remains live.
    map.set(
      candidate.id,
      nearestLaterTranscriptItem === "assistant-response"
        ? "resolved"
        : nearestLaterTranscriptItem === "assistant-retry"
          ? "superseded"
          : nearestLaterTranscriptItem === null && sessionIsBusy
            ? "live"
            : "inactive",
    );
    nearestLaterTranscriptItem = "assistant-retry";
  }
  return map;
}

export function parseConnectionRetryNotice(
  text: string,
): ConnectionRetryNotice | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("Connection dropped before the response finished.")) {
    return null;
  }

  const attemptMatch = trimmed.match(
    /Retrying automatically \(attempt (\d+) of (\d+)\)\.?$/,
  );
  const attemptLabel = attemptMatch
    ? `Attempt ${attemptMatch[1]} of ${attemptMatch[2]}`
    : null;

  return {
    attemptLabel,
    detail: trimmed,
  };
}
