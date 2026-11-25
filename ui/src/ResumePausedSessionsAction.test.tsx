import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ResumePausedQueuesResponse } from "./api";
import { ResumePausedSessionsAction } from "./ResumePausedSessionsAction";

function listing(dryRun: boolean): ResumePausedQueuesResponse {
  return {
    dryRun,
    pausedDelegationSessionsNotShown: 1,
    sessions: [
      {
        sessionId: "session-mail",
        name: "Mail runner",
        queuePauseReason: {
          cause: "turn_failed",
          failureKind: "provider_usage_limit",
          summary: "weekly limit reached",
          pausedAt: "2026-11-23T17:40:50.000Z",
        },
        action: "resume",
        ...(dryRun ? {} : { outcome: "resumed" as const }),
      },
      {
        sessionId: "session-operator",
        name: "Operator stopped",
        queuePauseReason: {
          cause: "stop_requested",
          pausedAt: "2026-11-23T18:00:00.000Z",
        },
        action: "skip",
        skipReason: "operator_paused",
      },
    ],
  };
}

function row(sessionId: string) {
  const item = screen
    .getByRole("list", { name: "Paused sessions" })
    .querySelector(`[data-session-id="${sessionId}"]`);
  if (!(item instanceof HTMLElement)) {
    throw new Error(`no row for ${sessionId}`);
  }
  return within(item);
}

describe("ResumePausedSessionsAction", () => {
  it("previews each paused session's reason and action, and resumes nothing until confirmed", async () => {
    const resumePausedQueues = vi.fn(async (dryRun: boolean) => listing(dryRun));
    render(<ResumePausedSessionsAction resumePausedQueues={resumePausedQueues} />);

    fireEvent.click(screen.getByRole("button", { name: "Resume paused…" }));
    await screen.findByRole("list", { name: "Paused sessions" });

    // The preview is a dry run only.
    expect(resumePausedQueues).toHaveBeenCalledTimes(1);
    expect(resumePausedQueues).toHaveBeenCalledWith(true);
    expect(
      row("session-mail").getByText(
        "turn failed (provider usage limit): weekly limit reached",
      ),
    ).toBeInTheDocument();
    expect(row("session-mail").getByText("Will resume")).toBeInTheDocument();
    expect(row("session-operator").getByText("stop requested")).toBeInTheDocument();
    expect(
      row("session-operator").getByText("Skipped: operator paused"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/1 paused delegation\s+session\(s\) not shown/),
    ).toBeInTheDocument();

    // Confirming resumes only the sessions the preview showed as resumable.
    fireEvent.click(screen.getByRole("button", { name: "Resume 1 session(s)" }));
    await waitFor(() =>
      expect(resumePausedQueues).toHaveBeenLastCalledWith(false, ["session-mail"]),
    );
    expect(await row("session-mail").findByText("Resumed")).toBeInTheDocument();
    expect(
      row("session-operator").getByText("Skipped: operator paused"),
    ).toBeInTheDocument();
  });

  it("says when an accepted Resume left the queue paused", async () => {
    const resumePausedQueues = vi.fn(async (dryRun: boolean) => {
      const response = listing(dryRun);
      if (!dryRun) {
        response.sessions[0] = {
          ...response.sessions[0],
          outcome: "stillPaused",
          queuePauseReason: {
            cause: "human_start_failed",
            pausedAt: "2026-11-23T18:05:00.000Z",
          },
        };
      }
      return response;
    });
    render(<ResumePausedSessionsAction resumePausedQueues={resumePausedQueues} />);

    fireEvent.click(screen.getByRole("button", { name: "Resume paused…" }));
    fireEvent.click(await screen.findByRole("button", { name: "Resume 1 session(s)" }));

    expect(
      await row("session-mail").findByText("Resume accepted; still paused"),
    ).toBeInTheDocument();
    expect(row("session-mail").getByText("human start failed")).toBeInTheDocument();
  });

  it("opens as a modal that takes focus and closes on Escape without resuming", async () => {
    const resumePausedQueues = vi.fn(async (dryRun: boolean) => listing(dryRun));
    render(<ResumePausedSessionsAction resumePausedQueues={resumePausedQueues} />);
    const opener = screen.getByRole("button", { name: "Resume paused…" });

    fireEvent.click(opener);
    const dialog = await screen.findByRole("dialog", { name: "Resume paused sessions" });
    await screen.findByRole("list", { name: "Paused sessions" });
    expect(dialog).toHaveFocus();
    expect(dialog.parentElement).toHaveClass("dialog-backdrop");

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(opener).toHaveFocus();
    expect(resumePausedQueues).toHaveBeenCalledTimes(1);
    expect(resumePausedQueues).toHaveBeenCalledWith(true);
  });

  it("keeps Tab and Shift+Tab on the dialog's own controls", async () => {
    const resumePausedQueues = vi.fn(async (dryRun: boolean) => listing(dryRun));
    render(<ResumePausedSessionsAction resumePausedQueues={resumePausedQueues} />);

    fireEvent.click(screen.getByRole("button", { name: "Resume paused…" }));
    const dialog = await screen.findByRole("dialog", { name: "Resume paused sessions" });
    const resume = await screen.findByRole("button", { name: "Resume 1 session(s)" });
    const cancel = screen.getByRole("button", { name: "Cancel" });

    // Backward from the dialog itself wraps to its last control.
    expect(dialog).toHaveFocus();
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(cancel).toHaveFocus();
    // Forward from the last control wraps to the first.
    fireEvent.keyDown(cancel, { key: "Tab" });
    expect(resume).toHaveFocus();
    // Backward from the first control wraps to the last.
    fireEvent.keyDown(resume, { key: "Tab", shiftKey: true });
    expect(cancel).toHaveFocus();
  });

  it("drops a preview that arrives after the dialog was closed", async () => {
    let answer: (response: ResumePausedQueuesResponse) => void = () => {};
    const resumePausedQueues = vi.fn(
      () =>
        new Promise<ResumePausedQueuesResponse>((resolve) => {
          answer = resolve;
        }),
    );
    render(<ResumePausedSessionsAction resumePausedQueues={resumePausedQueues} />);

    fireEvent.click(screen.getByRole("button", { name: "Resume paused…" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    await act(async () => {
      answer(listing(true));
    });

    expect(resumePausedQueues).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
