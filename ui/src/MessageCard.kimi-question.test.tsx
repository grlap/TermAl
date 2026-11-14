import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import permission from "../../src/tests/fixtures/kimi-question-permission.json";
import { MessageCard } from "./message-cards";
import { reconcileSingleSession } from "./session-reconcile";
import type { Session, UserInputRequestMessage } from "./types";

vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  listMailboxes: vi.fn().mockResolvedValue([]),
}));

function questionMessage(): UserInputRequestMessage {
  return {
    id: "kimi-question", type: "userInputRequest", author: "assistant",
    timestamp: "10:04", title: "Kimi needs your input",
    detail: "Choose an answer or skip this question.",
    state: "pending", declinable: true,
    questions: [{
      id: "question", header: "Question",
      question: permission.params.toolCall.content[0]!.content.text,
      options: permission.params.options.filter((option) => option.kind === "allow_once")
        .map((option) => ({ id: option.optionId, label: option.name, description: "" })),
    }],
  };
}

describe("Kimi permission questions in the shared input card", () => {
  it("renders plain question and answer names and submits the non-first opaque ID", async () => {
    const onSubmit = vi.fn();
    render(<MessageCard message={questionMessage()} onApprovalDecision={vi.fn()} onUserInputSubmit={onSubmit} />);
    expect(screen.getByText("Which color should I use?")).toBeVisible();
    expect(screen.getByRole("button", { name: "Red" })).toBeVisible();
    expect(screen.queryByRole("button", { name: /^Approve/ })).not.toBeInTheDocument();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Blue" })); });
    expect(onSubmit).toHaveBeenCalledWith("kimi-question", { question: ["q0_opt_1"] });
  });

  it("skips without fabricating an answer", async () => {
    const onSubmit = vi.fn();
    render(<MessageCard message={questionMessage()} onApprovalDecision={vi.fn()} onUserInputSubmit={onSubmit} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Skip" })); });
    expect(onSubmit).toHaveBeenCalledWith("kimi-question", null);
  });

  it("distinguishes equal labels, freezes all choices in flight, and respects inert snapshots", async () => {
    const message = questionMessage();
    message.questions[0]!.options![1]!.label = "Red";
    const onSubmit = vi.fn(() => new Promise<void>(() => {}));
    const { rerender } = render(<MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={onSubmit} />);
    const choices = screen.getAllByRole("button", { name: "Red" });
    fireEvent.click(choices[1]!);
    expect(onSubmit).toHaveBeenCalledWith("kimi-question", { question: ["q0_opt_1"] });
    expect(choices[0]).toBeDisabled();
    expect(choices[1]).toBeDisabled();
    expect(screen.getByRole("button", { name: "Skip" })).toBeDisabled();
    rerender(<MessageCard message={{ ...questionMessage(), id: "snapshot" }} userInputActionsEnabled={false} onApprovalDecision={vi.fn()} onUserInputSubmit={onSubmit} />);
    expect(screen.getByRole("button", { name: "Blue" })).toBeDisabled();
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });

  it("shows the submitted answer name instead of its opaque ID", () => {
    const message = questionMessage();
    message.state = "submitted";
    message.submittedAnswers = { question: ["q0_opt_1"] };
    render(<MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.queryByText("q0_opt_1")).not.toBeInTheDocument();
    expect(screen.getAllByText("Blue").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Blue" })).toBeDisabled();
  });

  it("adopts refreshed opaque choice IDs even when the labels are unchanged", async () => {
    const previous: Session = {
      id: "session-kimi", name: "Kimi", emoji: "K", agent: "Kimi",
      workdir: "/fixture", model: "kimi", status: "approval", preview: "",
      messages: [questionMessage()],
    };
    const incoming = structuredClone(previous);
    const refreshed = incoming.messages[0] as UserInputRequestMessage;
    const options = refreshed.questions[0]!.options!;
    options[1] = { ...options[1]!, id: "opaque-refreshed-blue" };
    const adopted = reconcileSingleSession(previous, incoming);
    const onSubmit = vi.fn();
    render(<MessageCard message={adopted.messages[0]!} onApprovalDecision={vi.fn()} onUserInputSubmit={onSubmit} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Blue" })); });
    expect(onSubmit).toHaveBeenCalledWith("kimi-question", { question: ["opaque-refreshed-blue"] });
  });
});
