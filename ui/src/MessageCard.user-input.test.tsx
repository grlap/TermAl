// Owns new coverage for the user-input card's Skip/decline, in-flight guard,
// actions gate, board-snapshot inertness, and resolved-declined behavior.
// Deliberately does not own the
// baseline structured-submission cases ("submits structured user input
// answers", "submits every Claude question including multi-select and
// Other"), which stay in MessageCard.test.tsx with the other card kinds.

import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MessageCard } from "./message-cards";
import type { UserInputRequestMessage } from "./types";

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    listMailboxes: vi.fn().mockResolvedValue([]),
  };
});

describe("MessageCard user input", () => {
  it("skips a declinable user input request without answers", async () => {
    // Claude AskUserQuestion cards that arrived over the permission channel
    // are declinable: Skip submits null answers, which the backend maps to
    // a permission deny telling Claude to decide on its own.
    const onUserInputSubmit = vi.fn();
    const message: UserInputRequestMessage = {
      id: "message-claude-declinable",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Skip" }));
      await Promise.resolve();
    });

    expect(onUserInputSubmit).toHaveBeenCalledWith(
      "message-claude-declinable",
      null,
    );
  });

  it("keeps a literal __other__ option distinct from the free-form Other choice", async () => {
    const onUserInputSubmit = vi.fn();
    const message: UserInputRequestMessage = {
      id: "message-literal-other",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Token",
          id: "claude-question-literal-other",
          isOther: true,
          question: "Which token should I use?",
          options: [
            {
              label: "__other__",
              description: "Use the literal protocol token.",
            },
          ],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: /__other__/ }));
    expect(screen.queryByRole("textbox")).toBeNull();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Submit answers" }));
      await Promise.resolve();
    });

    expect(onUserInputSubmit).toHaveBeenCalledWith(
      "message-literal-other",
      { "claude-question-literal-other": ["__other__"] },
    );
  });

  it("disables Submit and Skip while a submission is in flight", async () => {
    // A double-click must not dispatch a second request (and a second
    // error toast): after the first dispatch both actions disable until
    // the submission settles or the message updates.
    let resolveSubmission: (() => void) | undefined;
    const onUserInputSubmit = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSubmission = resolve;
        }),
    );
    const message: UserInputRequestMessage = {
      id: "message-inflight",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: /Focused/ }));
    const submitButton = screen.getByRole("button", {
      name: "Submit answers",
    });
    const skipButton = screen.getByRole("button", { name: "Skip" });
    fireEvent.click(submitButton);

    expect(submitButton).toBeDisabled();
    expect(skipButton).toBeDisabled();
    // The whole card freezes, not only the actions: editing the draft
    // mid-flight would desync what the user sees from what was sent.
    const focusedRadio = screen.getByRole("radio", { name: /Focused/ });
    const otherRadio = screen.getByRole("radio", { name: "Other" });
    expect(focusedRadio).toBeDisabled();
    expect(otherRadio).toBeDisabled();
    fireEvent.click(submitButton);
    fireEvent.click(skipButton);
    expect(onUserInputSubmit).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolveSubmission?.();
      await Promise.resolve();
    });
    // The guard lifts once the submission settles — for the whole card.
    expect(submitButton).not.toBeDisabled();
    expect(skipButton).not.toBeDisabled();
    expect(focusedRadio).not.toBeDisabled();
    expect(otherRadio).not.toBeDisabled();
  });

  it("re-enables the actions after a rejected submission without an unhandled rejection", async () => {
    let rejectSubmission: ((reason: Error) => void) | undefined;
    const onUserInputSubmit = vi.fn(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectSubmission = reject;
        }),
    );
    const message: UserInputRequestMessage = {
      id: "message-rejected",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    const skipButton = screen.getByRole("button", { name: "Skip" });
    const focusedRadio = screen.getByRole("radio", { name: /Focused/ });
    fireEvent.click(skipButton);
    expect(skipButton).toBeDisabled();
    expect(focusedRadio).toBeDisabled();

    await act(async () => {
      rejectSubmission?.(new Error("submission failed"));
      await Promise.resolve();
      await Promise.resolve();
    });
    // Error reporting happened in the app handler; the card only lets the
    // user retry — the whole card thaws, not only the actions. A rejection
    // must not surface as an unhandled event.
    expect(skipButton).not.toBeDisabled();
    expect(focusedRadio).not.toBeDisabled();
    expect(onUserInputSubmit).toHaveBeenCalledTimes(1);
  });

  it("re-enables the actions after a synchronous handler throw", () => {
    const onUserInputSubmit = vi.fn(() => {
      throw new Error("synchronous handler failure");
    });
    const message: UserInputRequestMessage = {
      id: "message-sync-throw",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={
          // Deliberately violate the promise-returning prop contract so this
          // component boundary is exercised against a synchronous throw.
          onUserInputSubmit as unknown as (
            messageId: string,
            answers: Record<string, string[]> | null,
          ) => Promise<void>
        }
      />,
    );

    const skipButton = screen.getByRole("button", { name: "Skip" });
    fireEvent.click(skipButton);
    // The throw is contained and the guard lifts immediately for a retry.
    expect(skipButton).not.toBeDisabled();
    expect(onUserInputSubmit).toHaveBeenCalledTimes(1);
  });

  it("ignores a submission that settles after the card unmounts", async () => {
    let resolveSubmission: (() => void) | undefined;
    const onUserInputSubmit = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSubmission = resolve;
        }),
    );
    const message: UserInputRequestMessage = {
      id: "message-unmounted",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    const { unmount } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    unmount();
    // Settling after unmount must neither warn about state updates nor
    // raise an unhandled rejection.
    await act(async () => {
      resolveSubmission?.();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(onUserInputSubmit).toHaveBeenCalledTimes(1);
  });

  it("renders a resolved declined card without live controls", () => {
    const message: UserInputRequestMessage = {
      id: "message-declined",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "The user skipped these questions; Claude was asked to decide on its own.",
      state: "declined",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
      />,
    );

    expect(
      screen.getByText(
        "The user skipped these questions; Claude was asked to decide on its own.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText(/Status: Skipped by you/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Submit answers" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
  });

  it("labels an unattended self-resolution separately from a user Skip", () => {
    const message: UserInputRequestMessage = {
      id: "message-self-resolved",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude asked a question",
      detail: "TermAl asked Claude to decide without human input.",
      state: "declined",
      declinable: false,
      questions: [],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
      />,
    );

    expect(
      screen.getByText(/Status: Resolved by TermAl without human input/),
    ).toBeInTheDocument();
  });

  it("renders disabled user-input actions when the actions gate is off", () => {
    // Board snapshots and staging trays must not show live-looking
    // Submit/Skip controls wired to no-op handlers.
    const onUserInputSubmit = vi.fn();
    const message: UserInputRequestMessage = {
      id: "message-gated",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [{ label: "Focused", description: "Only this module." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
        userInputActionsEnabled={false}
      />,
    );

    const submitButton = screen.getByRole("button", {
      name: "Submit answers",
    });
    const skipButton = screen.getByRole("button", { name: "Skip" });
    expect(submitButton).toBeDisabled();
    expect(skipButton).toBeDisabled();
    fireEvent.click(submitButton);
    fireEvent.click(skipButton);
    expect(onUserInputSubmit).not.toHaveBeenCalled();
    expect(
      screen.getByText("Answer these questions from the live conversation."),
    ).toBeInTheDocument();
  });

  it("keeps a board snapshot of a live card fully inert", async () => {
    // A live card and a board snapshot share message and question ids. The
    // snapshot must disable every control (not only Submit/Skip) and must
    // not share a native radio group with the live card, so clicking it
    // can neither change nor desync the live selection.
    const onUserInputSubmit = vi.fn(async () => {});
    const message: UserInputRequestMessage = {
      id: "message-shared",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [
        {
          header: "Scope",
          id: "claude-question-1",
          isOther: true,
          question: "Which scope should I use?",
          options: [
            { label: "Focused", description: "Only this module." },
            { label: "Broad", description: "The whole workspace." },
          ],
        },
      ],
    };

    render(
      <>
        <section data-testid="live">
          <MessageCard
            message={message}
            onApprovalDecision={vi.fn()}
            onUserInputSubmit={onUserInputSubmit}
          />
        </section>
        <section data-testid="snapshot">
          <MessageCard
            message={message}
            onApprovalDecision={vi.fn()}
            onUserInputSubmit={async () => {}}
            userInputActionsEnabled={false}
          />
        </section>
      </>,
    );

    const live = within(screen.getByTestId("live"));
    const snapshot = within(screen.getByTestId("snapshot"));
    const liveFocused = live.getByRole("radio", { name: /Focused/ });
    const liveBroad = live.getByRole("radio", { name: /Broad/ });
    const snapshotBroad = snapshot.getByRole("radio", { name: /Broad/ });

    // Every snapshot control is disabled, including options and Other.
    for (const control of snapshot.getAllByRole("radio")) {
      expect(control).toBeDisabled();
    }
    expect(snapshot.getByRole("button", { name: "Submit answers" })).toHaveProperty(
      "disabled",
      true,
    );

    // Distinct native radio groups: the live and snapshot inputs never
    // share a `name`.
    expect((liveBroad as HTMLInputElement).name).not.toBe(
      (snapshotBroad as HTMLInputElement).name,
    );

    fireEvent.click(liveFocused);
    expect(liveFocused).toHaveProperty("checked", true);
    fireEvent.click(snapshotBroad);
    // The snapshot click is inert for the live card: its selection is
    // untouched. (jsdom still flips the disabled DOM node's own `checked`
    // on a synthetic click, so the live inputs are the meaningful probe.)
    expect(liveFocused).toHaveProperty("checked", true);
    expect(liveBroad).toHaveProperty("checked", false);

    // The submit settles asynchronously (the in-flight guard lifts on
    // settle), so flush it inside act to keep the test act-clean.
    await act(async () => {
      fireEvent.click(live.getByRole("button", { name: "Submit answers" }));
      await Promise.resolve();
    });
    expect(onUserInputSubmit).toHaveBeenCalledWith("message-shared", {
      "claude-question-1": ["Focused"],
    });
  });

  it("keeps Submit and Skip disabled across a mid-flight message update", async () => {
    // Ordinary updates to the same message (a re-synced snapshot with a
    // different detail, say) must not lift the in-flight guard; only
    // settlement — or a genuinely different message — does.
    let resolveSubmission: (() => void) | undefined;
    const onUserInputSubmit = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSubmission = resolve;
        }),
    );
    const question = {
      header: "Scope",
      id: "claude-question-1",
      isOther: true,
      question: "Which scope should I use?",
      options: [{ label: "Focused", description: "Only this module." }],
    };
    const message: UserInputRequestMessage = {
      id: "message-midflight",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: true,
      questions: [question],
    };

    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    const focusedOption = screen.getByRole("radio", { name: /Focused/ });
    fireEvent.click(focusedOption);
    fireEvent.click(screen.getByRole("button", { name: "Submit answers" }));
    expect(screen.getByRole("button", { name: "Skip" })).toHaveProperty(
      "disabled",
      true,
    );
    expect(onUserInputSubmit).toHaveBeenCalledWith("message-midflight", {
      "claude-question-1": ["Focused"],
    });

    rerender(
      <MessageCard
        message={{
          ...message,
          detail: "Answer Claude's question to continue. (re-synced)",
          questions: [{ ...question }],
        }}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );
    expect(screen.getByRole("button", { name: "Skip" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Submit answers" }),
    ).toBeDisabled();
    expect(screen.getByRole("radio", { name: /Focused/ })).toHaveProperty(
      "checked",
      true,
    );
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    expect(onUserInputSubmit).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolveSubmission?.();
      await Promise.resolve();
    });
    expect(screen.getByRole("button", { name: "Skip" })).not.toBeDisabled();
    expect(screen.getByRole("radio", { name: /Focused/ })).toHaveProperty(
      "checked",
      true,
    );
  });

  it("offers no skip action on cards that cannot be declined", () => {
    const message: UserInputRequestMessage = {
      declinable: false,
      id: "message-codex-input",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Codex needs input",
      detail: "Codex requested additional input.",
      state: "pending",
      questions: [
        {
          header: "Environment",
          id: "environment",
          question: "Which environment should I use?",
          options: [{ label: "Staging", description: "Use staging." }],
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
    expect(
      screen.getByRole("button", { name: "Submit answers" }),
    ).toBeInTheDocument();
  });
});
